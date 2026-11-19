//! OpenShell sandboxes as Adytum cells: the argument list of each
//! `openshell` command (native, or through `wsl.exe` on Windows), its JSON,
//! the policy file a launch pins, and a [`Driver`] that runs them through
//! `adytum-exec`.
//!
//! What OpenShell does not do, the driver refuses with `Unsupported`:
//! stopping a sandbox, copying a file out, cloning, port publishing, host
//! mounts, networks, and secrets other than by provider name. A sandbox
//! runs from the moment it is created, so `start` only confirms it exists.
//!
//! The argument lists were written from OpenShell 0.1.4's help text and
//! XenoNet's use of it; the golden transcript in `tests/golden` is written
//! by hand, not recorded, because no OpenShell was available to record.

mod argv;
mod error;
mod listing;
mod names;
mod policy;
mod profile;

pub use argv::{create, delete, exec, get, list, list_labelled, logs, policy_full, upload, version, Create};
pub use error::OpenShellError;
pub use listing::{parse_list, phase_of, policy_denied, SandboxList};
pub use names::{check_guest_path, Distro, ImageRef, Label, ProviderName, SandboxName, MAX_NEW_NAME};
pub use policy::{sha256_hex, PolicyFile, MAX_POLICY_BYTES};
pub use profile::{parse_wsl_list, parse_wslpath, wsl_program, Host, Invocation, Profile};

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adytum_driver::{Driver, DriverError, ExecOpts, Listed};
use adytum_exec::{Cancel, Command, Duplex, Output, Runner, SystemRunner};
use adytum_model::{CellName, CellSpec, CellState};
use async_trait::async_trait;

const CREATE_DEADLINE: Duration = Duration::from_secs(10 * 60);
const COPY_DEADLINE: Duration = Duration::from_secs(10 * 60);
const QUICK_DEADLINE: Duration = Duration::from_secs(30);
const STDERR_KEPT: usize = 2000;

/// What a new sandbox gets that a [`CellSpec`] has no field for.
#[derive(Debug, Clone)]
pub struct CreateExtras {
    /// The container image. A spec's `ImagePin` is a VM disk, so it is not used.
    pub image: ImageRef,
    pub provider: Option<ProviderName>,
    /// The policy file as the `openshell` process sees it.
    pub policy: Option<String>,
    /// A label for the caller to select its sandboxes by.
    pub label: Option<Label>,
    /// What the sandbox runs; empty runs the image's own command.
    pub command: Vec<String>,
}

/// The OpenShell driver.
pub struct OpenShell {
    runner: Arc<dyn Runner>,
    profile: Profile,
    host: Host,
    extras: Option<CreateExtras>,
    pass_env: Vec<String>,
}

impl OpenShell {
    /// A driver that runs `openshell` for `profile` through `runner`.
    pub fn new(runner: Arc<dyn Runner>, profile: Profile, host: Host) -> OpenShell {
        OpenShell { runner, profile, host, extras: None, pass_env: ["PATH", "HOME", "SystemRoot"].map(String::from).to_vec() }
    }

    /// The real thing: commands run on this machine.
    pub fn system(profile: Profile) -> OpenShell {
        OpenShell::new(Arc::new(SystemRunner::new()), profile, Host::current())
    }

    /// What `create` gives every new sandbox beyond the spec.
    #[must_use]
    pub fn with_extras(mut self, extras: CreateExtras) -> OpenShell {
        self.extras = Some(extras);
        self
    }

    /// Variables copied from this process into `openshell`'s environment,
    /// beyond PATH, HOME and SystemRoot (for a gateway chosen by variable).
    #[must_use]
    pub fn with_pass_env(mut self, names: &[&str]) -> OpenShell {
        self.pass_env.extend(names.iter().map(|n| (*n).to_owned()));
        self
    }

    fn command(&self, args: &[String]) -> Result<Command, DriverError> {
        let inv = self.profile.invocation(&self.host, args).map_err(unsupported)?;
        let names: Vec<&str> = self.pass_env.iter().map(String::as_str).collect();
        Ok(Command::new(inv.file).args(inv.args).pass_env(&names))
    }

