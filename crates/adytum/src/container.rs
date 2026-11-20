//! Container cells through a docker-compatible command: `nerdctl` as root
//! inside a Lima cell, or `docker` on this machine. Every container runs
//! under gVisor unless its options say runc, as an unprivileged user with no
//! capabilities and no way to gain any, and a sealed one has no network. Copies go through `tar` and
//! `dd` in the container, as the copy's owner, never through `cp`, which
//! nerdctl documents as unsafe with an untrusted container. The image needs
//! `sleep`, and for copies `tar`, `dd`, `cat` and `test`, as busybox and
//! every common base image have them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::copy::{self, Channel};
use crate::exec::{tail, Command, Duplex, Output, Runner, SystemRunner};
use crate::{check_command, check_guest_path, CellId, CellName, CellSpec, CellState, Driver, Error, ExecOpts, GuestUser, Kind, Listed, Refusal, RefusalCode, Runtime};

/// The user a container's commands run as. It has no name in most images,
/// and needs none.
pub const CELL_USER: &str = "1000:1000";
const ROOT: &str = "0:0";
pub const WALL_LABEL: &str = "adytum.wall";
/// The ledger's id for the cell: what proves a container is the one made.
pub const CELL_LABEL: &str = "adytum.cell";

const PULL_DEADLINE: Duration = Duration::from_secs(30 * 60);
const COPY_DEADLINE: Duration = Duration::from_secs(10 * 60);
const QUICK_DEADLINE: Duration = Duration::from_secs(2 * 60);

pub struct Oci {
    runner: Arc<dyn Runner>,
    program: PathBuf,
    pass: &'static [&'static str],
    gvisor: &'static str,
}

impl Oci {
    /// `program` takes docker's arguments; nothing of this process's
    /// environment is passed to it.
    pub fn new(runner: Arc<dyn Runner>, program: impl Into<PathBuf>) -> Oci {
        Oci { runner, program: program.into(), pass: &[], gvisor: "io.containerd.runsc.v1" }
    }

    /// `docker` on this machine, with the variables it finds its daemon by.
    /// gVisor is the runtime docker knows as `runsc`.
    pub fn docker() -> Oci {
        Oci { pass: &["PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG"], gvisor: "runsc", ..Oci::new(Arc::new(SystemRunner::new()), "docker") }
    }

    /// `nerdctl` through `runner`, such as a Lima cell's
    /// [`Host::runner`](crate::Host::runner).
    pub fn nerdctl(runner: Arc<dyn Runner>) -> Oci {
        Oci::new(runner, "nerdctl")
    }

    fn cli(&self) -> Command {
        Command::new(&self.program).pass_env(self.pass)
    }

    async fn run_ok(&self, cmd: Command, what: &str) -> Result<Output, Error> {
        let out = self.runner.run(&cmd).await?;
        if out.success() {
            return Ok(out);
        }
        Err(failure(what, &out))
    }

    async fn require(&self, name: &CellName) -> Result<CellState, Error> {
        self.list().await?.into_iter().find(|c| &c.name == name).map(|c| c.state).ok_or_else(|| Error::NotFound { name: name.to_string() })
    }

    fn exec_command(&self, name: &CellName, argv: &[String], opts: &ExecOpts, interactive: bool) -> Result<Command, Error> {
        check_command(argv, &opts.env)?;
        let mut cmd = self.cli().arg("exec");
        if interactive {
            cmd = cmd.arg("-i");
        }
        cmd = cmd.args(["--user", if opts.root { ROOT } else { CELL_USER }]);
        for (key, value) in &opts.env {
            cmd = cmd.arg("-e").arg(format!("{key}={value}"));
        }
        Ok(cmd.arg(name.as_str()).args(argv))
    }

    fn channel_user(owner: Option<&GuestUser>) -> &str {
        owner.map_or(CELL_USER, GuestUser::as_str)
    }
}

/// Never `NotFound`: what a container prints can look like the runtime's
/// own words. Whether a container exists is read from `ps` alone.
fn failure(what: &str, out: &Output) -> Error {
    let stderr = if out.timed_out { "It did not finish before its deadline and was stopped.".to_owned() } else { tail(&out.stderr_lossy()) };
    Error::Tool { what: what.to_owned(), status: out.status, stderr }
}

