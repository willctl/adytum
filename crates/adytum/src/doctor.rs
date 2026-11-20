use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::exec::{Command, Runner, SystemRunner};
use crate::lima::{lima_home_in, LONGEST_SOCKET, SOCKET_BUDGET};
use crate::Error;

/// The Lima versions the live tests have passed against.
pub const KNOWN_LIMA_VERSIONS: &[&str] = &["2.1.2"];

/// `adytum-` and 47 more characters.
const LONGEST_CELL_NAME: usize = 54;
const SHORTEST_USEFUL_NAME: usize = 16;
const FREE_DISK_FAIL: u64 = 20 * 1024 * 1024 * 1024;

/// What [`checks`] reads; [`gather`] reads it from this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// What `limactl --version` says, such as `2.1.2`.
    pub limactl: Option<String>,
    pub home: Option<PathBuf>,
    pub lima_home_exists: bool,
    /// Whether Lima has already downloaded the default image.
    pub image_cached: bool,
    /// Bytes free on the disk that holds Adytum's folder.
    pub free_bytes: Option<u64>,
    pub ledger: LedgerStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerStatus {
    Missing,
    Open { unsettled: usize },
    SetUpAgain(String),
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Something will go wrong or work worse.
    Warn {
        fix: String,
    },
    /// Adytum cannot do its job until this is fixed.
    Fail {
        fix: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub id: &'static str,
    pub message: String,
    pub verdict: Verdict,
}

impl Finding {
    fn ok(id: &'static str, message: impl Into<String>) -> Finding {
        Finding { id, message: message.into(), verdict: Verdict::Ok }
    }

    fn warn(id: &'static str, message: impl Into<String>, fix: impl Into<String>) -> Finding {
        Finding { id, message: message.into(), verdict: Verdict::Warn { fix: fix.into() } }
    }

    fn fail(id: &'static str, message: impl Into<String>, fix: impl Into<String>) -> Finding {
        Finding { id, message: message.into(), verdict: Verdict::Fail { fix: fix.into() } }
    }

    pub fn fix(&self) -> Option<&str> {
        match &self.verdict {
            Verdict::Ok => None,
            Verdict::Warn { fix } | Verdict::Fail { fix } => Some(fix),
        }
    }
}

/// As `{"id", "ok", "level", "message", "fix"}`, the shape `adytum doctor
/// --json` prints.
impl Serialize for Finding {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let level = match self.verdict {
            Verdict::Ok => "info",
            Verdict::Warn { .. } => "warn",
            Verdict::Fail { .. } => "fail",
        };
        let mut out = s.serialize_struct("Finding", 5)?;
        out.serialize_field("id", self.id)?;
        out.serialize_field("ok", &(self.verdict == Verdict::Ok))?;
        out.serialize_field("level", level)?;
        out.serialize_field("message", &self.message)?;
        out.serialize_field("fix", &self.fix())?;
        out.end()
    }
}

/// Reads this machine: `limactl`, Adytum's folder, Lima's download cache,
/// the free disk and the ledger. Changes nothing.
pub async fn gather() -> Machine {
    let runner = SystemRunner::new();
    let ask = |program: &'static str, args: Vec<String>| {
        let runner = &runner;
        async move {
            let cmd = Command::new(program).args(args).pass_env(&["PATH", "HOME"]).deadline(Duration::from_secs(15));
            let out = runner.run(&cmd).await.ok()?;
            out.success().then(|| out.stdout_lossy())
        }
    };
    let limactl = ask("limactl", vec!["--version".to_owned()]).await.and_then(|text| version(&text));
    let home = crate::home();
    let free_bytes = match &home {
        Some(home) => ask("df", vec!["-Pk".to_owned(), nearest_existing(home).to_string_lossy().into_owned()]).await.and_then(|text| parse_df(&text)),
        None => None,
    };
    let image_cached = crate::lima::image(crate::lima::DEFAULT_IMAGE)
        .zip(lima_cache_dir())
        .is_some_and(|(pin, dir)| dir.join("download/by-url-sha256").join(sha256_hex(&pin.location)).join("data").is_file());
    let ledger = match home.as_deref().and_then(crate::cells::unsettled) {
        None => LedgerStatus::Missing,
        Some(Ok(unsettled)) => LedgerStatus::Open { unsettled },
        Some(Err(Error::SetUpAgain(why))) => LedgerStatus::SetUpAgain(why),
        Some(Err(e)) => LedgerStatus::Unreadable(e.to_string()),
    };
    Machine { limactl, lima_home_exists: home.as_deref().is_some_and(|h| lima_home_in(h).exists()), home, image_cached, free_bytes, ledger }
}

