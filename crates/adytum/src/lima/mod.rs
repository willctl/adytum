mod guest;
mod network;
mod pins;
mod template;

pub use pins::{image, image_names, DEFAULT_IMAGE};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::copy::{self, absolute, Channel};
use crate::exec::{tail, Command, Duplex, Output, Runner, SystemRunner};
use crate::{check_guest_path, command_line, CellName, CellSpec, CellState, Driver, Error, ExecOpts, GuestUser, Listed};
use async_trait::async_trait;

/// A unix socket path on macOS holds 104 bytes, its closing NUL included.
pub(crate) const SOCKET_BUDGET: usize = 104;
pub(crate) const LONGEST_SOCKET: &str = "default_ep.sock";

const CREATE_DEADLINE: Duration = Duration::from_secs(30 * 60);
const START_DEADLINE: Duration = Duration::from_secs(10 * 60);
const STOP_DEADLINE: Duration = Duration::from_secs(5 * 60);
const COPY_DEADLINE: Duration = Duration::from_secs(10 * 60);
const QUICK_DEADLINE: Duration = Duration::from_secs(2 * 60);

pub fn lima_home_in(adytum_home: &Path) -> PathBuf {
    adytum_home.join("lima")
}

/// Cells as Lima VMs, by running `limactl` with a `LIMA_HOME` of Adytum's
/// own, so the VMs in Lima's default home are never listed or reached.
pub struct Lima {
    runner: Arc<dyn Runner>,
    limactl: PathBuf,
    home: PathBuf,
}

impl Lima {
    /// `home` is the `LIMA_HOME`, and must be absolute.
    pub fn new(runner: Arc<dyn Runner>, limactl: PathBuf, home: PathBuf) -> Result<Lima, Error> {
        if !home.is_absolute() {
            return Err(Error::BadPath { path: home.display().to_string(), rule: "Adytum's Lima folder must be an absolute path" });
        }
        Ok(Lima { runner, limactl, home })
    }