    async fn run(&self, cmd: Command, what: &str) -> Result<Output, DriverError> {
        let out = self.runner.run(&cmd, &Cancel::new()).await?;
        if out.timed_out {
            return Err(DriverError::Tool { what: what.to_owned(), status: None, stderr: "It did not finish before its deadline and was stopped.".to_owned() });
        }
        Ok(out)
    }

    async fn run_ok(&self, args: &[String], what: &str, deadline: Duration) -> Result<Output, DriverError> {
        let out = self.run(self.command(args)?.deadline(deadline), what).await?;
        if out.success() {
            Ok(out)
        } else {
            Err(DriverError::Tool { what: what.to_owned(), status: out.status, stderr: tail(&out.stderr_lossy()) })
        }
    }

    async fn require(&self, name: &CellName) -> Result<(), DriverError> {
        let sandbox = sandbox_name(name)?;
        let out = self.run(self.command(&get(&sandbox))?.deadline(QUICK_DEADLINE), "openshell sandbox get").await?;
        if out.success() {
            return Ok(());
        }
        if not_found(&out.stderr_lossy()) {
            return Err(DriverError::NotFound { name: name.to_string() });
        }
        Err(DriverError::Tool { what: "openshell sandbox get".to_owned(), status: out.status, stderr: tail(&out.stderr_lossy()) })
    }
}

fn unsupported(e: OpenShellError) -> DriverError {
    DriverError::Unsupported { what: e.to_string() }
}

fn sandbox_name(name: &CellName) -> Result<SandboxName, DriverError> {
    adytum_policy::name_is_ours(name.as_str())?;
    SandboxName::new(name.as_str()).map_err(unsupported)
}

fn not_found(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("not found") || s.contains("notfound") || s.contains("does not exist")
}

