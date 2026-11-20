//! OpenShell sandboxes as cells, run natively or through `wsl.exe`. The
//! argument lists follow OpenShell 0.1.4's help text. What OpenShell does
//! not do (stop, copy out, clone, ports, mounts, networks) is refused as
//! unsupported.

mod argv;
mod listing;
mod names;
mod policy;
mod profile;

pub use argv::{create, delete, exec, get, list, list_labelled, logs, policy_full, upload, version, Create};
pub use listing::{parse_list, phase_of, policy_denied, SandboxList};
pub use names::{Distro, ImageRef, Label, ProviderName, SandboxName};
pub use policy::PolicyFile;
pub use profile::{parse_wsl_list, parse_wslpath, wsl_program, Host, Invocation, Profile};

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::exec::{tail, Command, Duplex, Output, Runner, SystemRunner};
use crate::{CellName, CellSpec, CellState};
use crate::{Driver, Error, ExecOpts, GuestUser, Listed};
use async_trait::async_trait;

const CREATE_DEADLINE: Duration = Duration::from_secs(10 * 60);
const COPY_DEADLINE: Duration = Duration::from_secs(10 * 60);
const QUICK_DEADLINE: Duration = Duration::from_secs(30);

/// What a new sandbox gets that a [`CellSpec`] has no field for.
#[derive(Debug, Clone)]
pub struct CreateExtras {
    /// A spec's `ImagePin` is a VM disk, so a sandbox's image is given here.
    pub image: ImageRef,
    pub provider: Option<ProviderName>,
    pub policy: Option<String>,
    pub label: Option<Label>,
    pub command: Vec<String>,
}

pub struct OpenShell {
    runner: Arc<dyn Runner>,
    profile: Profile,
    host: Host,
    extras: Option<CreateExtras>,
}

impl OpenShell {
    pub fn new(runner: Arc<dyn Runner>, profile: Profile, host: Host) -> OpenShell {
        OpenShell { runner, profile, host, extras: None }
    }

    pub fn system(profile: Profile) -> OpenShell {
        OpenShell::new(Arc::new(SystemRunner::new()), profile, Host::current())
    }

    #[must_use]
    pub fn with_extras(mut self, extras: CreateExtras) -> OpenShell {
        self.extras = Some(extras);
        self
    }

    fn command(&self, args: &[String]) -> Result<Command, Error> {
        let inv = self.profile.invocation(&self.host, args)?;
        Ok(Command::new(inv.file).args(inv.args).pass_env(&["PATH", "HOME", "SystemRoot"]))
    }

    async fn run(&self, cmd: Command, what: &str) -> Result<Output, Error> {
        let out = self.runner.run(&cmd).await?;
        if out.timed_out {
            return Err(Error::Tool { what: what.to_owned(), status: None, stderr: "It did not finish before its deadline and was stopped.".to_owned() });
        }
        Ok(out)
    }

    async fn run_ok(&self, args: &[String], what: &str, deadline: Duration) -> Result<Output, Error> {
        let out = self.run(self.command(args)?.deadline(deadline), what).await?;
        if out.success() {
            Ok(out)
        } else {
            Err(Error::Tool { what: what.to_owned(), status: out.status, stderr: tail(&out.stderr_lossy()) })
        }
    }

    async fn require(&self, name: &CellName) -> Result<(), Error> {
        let sandbox = sandbox_name(name)?;
        let out = self.run(self.command(&get(&sandbox))?.deadline(QUICK_DEADLINE), "openshell sandbox get").await?;
        if out.success() {
            return Ok(());
        }
        if not_found(&out.stderr_lossy()) {
            return Err(Error::NotFound { name: name.to_string() });
        }
        Err(Error::Tool { what: "openshell sandbox get".to_owned(), status: out.status, stderr: tail(&out.stderr_lossy()) })
    }
}

fn sandbox_name(name: &CellName) -> Result<SandboxName, Error> {
    SandboxName::new(name.as_str())
}

fn not_found(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("not found") || s.contains("notfound") || s.contains("does not exist")
}

fn state_of(phase: Option<&serde_json::Value>) -> CellState {
    match phase.and_then(serde_json::Value::as_str).map(str::to_ascii_lowercase).as_deref() {
        Some("ready" | "running") => CellState::Running,
        Some("provisioning" | "creating" | "pending") => CellState::Creating,
        Some("deleting" | "terminating") => CellState::Removing,
        Some("stopped") => CellState::Stopped,
        _ => CellState::Broken,
    }
}

fn refuse(what: &str) -> Error {
    Error::Unsupported { what: what.to_owned() }
}

