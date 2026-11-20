//! Where a driver runs its programs: on this machine, or inside a cell it
//! reaches through that cell's own driver. Either way a driver gets a
//! [`Runner`], so one driver serves both.

use std::fmt;
#[cfg(feature = "lima")]
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::exec::{Command, Duplex, Output, Runner, SystemRunner};
use crate::{CellName, Driver, Error, ExecOpts, KindName};

#[derive(Clone)]
pub enum Host {
    Machine,
    /// Inside `name`, as root, through `driver`, the cell's own driver.
    Cell {
        name: CellName,
        driver: Arc<dyn Driver>,
    },
}

impl Host {
    pub fn cell(&self) -> Option<&CellName> {
        match self {
            Host::Machine => None,
            Host::Cell { name, .. } => Some(name),
        }
    }

    pub fn runner(&self) -> Arc<dyn Runner> {
        match self {
            Host::Machine => Arc::new(SystemRunner::new()),
            Host::Cell { name, driver } => Arc::new(InCell { name: name.clone(), driver: driver.clone() }),
        }
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Host::Machine => f.write_str("Machine"),
            Host::Cell { name, .. } => write!(f, "Cell({name})"),
        }
    }
}

/// Runs each command in a cell as root through the cell's driver. The
/// program and its arguments are the guest's; nothing of this machine's
/// environment goes along unless the command names it.
pub struct InCell {
    name: CellName,
    driver: Arc<dyn Driver>,
}

impl InCell {
    pub fn new(name: CellName, driver: Arc<dyn Driver>) -> InCell {
        InCell { name, driver }
    }

    fn opts(cmd: &Command) -> Result<(Vec<String>, ExecOpts), Error> {
        if cmd.working_dir().is_some() {
            return Err(Error::Unsupported { what: "a working folder for a command run inside a cell".to_owned() });
        }
        let env = cmd.env_vars().iter().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();
        Ok((cmd.argv(), ExecOpts { env, stdin: None, deadline: cmd.deadline_limit(), root: true }))
    }
}

#[async_trait]
impl Runner for InCell {
    async fn run(&self, cmd: &Command) -> Result<Output, Error> {
        let (argv, opts) = InCell::opts(cmd)?;
        let opts = ExecOpts { stdin: cmd.stdin().map(<[u8]>::to_vec), ..opts };
        self.driver.exec(&self.name, &argv, opts).await
    }

    async fn duplex(&self, cmd: &Command) -> Result<Duplex, Error> {
        let (argv, opts) = InCell::opts(cmd)?;
        self.driver.stream(&self.name, &argv, opts).await
    }
}

/// Makes the driver for each kind of cell on each host. [`Cells`](crate::Cells)
/// asks it for the driver of every cell it touches.
pub trait Drivers: Send + Sync {
    fn driver(&self, kind: KindName, host: Host) -> Result<Arc<dyn Driver>, Error>;

    /// The kinds a reconcile lists on this machine even when the ledger has
    /// none of them, so it can report cells it never recorded.
    fn machine_kinds(&self) -> Vec<KindName>;
}

/// Lima VMs on this machine under `lima_home`, containers through `docker`
/// on this machine and through `nerdctl` in a Lima cell.
#[cfg(feature = "lima")]
#[derive(Debug, Clone)]
pub struct SystemDrivers {
    lima_home: PathBuf,
}

#[cfg(feature = "lima")]
impl SystemDrivers {
    /// `lima_home` is the `LIMA_HOME` for Lima cells, such as
    /// [`lima_home_in`](crate::lima::lima_home_in) of Adytum's folder.
    pub fn new(lima_home: PathBuf) -> SystemDrivers {
        SystemDrivers { lima_home }
    }
}

#[cfg(feature = "lima")]
fn unsupported(what: &str) -> Error {
    Error::Unsupported { what: what.to_owned() }
}