/// docker and nerdctl both refuse a name in use; the name is someone
/// else's then, and nothing is to be cleaned up.
fn name_in_use(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("already in use") || s.contains("is already used") || s.contains("conflict")
}

/// nerdctl ends a failed command's stderr with a line of its own, which
/// holds the command's exit code; nerdctl itself exits 1.
fn without_exec_note(mut out: Output) -> Output {
    let text = out.stderr_lossy();
    let body = text.trim_end_matches('\n');
    let (rest, last) = body.rsplit_once('\n').unwrap_or(("", body));
    if let Some((_, code)) = last.split_once("msg=\"exec failed with exit code ") {
        if let Ok(code) = code.trim_end_matches('"').trim().parse::<i32>() {
            out.status = Some(code);
        }
        out.stderr = if rest.is_empty() { Vec::new() } else { format!("{rest}\n").into_bytes() };
    }
    out
}

/// docker says `State`; nerdctl says only `Status`, such as `Up` or
/// `Exited (0) 3 seconds ago`.
fn state_of(entry: &serde_json::Value) -> CellState {
    let text = |key: &str| entry.get(key).and_then(serde_json::Value::as_str).unwrap_or_default().to_ascii_lowercase();
    let (state, status) = (text("State"), text("Status"));
    match state.as_str() {
        "running" => CellState::Running,
        "created" | "exited" => CellState::Stopped,
        "" if status.starts_with("up") => CellState::Running,
        "" if status.starts_with("created") || status.starts_with("exited") => CellState::Stopped,
        _ => CellState::Broken,
    }
}

/// One JSON object per line, as `ps --format '{{json .}}'` prints them.
/// Only containers Adytum made: named `adytum-*` and carrying the
/// `adytum.cell` label with the id they were made with.
pub fn parse_ps(text: &str) -> Result<Vec<Listed>, Error> {
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let entry: serde_json::Value =
            serde_json::from_str(line).map_err(|e| Error::Tool { what: "ps".to_owned(), status: Some(0), stderr: format!("its output was not JSON: {e}") })?;
        let labels = entry.get("Labels").and_then(serde_json::Value::as_str).unwrap_or_default();
        let Some(id) = labels.split(',').find_map(|l| l.strip_prefix(&format!("{CELL_LABEL}="))?.parse::<CellId>().ok()) else { continue };
        let names = entry.get("Names").and_then(serde_json::Value::as_str).unwrap_or_default();
        for name in names.split(',').filter_map(|n| n.trim().trim_start_matches('/').parse::<CellName>().ok()) {
            out.push(Listed { name, state: state_of(&entry), id: Some(id) });
        }
    }
    Ok(out)
}

#[async_trait]
impl Driver for Oci {
    async fn create(&self, spec: &CellSpec, id: &CellId) -> Result<(), Error> {
        crate::policy::check_spec(spec)?;
        let Kind::Container(options) = &spec.kind else { return Err(Error::Unsupported { what: "a cell of another kind".to_owned() }) };
        let refuse = |what: &str| Err(Error::Unsupported { what: what.to_owned() });
        if !spec.mounts.is_empty() {
            return refuse("host folders in a container cell (copy them in instead)");
        }
        if spec.network.is_some() {
            return refuse("a named network for a container cell");
        }
        if spec.clone_from.is_some() {
            return refuse("cloning a container cell");
        }
        // Nested cells publish nothing and a container here is sealed, so no
        // container has a network to publish from.
        if !spec.ports.is_empty() {
            return refuse("published ports on a container cell");
        }
        let image = options.image.as_str();
        self.run_ok(self.cli().args(["pull", "--quiet", image]).deadline(PULL_DEADLINE), "pull").await?;
        let mut cmd = self
            .cli()
            .args(["create", "--name", spec.name.as_str()])
            .arg("--label")
            .arg(format!("{WALL_LABEL}={}", spec.wall))
            .arg("--label")
            .arg(format!("{CELL_LABEL}={id}"))
            .args(["--init", "--user", CELL_USER, "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--pids-limit", "4096"])
            // Its main process only sleeps; and gVisor's shim cannot write nerdctl's own log.
            .args(["--log-driver", "none"])
            .arg("--cpus")
            .arg(spec.resources.cpus.to_string())
            .arg("--memory")
            .arg(format!("{}m", spec.resources.memory_mib));
        if options.runtime == Runtime::Gvisor {
            cmd = cmd.args(["--runtime", self.gvisor]);
        }
        if spec.sealed {
            cmd = cmd.args(["--network", "none"]);
        }
        let cmd = cmd.arg(image).args(["sleep", "infinity"]).deadline(QUICK_DEADLINE);
        let out = self.runner.run(&cmd).await?;
        if out.success() {
            return Ok(());
        }
        if name_in_use(&out.stderr_lossy()) {
            return Err(Refusal::new(
                RefusalCode::NameTaken,
                format!("Something else took the name {} while its image was pulled.", spec.name),
                "Pick another name; Adytum leaves that container alone.",
            )
            .into());
        }
        Err(failure("create", &out))
    }