fn tail(text: &str) -> String {
    let text = text.trim();
    if text.len() <= STDERR_KEPT {
        return text.to_owned();
    }
    let mut start = text.len() - STDERR_KEPT;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
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

fn refuse(what: &str) -> DriverError {
    DriverError::Unsupported { what: what.to_owned() }
}

#[async_trait]
impl Driver for OpenShell {
    async fn create(&self, spec: &CellSpec) -> Result<(), DriverError> {
        adytum_policy::name_is_ours(spec.name.as_str())?;
        // OpenShell takes none of these; saying so beats ignoring them.
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
        if !spec.secrets.is_empty() {
            return Err(refuse("secrets by name (give the sandbox an OpenShell provider instead)"));
        }
        if spec.automation_port.is_some() {
            return Err(refuse("an automation port in an OpenShell sandbox"));
        }
        let Some(extras) = &self.extras else {
            return Err(refuse("a sandbox without a container image (call with_extras first)"));
        };
        let name = SandboxName::for_create(spec.name.as_str()).map_err(unsupported)?;
        let args = create(&Create {
            name,
            image: extras.image.clone(),
            label: extras.label.clone(),
            provider: extras.provider.clone(),
            policy: extras.policy.clone(),
            command: extras.command.clone(),
        })
        .map_err(unsupported)?;
        self.run_ok(&args, "openshell sandbox create", CREATE_DEADLINE).await.map(|_| ())
    }

    async fn start(&self, name: &CellName) -> Result<(), DriverError> {
        // A sandbox runs from creation; this confirms it is there.
        self.require(name).await
    }

    async fn exec(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Output, DriverError> {
        let sandbox = sandbox_name(name)?;
        if opts.root {
            return Err(refuse("a command as root (OpenShell runs a sandbox's commands as the sandbox's own user)"));
        }
        let Some(program) = argv.first() else {
            return Err(refuse("an empty command"));
        };
        if argv.iter().chain(opts.env.iter().flat_map(|(k, v)| [k, v])).any(|a| a.contains('\0')) {
            return Err(refuse("a NUL byte in a command"));
        }
        let mut full: Vec<String> = Vec::new();
        if !opts.env.is_empty() {
            for (key, _) in &opts.env {
                let ok = key.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !ok {
                    return Err(refuse(&format!("the variable name {key:?}")));
                }
            }
            if program.contains('=') {
                return Err(refuse("a program name with = in it together with variables"));
            }
            full.push("env".to_owned());
            full.extend(opts.env.iter().map(|(k, v)| format!("{k}={v}")));
        }
        full.extend(argv.iter().cloned());
        let secs = opts.deadline.map(|d| u32::try_from(d.as_secs().max(1)).unwrap_or(u32::MAX));
        let mut cmd = self.command(&exec(&sandbox, secs, &full))?;
        if let Some(bytes) = opts.stdin {
            cmd = cmd.stdin_bytes(bytes);
        }
        if let Some(d) = opts.deadline {
            // A little longer than OpenShell's own limit, so its message wins.
            cmd = cmd.deadline(d + Duration::from_secs(10));
        }
        let out = self.runner.run(&cmd, &Cancel::new()).await?;
        Ok(out)
    }

    async fn stream(&self, name: &CellName, argv: &[String]) -> Result<Duplex, DriverError> {
        let sandbox = sandbox_name(name)?;
        if argv.is_empty() {
            return Err(refuse("an empty command"));
        }
        let cmd = self.command(&exec(&sandbox, None, argv))?;
        Ok(self.runner.duplex(&cmd).await?)
    }

    async fn copy_in(&self, name: &CellName, host: &Path, guest: &str) -> Result<(), DriverError> {
        let sandbox = sandbox_name(name)?;
        if !host.is_absolute() {
            return Err(DriverError::BadPath { path: host.display().to_string(), rule: "a host path must be absolute" });
        }
        let args = upload(&sandbox, &host.to_string_lossy(), guest, false)
            .map_err(|_| DriverError::BadPath { path: guest.to_owned(), rule: "a guest path must be absolute, with no .. or control characters" })?;
        self.run_ok(&args, "openshell sandbox upload", COPY_DEADLINE).await.map(|_| ())
    }

    async fn copy_out(&self, _name: &CellName, _guest: &str, _host: &Path) -> Result<(), DriverError> {
        Err(refuse("copying a file out of an OpenShell sandbox (run `cat` in it and read the output instead)"))
    }

    async fn stop(&self, _name: &CellName) -> Result<(), DriverError> {
        Err(refuse("stopping an OpenShell sandbox (remove it instead)"))
    }

    async fn remove(&self, name: &CellName) -> Result<(), DriverError> {
        let sandbox = sandbox_name(name)?;
        let out = self.run(self.command(&delete(&sandbox))?.deadline(CREATE_DEADLINE), "openshell sandbox delete").await?;
        // Removing twice is fine, as it is for the other drivers.
        if out.success() || not_found(&out.stderr_lossy()) {
            return Ok(());
        }
        Err(DriverError::Tool { what: "openshell sandbox delete".to_owned(), status: out.status, stderr: tail(&out.stderr_lossy()) })
    }

    async fn list(&self) -> Result<Vec<Listed>, DriverError> {
        let out = self.run_ok(&list(), "openshell sandbox list", QUICK_DEADLINE).await?;
        let parsed = parse_list(&out.stdout_lossy()).map_err(|e| DriverError::Tool { what: "openshell sandbox list".to_owned(), status: out.status, stderr: e.to_string() })?;
        if parsed.more {
            return Err(DriverError::Tool {
                what: "openshell sandbox list".to_owned(),
                status: out.status,
                stderr: "There is more than one page of sandboxes. Delete old ones and list again.".to_owned(),
            });
        }
        // Sandboxes not named by Adytum are none of its business.
        Ok(parsed.entries().into_iter().filter_map(|(name, phase)| Some(Listed { name: name?.parse::<CellName>().ok()?, state: state_of(phase.as_ref()) })).collect())
    }
}