#[async_trait]
impl Driver for OpenShell {
    async fn create(&self, spec: &CellSpec) -> Result<(), Error> {
        if !spec.ports.is_empty() {
            return Err(refuse("publishing ports from an OpenShell sandbox"));
        }
        if !spec.mounts.is_empty() {
            return Err(refuse("host folders in an OpenShell sandbox (copy them in instead)"));
        }
        if spec.network.is_some() {
            return Err(refuse("a named network for an OpenShell sandbox (its policy controls the network)"));
        }
        if spec.clone_from.is_some() {
            return Err(refuse("cloning an OpenShell sandbox"));
        }
        let Some(extras) = &self.extras else {
            return Err(refuse("a sandbox without a container image (call with_extras first)"));
        };
        let name = SandboxName::for_create(spec.name.as_str())?;
        let args = create(&Create {
            name,
            image: extras.image.clone(),
            label: extras.label.clone(),
            provider: extras.provider.clone(),
            policy: extras.policy.clone(),
            command: extras.command.clone(),
        })?;
        self.run_ok(&args, "openshell sandbox create", CREATE_DEADLINE).await.map(|_| ())
    }

    async fn start(&self, name: &CellName) -> Result<(), Error> {
        // A sandbox runs from creation; this confirms it is there.
        self.require(name).await
    }

    async fn exec(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Output, Error> {
        let sandbox = sandbox_name(name)?;
        if opts.root {
            return Err(refuse("a command as root (OpenShell runs a sandbox's commands as the sandbox's own user)"));
        }
        let full = crate::command_line(argv, &opts.env)?;
        let secs = opts.deadline.map(|d| u32::try_from(d.as_secs().max(1)).unwrap_or(u32::MAX));
        let mut cmd = self.command(&exec(&sandbox, secs, &full))?;
        if let Some(bytes) = opts.stdin {
            cmd = cmd.stdin_bytes(bytes);
        }
        if let Some(d) = opts.deadline {
            // A little longer than OpenShell's own limit, so its message wins.
            cmd = cmd.deadline(d + Duration::from_secs(10));
        }
        let out = self.runner.run(&cmd).await?;
        Ok(out)
    }

    async fn stream(&self, name: &CellName, argv: &[String]) -> Result<Duplex, Error> {
        let sandbox = sandbox_name(name)?;
        if argv.is_empty() {
            return Err(refuse("an empty command"));
        }
        let cmd = self.command(&exec(&sandbox, None, argv))?;
        Ok(self.runner.duplex(&cmd).await?)
    }

    async fn copy_in(&self, name: &CellName, host: &Path, guest: &str, owner: Option<&GuestUser>) -> Result<(), Error> {
        let sandbox = sandbox_name(name)?;
        if owner.is_some() {
            return Err(refuse("a copy owned by another user (OpenShell writes a sandbox's files as the sandbox's own user)"));
        }
        if !host.is_absolute() {
            return Err(Error::BadPath { path: host.display().to_string(), rule: "a host path must be absolute" });
        }
        let args = upload(&sandbox, &host.to_string_lossy(), guest, false)?;
        self.run_ok(&args, "openshell sandbox upload", COPY_DEADLINE).await.map(|_| ())
    }

    async fn copy_out(&self, _name: &CellName, _guest: &str, _host: &Path) -> Result<(), Error> {
        Err(refuse("copying a file out of an OpenShell sandbox (run `cat` in it and read the output instead)"))
    }

    async fn stop(&self, _name: &CellName) -> Result<(), Error> {
        Err(refuse("stopping an OpenShell sandbox (remove it instead)"))
    }

    async fn remove(&self, name: &CellName) -> Result<(), Error> {
        let sandbox = sandbox_name(name)?;
        let out = self.run(self.command(&delete(&sandbox))?.deadline(CREATE_DEADLINE), "openshell sandbox delete").await?;
        if out.success() || not_found(&out.stderr_lossy()) {
            return Ok(());
        }
        Err(Error::Tool { what: "openshell sandbox delete".to_owned(), status: out.status, stderr: tail(&out.stderr_lossy()) })
    }

    async fn list(&self) -> Result<Vec<Listed>, Error> {
        let out = self.run_ok(&list(), "openshell sandbox list", QUICK_DEADLINE).await?;
        let parsed = parse_list(&out.stdout_lossy()).map_err(|e| Error::Tool { what: "openshell sandbox list".to_owned(), status: out.status, stderr: e.to_string() })?;
        if parsed.more {
            return Err(Error::Tool {
                what: "openshell sandbox list".to_owned(),
                status: out.status,
                stderr: "There is more than one page of sandboxes. Delete old ones and list again.".to_owned(),
            });
        }
        Ok(parsed.entries().into_iter().filter_map(|(name, phase)| Some(Listed { name: name?.parse::<CellName>().ok()?, state: state_of(phase.as_ref()) })).collect())
    }
}