    async fn start(&self, name: &CellName) -> Result<(), Error> {
        self.require(name).await?;
        self.run_ok(self.cli().args(["start", name.as_str()]).deadline(QUICK_DEADLINE), "start").await.map(drop)
    }

    async fn exec(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Output, Error> {
        let mut cmd = self.exec_command(name, argv, &opts, opts.stdin.is_some())?;
        if let Some(bytes) = opts.stdin {
            cmd = cmd.stdin_bytes(bytes);
        }
        if let Some(deadline) = opts.deadline {
            cmd = cmd.deadline(deadline);
        }
        Ok(without_exec_note(self.runner.run(&cmd).await?))
    }

    async fn stream(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Duplex, Error> {
        if opts.stdin.is_some() {
            return Err(Error::Unsupported { what: "stdin bytes for a stream (write them to the stream)".to_owned() });
        }
        let mut cmd = self.exec_command(name, argv, &opts, true)?;
        if let Some(deadline) = opts.deadline {
            cmd = cmd.deadline(deadline);
        }
        Ok(self.runner.duplex(&cmd).await?.map_end(without_exec_note))
    }

    async fn copy_in(&self, name: &CellName, host: &Path, guest: &str, owner: Option<&GuestUser>) -> Result<(), Error> {
        check_guest_path(guest)?;
        let user = Oci::channel_user(owner);
        let as_owner = |argv: Vec<String>| self.cli().args(["exec", "-i", "--user", user, name.as_str()]).args(argv);
        let fail = |what: &str, out: &Output| failure(what, &without_exec_note(out.clone()));
        let local = SystemRunner::new();
        let channel = Channel { local: &local, remote: &*self.runner, as_owner: &as_owner, fail: &fail, quick: QUICK_DEADLINE, deadline: COPY_DEADLINE };
        copy::send(&channel, host, guest).await
    }

    async fn copy_out(&self, name: &CellName, guest: &str, host: &Path) -> Result<(), Error> {
        check_guest_path(guest)?;
        let as_owner = |argv: Vec<String>| self.cli().args(["exec", "-i", "--user", CELL_USER, name.as_str()]).args(argv);
        let fail = |what: &str, out: &Output| failure(what, &without_exec_note(out.clone()));
        let local = SystemRunner::new();
        let channel = Channel { local: &local, remote: &*self.runner, as_owner: &as_owner, fail: &fail, quick: QUICK_DEADLINE, deadline: COPY_DEADLINE };
        copy::fetch(&channel, guest, host).await
    }

    async fn stop(&self, name: &CellName) -> Result<(), Error> {
        self.require(name).await?;
        self.run_ok(self.cli().args(["stop", name.as_str()]).deadline(QUICK_DEADLINE), "stop").await.map(drop)
    }

    /// Only the container that carries `id`: one that took the name since
    /// is someone else's, and is left alone.
    async fn remove(&self, name: &CellName, id: &CellId) -> Result<(), Error> {
        let ours = self.list().await?.into_iter().any(|l| &l.name == name && l.id.as_ref() == Some(id));
        if !ours {
            return Err(Error::NotFound { name: name.to_string() });
        }
        self.run_ok(self.cli().args(["rm", "--force", name.as_str()]).deadline(QUICK_DEADLINE), "rm").await.map(drop)
    }

    async fn list(&self) -> Result<Vec<Listed>, Error> {
        let out = self.run_ok(self.cli().args(["ps", "--all", "--format", "{{json .}}"]).deadline(QUICK_DEADLINE), "ps").await?;
        parse_ps(&out.stdout_lossy())
    }
}