pub fn checks(machine: &Machine) -> Vec<Finding> {
    let mut out = vec![limactl(machine.limactl.as_deref())];
    let Some(home) = &machine.home else {
        out.push(Finding::fail(
            "adytum-home",
            "Adytum's folder cannot be found: neither ADYTUM_HOME nor HOME is set.",
            "Set ADYTUM_HOME to a folder Adytum may keep its cells in, such as ~/.adytum.",
        ));
        return out;
    };
    let lima = lima_home_in(home);
    out.push(if machine.lima_home_exists {
        Finding::ok("lima-home", format!("Adytum's Lima folder exists at {}.", lima.display()))
    } else {
        Finding::ok("lima-home", format!("Adytum's Lima folder {} does not exist yet; the first `adytum create` makes it.", lima.display()))
    });
    out.push(socket_path(&lima));
    out.push(if machine.image_cached {
        Finding::ok("image-cache", format!("The pinned image {} is in Lima's download cache.", crate::lima::DEFAULT_IMAGE))
    } else {
        Finding::ok("image-cache", format!("The pinned image {} is not in Lima's download cache yet, so the first create downloads it.", crate::lima::DEFAULT_IMAGE))
    });
    out.push(disk(machine.free_bytes));
    out.extend(ledger(&machine.ledger));
    out
}

fn limactl(version: Option<&str>) -> Finding {
    let known = KNOWN_LIMA_VERSIONS.join(", ");
    match version {
        None => Finding::fail("limactl", "limactl was not found, so Adytum cannot make VM cells.", "Install Lima (https://lima-vm.io) so that `limactl` is on PATH."),
        Some(v) if KNOWN_LIMA_VERSIONS.contains(&v) => Finding::ok("limactl", format!("limactl {v}, a version the live tests pass on.")),
        Some(v) => Finding::warn(
            "limactl",
            format!("limactl {v} is not a version the live tests have run on (they know {known}); its answers may differ."),
            format!("Install Lima {known}, or run the live tests on this version first."),
        ),
    }
}

