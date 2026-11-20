//! Container cells through a docker-compatible command: `nerdctl` as root
//! inside a Lima cell, or `docker` on this machine. Every container runs
//! as an unprivileged user with no capabilities and no way to gain any,
//! and a sealed one has no network at all. Copies go through `tar` and
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
use crate::{check_command, check_guest_path, CellName, CellSpec, CellState, Driver, Error, ExecOpts, GuestUser, Kind, Listed, Port};

/// The user a container's commands run as. It has no name in most images,
/// and needs none.
pub const CELL_USER: &str = "1000:1000";
const ROOT: &str = "0:0";
pub const WALL_LABEL: &str = "adytum.wall";

const PULL_DEADLINE: Duration = Duration::from_secs(30 * 60);
const COPY_DEADLINE: Duration = Duration::from_secs(10 * 60);
const QUICK_DEADLINE: Duration = Duration::from_secs(2 * 60);

pub struct Oci {
    runner: Arc<dyn Runner>,
    program: PathBuf,
    pass: &'static [&'static str],
}

impl Oci {
    /// `program` takes docker's arguments; nothing of this process's
    /// environment is passed to it.
    pub fn new(runner: Arc<dyn Runner>, program: impl Into<PathBuf>) -> Oci {
        Oci { runner, program: program.into(), pass: &[] }
    }

    /// `docker` on this machine, with the variables it finds its daemon by.
    pub fn docker() -> Oci {
        Oci { pass: &["PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG"], ..Oci::new(Arc::new(SystemRunner::new()), "docker") }
    }

    /// `nerdctl` through `runner`, such as a Lima cell's
    /// [`Host::runner`](crate::Host::runner).
    pub fn nerdctl(runner: Arc<dyn Runner>) -> Oci {
        Oci::new(runner, "nerdctl")
    }

    fn cli(&self) -> Command {
        Command::new(&self.program).pass_env(self.pass)
    }