    pub fn system(home: PathBuf) -> Result<Lima, Error> {
        Lima::new(Arc::new(SystemRunner::new()), PathBuf::from("limactl"), home)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    fn limactl(&self) -> Command {
        Command::new(&self.limactl).env("LIMA_HOME", &self.home).pass_env(&["PATH", "HOME"])
    }

    fn check_socket_path(&self, name: &CellName) -> Result<(), Error> {
        let longest = self.home.join(name.as_str()).join(LONGEST_SOCKET);
        if longest.as_os_str().len() >= SOCKET_BUDGET {
            return Err(Error::BadPath { path: self.home.display().to_string(), rule: "it is too long for the sockets Lima keeps inside it (about 100 bytes in all)" });
        }
        Ok(())
    }

    fn networks_of(&self, name: &CellName) -> Vec<String> {
        std::fs::read_to_string(self.home.join(name.as_str()).join("lima.yaml")).map(|t| network::instance_networks(&t)).unwrap_or_default()
    }

    async fn lima_networks(&self) -> Result<Vec<(String, String)>, Error> {
        let out = self.run_ok(self.limactl().args(["network", "list"]).deadline(QUICK_DEADLINE), "limactl network list", None).await?;
        Ok(network::parse_network_list(&out.stdout_lossy()))
    }

    /// `None` if the spec names no network or Lima has not made it yet.
    pub async fn network_gateway(&self, spec: &CellSpec) -> Result<Option<String>, Error> {
        let Some(name) = template::network_for(spec) else { return Ok(None) };
        Ok(self.lima_networks().await?.into_iter().find(|(n, _)| *n == name).map(|(_, gateway)| gateway))
    }

    async fn ensure_network(&self, spec: &CellSpec) -> Result<(), Error> {
        let (Some(name), Some(network)) = (template::network_for(spec), spec.network.as_ref()) else { return Ok(()) };
        let existing = self.lima_networks().await?;
        if existing.iter().any(|(n, _)| *n == name) {
            return Ok(());
        }
        // Lima does not refuse two networks with one gateway.
        let gateway = (0..=u8::MAX)
            .map(|shift| network::candidate_gateway(&spec.wall, network, shift))
            .find(|g| !existing.iter().any(|(_, held)| held == g))
            .ok_or_else(|| Error::Unsupported { what: "another network: every address Adytum may use is taken".to_owned() })?;
        let cmd = self.limactl().args(["network", "create", "--tty=false"]).arg(&name).arg(format!("--gateway={gateway}/24")).deadline(QUICK_DEADLINE);
        self.run_ok(cmd, "limactl network create", Some(&spec.name)).await.map(drop)
    }

    /// Best effort: a network left behind goes with the next cell that used it.
    async fn prune_networks(&self, candidates: &[String]) {
        let mut in_use = BTreeSet::new();
        for entry in std::fs::read_dir(&self.home).into_iter().flatten().flatten() {
            if let Ok(text) = std::fs::read_to_string(entry.path().join("lima.yaml")) {
                in_use.extend(network::instance_networks(&text));
            }
        }
        let unused: Vec<&String> = candidates.iter().filter(|n| !in_use.contains(*n)).collect();
        if !unused.is_empty() {
            let _ = self.run(self.limactl().args(["network", "delete", "--force", "--tty=false"]).args(unused).deadline(QUICK_DEADLINE), "limactl network delete").await;
        }
    }

    async fn run(&self, cmd: Command, what: &str) -> Result<Output, Error> {
        let out = self.runner.run(&cmd).await?;
        if out.timed_out {
            return Err(Error::Tool { what: what.to_owned(), status: None, stderr: "It did not finish before its deadline and was stopped.".to_owned() });
        }
        Ok(out)
    }

    async fn run_ok(&self, cmd: Command, what: &str, name: Option<&CellName>) -> Result<Output, Error> {
        let out = self.run(cmd, what).await?;
        if out.success() {
            return Ok(out);
        }
        Err(tool_failure(what, name, &out))
    }

    async fn require(&self, name: &CellName) -> Result<CellState, Error> {
        self.list().await?.into_iter().find(|c| &c.name == name).map(|c| c.state).ok_or_else(|| Error::NotFound { name: name.to_string() })
    }

    /// `limactl shell` logs in as Lima's admin user, which has sudo; the
    /// command never runs as that user.
    fn shell_command(&self, name: &CellName, argv: &[String], env: &[(String, String)], root: bool) -> Result<Command, Error> {
        let inner = command_line(argv, env)?;
        let full = if root { guest::as_root(inner) } else { guest::as_user(guest::EXEC_USER, inner) };
        Ok(self.guest_shell(name, full))
    }

    fn guest_shell(&self, name: &CellName, full: Vec<String>) -> Command {
        self.limactl().args(["shell", "--workdir=/"]).arg(name.as_str()).arg("--").args(full)
    }
}

fn tool_failure(what: &str, name: Option<&CellName>, out: &Output) -> Error {
    let stderr = out.stderr_lossy();
    let missing = stderr.contains("does not exist") || (stderr.contains("no such file or directory") && stderr.contains("lima.yaml"));
    match name {
        Some(name) if missing => Error::NotFound { name: name.to_string() },
        _ => Error::Tool { what: what.to_owned(), status: out.status, stderr: tail(&stderr) },
    }
}

fn state_of(status: &str) -> CellState {
    match status {
        "Running" => CellState::Running,
        "Stopped" => CellState::Stopped,
        _ => CellState::Broken,
    }
}

#[async_trait]
impl Driver for Lima {
    async fn create(&self, spec: &CellSpec) -> Result<(), Error> {
        self.check_socket_path(&spec.name)?;
        let text = template::render(spec)?;
        let file = self.home.join("_templates").join(format!("{}.yaml", spec.name));
        std::fs::create_dir_all(self.home.join("_templates")).map_err(|source| Error::Io { what: "make the folder for templates", source })?;
        self.ensure_network(spec).await?;
        std::fs::write(&file, text).map_err(|source| Error::Io { what: "write the template", source })?;
        let cmd = self.limactl().arg("create").arg(format!("--name={}", spec.name)).arg("--tty=false").arg(&file).deadline(CREATE_DEADLINE);
        let result = self.run_ok(cmd, "limactl create", Some(&spec.name)).await;
        let _ = std::fs::remove_file(&file);
        if result.is_err() {
            self.prune_networks(&template::network_for(spec).into_iter().collect::<Vec<_>>()).await;
        }
        result.map(drop)
    }