/// How long a cell name can be is what the socket budget leaves after the
/// Lima folder, the two slashes, the socket's name and its NUL.
fn socket_path(lima: &Path) -> Finding {
    let used = lima.as_os_str().len() + 2 + LONGEST_SOCKET.len() + 1;
    let room = SOCKET_BUDGET.saturating_sub(used);
    if room >= LONGEST_CELL_NAME {
        Finding::ok("socket-path", format!("Lima's longest socket path fits for any cell name ({} of {SOCKET_BUDGET} bytes at most).", used + LONGEST_CELL_NAME))
    } else if room >= SHORTEST_USEFUL_NAME {
        Finding::warn(
            "socket-path",
            format!("Adytum's folder is long: a cell name over {room} characters would make a socket path Lima cannot use, and Adytum refuses it."),
            "Keep cell names short, or set ADYTUM_HOME to a shorter folder such as ~/.adytum.",
        )
    } else {
        Finding::fail(
            "socket-path",
            format!("Adytum's folder {} is too long: Lima's sockets would not fit in it (room for a {room}-character name).", lima.display()),
            "Set ADYTUM_HOME to a shorter folder, such as ~/.adytum.",
        )
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn disk(free: Option<u64>) -> Finding {
    match free {
        None => {
            Finding::warn("disk", "Could not read how much disk is free where Adytum keeps its cells.", "Run `df -h` on Adytum's folder and make sure at least 20 GiB is free.")
        }
        Some(free) if free < FREE_DISK_FAIL => Finding::fail(
            "disk",
            format!("Only {} is free where Adytum keeps its cells; a VM needs at least {}.", gib(free), gib(FREE_DISK_FAIL)),
            "Free disk space, remove unused cells with `adytum rm`, or move ADYTUM_HOME to a bigger disk.",
        ),
        Some(free) => Finding::ok("disk", format!("{} is free where Adytum keeps its cells.", gib(free))),
    }
}

fn ledger(status: &LedgerStatus) -> Vec<Finding> {
    match status {
        LedgerStatus::Missing => vec![Finding::ok("ledger", "There is no ledger yet; the first command makes it.")],
        LedgerStatus::Open { unsettled: 0 } => vec![Finding::ok("ledger", "The ledger opens."), Finding::ok("ledger-settled", "Every operation in the ledger is settled.")],
        LedgerStatus::Open { unsettled } => vec![
            Finding::ok("ledger", "The ledger opens."),
            Finding::warn(
                "ledger-settled",
                format!("{unsettled} operations started and never settled, probably because Adytum was stopped part-way."),
                "Run any adytum command to finish them.",
            ),
        ],
        LedgerStatus::SetUpAgain(why) => vec![Finding::fail(
            "ledger",
            format!("The ledger cannot be used: {why}"),
            "Set Adytum up again: move adytum.sqlite out of Adytum's folder, remove the cells it made (`limactl list`, names starting adytum-), then run any adytum command.",
        )],
        LedgerStatus::Unreadable(why) => {
            vec![Finding::fail("ledger", format!("The ledger could not be opened: {why}"), "Check that Adytum's folder can be read and written and the disk is not full.")]
        }
    }
}

pub fn has_failure(findings: &[Finding]) -> bool {
    findings.iter().any(|f| matches!(f.verdict, Verdict::Fail { .. }))
}

/// One line per finding, and a `fix:` line under each that is not ok.
pub fn render_human(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings {
        let mark = match f.verdict {
            Verdict::Ok => "● ok  ",
            Verdict::Warn { .. } => "▲ warn",
            Verdict::Fail { .. } => "✕ fail",
        };
        let _ = writeln!(out, "{mark} {}: {}", f.id, f.message);
        if let Some(fix) = f.fix() {
            let _ = writeln!(out, "       fix: {fix}");
        }
    }
    out
}

/// `{"ok": bool, "checks": [...]}`, where `ok` means nothing failed.
pub fn render_json(findings: &[Finding]) -> String {
    let value = serde_json::json!({ "ok": !has_failure(findings), "checks": findings });
    format!("{value:#}\n")
}

fn version(text: &str) -> Option<String> {
    text.lines().next()?.split_whitespace().last().map(|w| w.trim_start_matches('v').to_owned())
}

/// The column before the first one ending in `%` is "Available" in
/// 1024-byte blocks; a file system's name may hold spaces.
fn parse_df(text: &str) -> Option<u64> {
    let tokens: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    let at = tokens.iter().position(|t| t.ends_with('%'))?;
    let blocks: u64 = tokens.get(at.checked_sub(1)?)?.parse().ok()?;
    blocks.checked_mul(1024)
}

fn nearest_existing(path: &Path) -> &Path {
    path.ancestors().find(|p| p.exists()).unwrap_or(path)
}

fn lima_cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Caches/lima"));
    }
    let base = std::env::var_os("XDG_CACHE_HOME").filter(|h| !h.is_empty()).map(PathBuf::from).or_else(|| home.map(|h| h.join(".cache")));
    base.map(|b| b.join("lima"))
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

#[cfg(test)]
mod tests;