    async fn run_ok(&self, cmd: Command, what: &str, name: Option<&CellName>) -> Result<Output, Error> {
        let out = self.runner.run(&cmd).await?;
        if out.success() {
            return Ok(out);
        }
        Err(failure(what, name, &out))
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

fn failure(what: &str, name: Option<&CellName>, out: &Output) -> Error {
    let stderr = out.stderr_lossy();
    match name {
        Some(name) if stderr.to_ascii_lowercase().contains("no such container") => Error::NotFound { name: name.to_string() },
        _ => {
            let stderr = if out.timed_out { "It did not finish before its deadline and was stopped.".to_owned() } else { tail(&stderr) };
            Error::Tool { what: what.to_owned(), status: out.status, stderr }
        }
    }
}

/// nerdctl ends a failed command's stderr with a line of its own.
fn without_exec_note(mut out: Output) -> Output {
    let text = out.stderr_lossy();
    let body = text.trim_end_matches('\n');
    let (rest, last) = body.rsplit_once('\n').unwrap_or(("", body));
    if last.contains("msg=\"exec failed with exit code") {
        out.stderr = if rest.is_empty() { Vec::new() } else { format!("{rest}\n").into_bytes() };
    }
    out
}

fn published(p: &Port) -> String {
    match p.bind {
        std::net::IpAddr::V4(ip) => format!("{ip}:{}:{}", p.host, p.guest),
        std::net::IpAddr::V6(ip) => format!("[{ip}]:{}:{}", p.host, p.guest),
    }
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
pub fn parse_ps(text: &str) -> Result<Vec<Listed>, Error> {
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let entry: serde_json::Value =
            serde_json::from_str(line).map_err(|e| Error::Tool { what: "ps".to_owned(), status: Some(0), stderr: format!("its output was not JSON: {e}") })?;
        let names = entry.get("Names").and_then(serde_json::Value::as_str).unwrap_or_default();
        for name in names.split(',').filter_map(|n| n.trim().trim_start_matches('/').parse::<CellName>().ok()) {
            out.push(Listed { name, state: state_of(&entry) });
        }
    }
    Ok(out)
}

#[async_trait]
impl Driver for Oci {
    async fn create(&self, spec: &CellSpec) -> Result<(), Error> {
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
        let image = options.image.as_str();
        self.run_ok(self.cli().args(["pull", "--quiet", image]).deadline(PULL_DEADLINE), "pull", None).await?;
        let mut cmd = self
            .cli()
            .args(["create", "--name", spec.name.as_str()])
            .arg("--label")
            .arg(format!("{WALL_LABEL}={}", spec.wall))
            .args(["--init", "--user", CELL_USER, "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--pids-limit", "4096"])
            .arg("--cpus")
            .arg(spec.resources.cpus.to_string())
            .arg("--memory")
            .arg(format!("{}m", spec.resources.memory_mib));
        if spec.sealed {
            cmd = cmd.args(["--network", "none"]);
        }
        for p in &spec.ports {
            cmd = cmd.arg("--publish").arg(published(p));
        }
        let cmd = cmd.arg(image).args(["sleep", "infinity"]).deadline(QUICK_DEADLINE);
        self.run_ok(cmd, "create", Some(&spec.name)).await.map(drop)
    }

    async fn start(&self, name: &CellName) -> Result<(), Error> {
        self.require(name).await?;
        self.run_ok(self.cli().args(["start", name.as_str()]).deadline(QUICK_DEADLINE), "start", Some(name)).await.map(drop)
    }

    async fn exec(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Output, Error> {
        let mut cmd = self.exec_command(name, argv, &opts, opts.stdin.is_some())?;
        if let Some(bytes) = opts.stdin {
            cmd = cmd.stdin_bytes(bytes);
        }
        if let Some(deadline) = opts.deadline {
            cmd = cmd.deadline(deadline);
        }
        let out = self.runner.run(&cmd).await?;
        if !out.success() && out.stderr_lossy().to_ascii_lowercase().contains("no such container") {
            return Err(Error::NotFound { name: name.to_string() });
        }
        Ok(without_exec_note(out))
    }

    async fn stream(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Duplex, Error> {
        if opts.stdin.is_some() {
            return Err(Error::Unsupported { what: "stdin bytes for a stream (write them to the stream)".to_owned() });
        }
        let mut cmd = self.exec_command(name, argv, &opts, true)?;
        if let Some(deadline) = opts.deadline {
            cmd = cmd.deadline(deadline);
        }
        self.runner.duplex(&cmd).await
    }

    async fn copy_in(&self, name: &CellName, host: &Path, guest: &str, owner: Option<&GuestUser>) -> Result<(), Error> {
        check_guest_path(guest)?;
        let user = Oci::channel_user(owner);
        let as_owner = |argv: Vec<String>| self.cli().args(["exec", "-i", "--user", user, name.as_str()]).args(argv);
        let fail = |what: &str, out: &Output| failure(what, Some(name), &without_exec_note(out.clone()));
        let local = SystemRunner::new();
        let channel = Channel { local: &local, remote: &*self.runner, as_owner: &as_owner, fail: &fail, quick: QUICK_DEADLINE, deadline: COPY_DEADLINE };
        copy::send(&channel, host, guest).await
    }

    async fn copy_out(&self, name: &CellName, guest: &str, host: &Path) -> Result<(), Error> {
        check_guest_path(guest)?;
        let as_owner = |argv: Vec<String>| self.cli().args(["exec", "-i", "--user", CELL_USER, name.as_str()]).args(argv);
        let fail = |what: &str, out: &Output| failure(what, Some(name), &without_exec_note(out.clone()));
        let local = SystemRunner::new();
        let channel = Channel { local: &local, remote: &*self.runner, as_owner: &as_owner, fail: &fail, quick: QUICK_DEADLINE, deadline: COPY_DEADLINE };
        copy::fetch(&channel, guest, host).await
    }

    async fn stop(&self, name: &CellName) -> Result<(), Error> {
        self.require(name).await?;
        self.run_ok(self.cli().args(["stop", name.as_str()]).deadline(QUICK_DEADLINE), "stop", Some(name)).await.map(drop)
    }

    async fn remove(&self, name: &CellName) -> Result<(), Error> {
        self.run_ok(self.cli().args(["rm", "--force", name.as_str()]).deadline(QUICK_DEADLINE), "rm", Some(name)).await.map(drop)
    }

    async fn list(&self) -> Result<Vec<Listed>, Error> {
        let out = self.run_ok(self.cli().args(["ps", "--all", "--format", "{{json .}}"]).deadline(QUICK_DEADLINE), "ps", None).await?;
        parse_ps(&out.stdout_lossy())
    }
}