    async fn start(&self, name: &CellName) -> Result<(), Error> {
        // `limactl start` makes a default VM, home folder mounted, for a name it does not know.
        self.require(name).await?;
        let start = || self.limactl().arg("start").arg("--tty=false").arg(name.as_str()).deadline(START_DEADLINE);
        let first = self.run(start(), "limactl start").await?;
        if first.success() {
            return Ok(());
        }
        // Under load Lima's host agent can miss its 5 s start-up window; the VM never came up.
        if !first.stderr_lossy().contains("did not start up in") {
            return Err(tool_failure("limactl start", Some(name), &first));
        }
        let _ = self.run(self.limactl().args(["stop", "--force", "--tty=false"]).arg(name.as_str()).deadline(STOP_DEADLINE), "limactl stop").await;
        let again = self.run(start(), "limactl start").await?;
        if again.success() {
            return Ok(());
        }
        Err(Error::Tool { what: "limactl start (tried twice: Lima's host agent did not start up)".to_owned(), status: again.status, stderr: tail(&again.stderr_lossy()) })
    }

    async fn exec(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Output, Error> {
        let mut cmd = self.shell_command(name, argv, &opts.env, opts.root)?;
        if let Some(bytes) = opts.stdin {
            cmd = cmd.stdin_bytes(bytes);
        }
        if let Some(deadline) = opts.deadline {
            cmd = cmd.deadline(deadline);
        }
        let out = self.runner.run(&cmd).await?;
        if !out.success() && out.stderr_lossy().contains("level=fatal") && out.stderr_lossy().contains("does not exist") {
            return Err(Error::NotFound { name: name.to_string() });
        }
        Ok(out)
    }

    async fn stream(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Duplex, Error> {
        if opts.stdin.is_some() {
            return Err(Error::Unsupported { what: "stdin bytes for a stream (write them to the stream)".to_owned() });
        }
        let mut cmd = self.shell_command(name, argv, &opts.env, opts.root)?;
        if let Some(deadline) = opts.deadline {
            cmd = cmd.deadline(deadline);
        }
        self.runner.duplex(&cmd).await
    }

    async fn copy_in(&self, name: &CellName, host: &Path, guest: &str, owner: Option<&GuestUser>) -> Result<(), Error> {
        check_guest_path(guest)?;
        // sudo's umask is at least 022, and tar applies it even as root.
        let user = owner.map_or(guest::EXEC_USER, GuestUser::as_str);
        let as_owner = |argv: Vec<String>| self.guest_shell(name, guest::as_user(user, argv));
        let fail = |what: &str, out: &Output| tool_failure(what, Some(name), out);
        let channel = Channel { local: &*self.runner, remote: &*self.runner, as_owner: &as_owner, fail: &fail, quick: QUICK_DEADLINE, deadline: COPY_DEADLINE };
        copy::send(&channel, host, guest).await
    }

    async fn copy_out(&self, name: &CellName, guest: &str, host: &Path) -> Result<(), Error> {
        check_guest_path(guest)?;
        let host = absolute(host)?;
        if host.to_string_lossy().contains(':') {
            return Err(Error::BadPath { path: host.display().to_string(), rule: "limactl reads a colon as the end of an instance name" });
        }
        // rsync fails on a file given -r, and a folder needs it.
        let probe = self.shell_command(name, &["test".to_owned(), "-d".to_owned(), guest.to_owned()], &[], false)?.deadline(QUICK_DEADLINE);
        let probe = self.run(probe, "limactl shell").await?;
        let is_dir = match probe.status {
            Some(0) => true,
            Some(1) => false,
            _ => return Err(tool_failure("limactl shell", Some(name), &probe)),
        };
        let mut cmd = self.limactl().arg("copy").arg("--tty=false");
        if is_dir {
            cmd = cmd.arg("-r");
        }
        let cmd = cmd.arg(format!("{name}:{guest}")).arg(host).deadline(COPY_DEADLINE);
        self.run_ok(cmd, "limactl copy", Some(name)).await.map(drop)
    }

    async fn stop(&self, name: &CellName) -> Result<(), Error> {
        self.require(name).await?;
        let cmd = self.limactl().arg("stop").arg("--tty=false").arg(name.as_str()).deadline(STOP_DEADLINE);
        self.run_ok(cmd, "limactl stop", Some(name)).await.map(drop)
    }

    async fn remove(&self, name: &CellName) -> Result<(), Error> {
        let networks = self.networks_of(name);
        let cmd = self.limactl().args(["delete", "--force", "--tty=false"]).arg(name.as_str()).deadline(STOP_DEADLINE);
        self.run_ok(cmd, "limactl delete", Some(name)).await?;
        self.prune_networks(&networks).await;
        Ok(())
    }

    async fn clone_cell(&self, from: &CellName, to: &CellSpec) -> Result<(), Error> {
        self.check_socket_path(&to.name)?;
        let mut edits = template::clone_edits(to)?;
        if to.network.is_none() && !self.networks_of(from).is_empty() {
            edits.push(".networks = []".to_owned());
        }
        let cells = self.list().await?;
        match cells.iter().find(|c| &c.name == from).map(|c| c.state) {
            None => return Err(Error::NotFound { name: from.to_string() }),
            Some(CellState::Stopped) => {}
            Some(_) => return Err(Error::Unsupported { what: format!("cloning {from} while it is not stopped (stop it first)") }),
        }
        if cells.iter().any(|c| c.name == to.name) {
            return Err(
                crate::Refusal::new(crate::RefusalCode::NameTaken, format!("A cell named {} already exists.", to.name), "Pick another name or remove that cell first.").into()
            );
        }
        self.ensure_network(to).await?;
        let mut cmd = self.limactl().arg("clone").arg("--tty=false");
        for edit in &edits {
            cmd = cmd.arg("--set").arg(edit);
        }
        let cmd = cmd.arg(from.as_str()).arg(to.name.as_str()).deadline(CREATE_DEADLINE);
        let result = self.run_ok(cmd, "limactl clone", Some(from)).await;
        if result.is_err() {
            self.prune_networks(&template::network_for(to).into_iter().collect::<Vec<_>>()).await;
        }
        result.map(drop)
    }

    async fn list(&self) -> Result<Vec<Listed>, Error> {
        if !self.home.exists() {
            return Ok(Vec::new());
        }
        let out = self.run_ok(self.limactl().args(["list", "--json"]).deadline(QUICK_DEADLINE), "limactl list", None).await?;
        let mut cells = Vec::new();
        for line in out.stdout_lossy().lines().filter(|l| !l.trim().is_empty()) {
            let value: serde_json::Value =
                serde_json::from_str(line).map_err(|e| Error::Tool { what: "limactl list".to_owned(), status: out.status, stderr: format!("its output was not JSON: {e}") })?;
            let (Some(name), Some(status)) = (value.get("name").and_then(|v| v.as_str()), value.get("status").and_then(|v| v.as_str())) else { continue };
            if let Ok(name) = name.parse::<CellName>() {
                cells.push(Listed { name, state: state_of(status) });
            }
        }
        Ok(cells)
    }
}