#[cfg(feature = "lima")]
impl Drivers for SystemDrivers {
    fn driver(&self, kind: KindName, host: Host) -> Result<Arc<dyn Driver>, Error> {
        match (kind, host) {
            #[cfg(unix)]
            (KindName::Lima, Host::Machine) => Ok(Arc::new(crate::lima::Lima::system(self.lima_home.clone())?)),
            #[cfg(not(unix))]
            (KindName::Lima, Host::Machine) => Err(unsupported("Lima cells on this machine (they need a Mac or Linux host)")),
            (KindName::Lima, Host::Cell { .. }) => Err(unsupported("a Lima cell inside another cell")),
            #[cfg(feature = "container")]
            (KindName::Container, Host::Machine) => Ok(Arc::new(crate::container::Oci::docker())),
            #[cfg(feature = "container")]
            (KindName::Container, host @ Host::Cell { .. }) => Ok(Arc::new(crate::container::Oci::nerdctl(host.runner()))),
            #[cfg(not(feature = "container"))]
            (KindName::Container, _) => Err(unsupported("container cells in a build without the container feature")),
            (KindName::OpenShell, _) => Err(unsupported("OpenShell sandboxes here (make an OpenShell driver with its profile instead)")),
        }
    }

    fn machine_kinds(&self) -> Vec<KindName> {
        if cfg!(unix) {
            vec![KindName::Lima]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ok, FakeDriver};
    use crate::CellState;
    use std::time::Duration;

    #[tokio::test]
    async fn a_command_in_a_cell_runs_as_root_through_its_driver_with_only_its_own_variables() -> Result<(), Box<dyn std::error::Error>> {
        let parent: CellName = "adytum-p".parse()?;
        let fake = FakeDriver::new();
        fake.add_cell(&parent, CellState::Running);
        fake.on_exec(|e| ok(&format!("{} {:?} {}", e.argv.join(" "), e.env, String::from_utf8_lossy(&e.stdin))));
        let host = Host::Cell { name: parent.clone(), driver: Arc::new(fake.clone()) };
        let runner = host.runner();
        let cmd = Command::new("nerdctl").args(["ps", "-a"]).env("A", "1").stdin_bytes(b"in".to_vec()).deadline(Duration::from_secs(5));
        let out = runner.run(&cmd).await?;
        assert_eq!(out.stdout_lossy(), r#"nerdctl ps -a {"A": "1"} in"#);
        assert!(runner.duplex(&Command::new("cat")).await.is_err(), "the fake has no stream answer");
        let lines: Vec<String> = fake.calls().iter().map(ToString::to_string).collect();
        assert_eq!(lines, ["exec adytum-p (root) nerdctl ps -a", "stream adytum-p (root) cat"]);
        assert!(matches!(runner.run(&Command::new("ls").cwd("/tmp")).await, Err(Error::Unsupported { .. })));
        assert_eq!(format!("{host:?}"), "Cell(adytum-p)");
        assert_eq!(host.cell(), Some(&parent));
        assert_eq!(Host::Machine.cell(), None);
        Ok(())
    }

    #[cfg(feature = "lima")]
    #[test]
    fn the_system_drivers_run_containers_in_lima_and_refuse_what_they_cannot() {
        let drivers = SystemDrivers::new(std::env::temp_dir().join("adytum-system-drivers"));
        assert_eq!(drivers.driver(KindName::Lima, Host::Machine).is_ok(), cfg!(unix));
        let parent = Host::Cell { name: "adytum-p".parse().unwrap(), driver: Arc::new(FakeDriver::new()) };
        assert!(matches!(drivers.driver(KindName::Lima, parent.clone()), Err(Error::Unsupported { .. })));
        assert!(matches!(drivers.driver(KindName::OpenShell, Host::Machine), Err(Error::Unsupported { .. })));
        assert_eq!(drivers.driver(KindName::Container, parent).is_ok(), cfg!(feature = "container"));
        assert_eq!(drivers.machine_kinds().contains(&KindName::Lima), cfg!(unix));
    }
}
