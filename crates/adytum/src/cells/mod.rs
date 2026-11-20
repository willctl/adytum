mod ledger;
mod schema;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ledger::{Ledger, OpKind, OpenOp, Outcome};

use crate::model::tree::{ancestors, descendants, find};
use crate::{Cell, CellId, CellName, CellSpec, CellState, Driver, Drivers, Error, Host, KindName, Refusal, RefusalCode};

/// Opening a ledger makes one, so a missing file is `None`, not zero.
#[cfg(feature = "doctor")]
pub(crate) fn unsettled(home: &Path) -> Option<Result<usize, Error>> {
    home.join(ledger::FILE).exists().then(|| Ledger::open(home).and_then(|l| l.open_ops()).map(|ops| ops.len()))
}

/// Adytum's folder: `$ADYTUM_HOME` if set, else `~/.adytum`.
pub fn home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("ADYTUM_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".adytum"))
}

/// What [`Cells::reconcile`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub removed: Vec<CellName>,
    pub finished: Vec<CellName>,
    pub unrecorded: Vec<CellName>,
    /// Hosts whose cells of a kind could not be listed. Those cells are left
    /// as they are recorded, and the cells inside them are not visited.
    pub unlisted: Vec<Unlisted>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlisted {
    /// `None` is this machine.
    pub host: Option<CellName>,
    pub kind: KindName,
    pub error: String,
}

/// The cells of one Adytum folder: a ledger that records each create,
/// start, stop and remove before a driver runs and settles it after, so a
/// crash at any point leaves something [`Cells::reconcile`] can finish, and
/// the drivers that run each kind of cell on each host. Adytum never
/// touches a cell the ledger did not record.
pub struct Cells {
    ledger: Ledger,
    drivers: Arc<dyn Drivers>,
}

impl Cells {
    /// Opens `<home>/adytum.sqlite`, making it if there is none.
    pub fn open(home: &Path, drivers: Arc<dyn Drivers>) -> Result<Cells, Error> {
        Ok(Cells { ledger: Ledger::open(home)?, drivers })
    }

    #[cfg(test)]
    fn memory(drivers: Arc<dyn Drivers>) -> Result<Cells, Error> {
        Ok(Cells { ledger: Ledger::memory()?, drivers })
    }

    pub fn list(&self) -> Result<Vec<Cell>, Error> {
        self.ledger.cells()
    }

    pub fn get(&self, name: &CellName) -> Result<Cell, Error> {
        self.list()?.into_iter().find(|c| &c.spec.name == name).ok_or_else(|| Error::NotRecorded(name.clone()))
    }

    /// The driver of a recorded cell, on the host the cell runs on: use it
    /// for commands and copies, and `Cells` for the cell's life.
    pub fn driver_of(&self, name: &CellName) -> Result<Arc<dyn Driver>, Error> {
        let cells = self.list()?;
        let cell = find(&cells, name).ok_or_else(|| Error::NotRecorded(name.clone()))?;
        self.drivers.driver(cell.spec.kind.name(), self.host(&cells, cell.spec.parent.as_ref())?)
    }

    /// `parent`'s driver is built the same way, up to this machine.
    fn host(&self, cells: &[Cell], parent: Option<&CellName>) -> Result<Host, Error> {
        let Some(name) = parent else { return Ok(Host::Machine) };
        let above = ancestors(cells, name);
        let mut host = Host::Machine;
        for cell in above.iter().rev().copied().chain(find(cells, name)) {
            let driver = self.drivers.driver(cell.spec.kind.name(), host)?;
            host = Host::Cell { name: cell.spec.name.clone(), driver };
        }
        if host.cell() != Some(name) {
            return Err(Error::NotRecorded(name.clone()));
        }
        Ok(host)
    }

    /// Settles every operation a crash left open, then makes the ledger
    /// agree with what each host lists: this machine first, then each
    /// running cell that holds cells. The cells in a cell that is not
    /// running are left as they are recorded. Run it before other work: it
    /// treats every open operation as interrupted.
    pub async fn reconcile(&self) -> Result<Report, Error> {
        let mut report = Report::default();
        let recorded: BTreeSet<CellName> = self.list()?.into_iter().map(|c| c.spec.name).collect();
        for op in self.ledger.open_ops()?.into_iter().filter(|op| !recorded.contains(&op.name)) {
            // Its cell went with the cell it ran in.
            self.ledger.settle(op.id, Outcome::Failed("its cell is gone".to_owned()))?;
        }
        let mut hosts = VecDeque::from([None]);
        while let Some(host) = hosts.pop_front() {
            self.reconcile_host(host.as_ref(), &mut report).await?;
            let cells = self.list()?;
            let unlisted = |c: &Cell| report.unlisted.iter().any(|u| u.host == host && u.kind == c.spec.kind.name());
            hosts.extend(
                cells
                    .iter()
                    .filter(|c| c.spec.parent == host && c.state == CellState::Running && !unlisted(c) && cells.iter().any(|d| d.spec.parent.as_ref() == Some(&c.spec.name)))
                    .map(|c| Some(c.spec.name.clone())),
            );
        }
        Ok(report)
    }

    async fn reconcile_host(&self, host: Option<&CellName>, report: &mut Report) -> Result<(), Error> {
        let here = |cells: &[Cell]| -> Vec<Cell> { cells.iter().filter(|c| c.spec.parent.as_ref() == host).cloned().collect() };
        let names: Vec<CellName> = here(&self.list()?).into_iter().map(|c| c.spec.name).collect();
        for op in self.ledger.open_ops()?.into_iter().filter(|op| names.contains(&op.name)) {
            self.finish(op, report).await?;
        }
        let cells = self.list()?;
        let mut kinds: BTreeSet<KindName> = here(&cells).iter().map(|c| c.spec.kind.name()).collect();
        if host.is_none() {
            kinds.extend(self.drivers.machine_kinds());
        }
        let on = self.host(&cells, host)?;
        let mut listed = Vec::new();
        let mut failed = Vec::new();
        for kind in kinds {
            match self.drivers.driver(kind, on.clone()) {
                Ok(driver) => match driver.list().await {
                    Ok(cells) => listed.extend(cells),
                    Err(e) => failed.push(Unlisted { host: host.cloned(), kind, error: e.to_string() }),
                },
                Err(e) => failed.push(Unlisted { host: host.cloned(), kind, error: e.to_string() }),
            }
        }
        for cell in here(&cells).into_iter().filter(|c| !failed.iter().any(|u| u.kind == c.spec.kind.name())) {
            let name = &cell.spec.name;
            match listed.iter().find(|l| &l.name == name && l.id.is_none_or(|i| i == cell.id)) {
                // The cells that ran in it went with it.
                None => {
                    self.ledger.forget(name)?;
                    report.removed.push(name.clone());
                }
                Some(l) if l.state != cell.state => self.ledger.set_state(name, l.state)?,
                Some(_) => {}
            }
        }
        let known: Vec<CellName> = self.list()?.into_iter().map(|c| c.spec.name).collect();
        for l in listed {
            if !known.contains(&l.name) && !report.unrecorded.contains(&l.name) {
                report.unrecorded.push(l.name);
            }
        }
        report.unlisted.extend(failed);
        Ok(())
    }

    async fn finish(&self, op: OpenOp, report: &mut Report) -> Result<(), Error> {
        match op.kind {
            OpKind::Create => {
                self.remove_tolerant(&op.name).await?;
                self.ledger.settle(op.id, Outcome::Failed("interrupted".to_owned()))?;
                report.removed.push(op.name);
            }
            OpKind::Remove => {
                self.remove_tolerant(&op.name).await?;
                self.ledger.settle(op.id, Outcome::Done)?;
                report.finished.push(op.name);
            }
            OpKind::Start | OpKind::Stop => self.ledger.settle(op.id, Outcome::Failed("interrupted".to_owned()))?,
        }
        Ok(())
    }

    /// Makes a stopped cell, or a copy of the stopped cell `spec.clone_from`
    /// names. Every rule runs before anything is written. A cell that runs
    /// in another has that cell and the ones above it started first.
    pub async fn create(&self, spec: &CellSpec) -> Result<CellId, Error> {
        let cells = self.list()?;
        crate::policy::check_create(spec, &cells)?;
        let parent_id = spec.parent.as_ref().and_then(|p| find(&cells, p)).map(|c| c.id);
        if let Some(parent) = &spec.parent {
            self.bring_up(parent).await?;
        }
        let driver = self.drivers.driver(spec.kind.name(), self.host(&self.list()?, spec.parent.as_ref())?)?;
        if driver.list().await?.iter().any(|l| l.name == spec.name) {
            return Err(Refusal::new(
                RefusalCode::NameTaken,
                format!("The driver already has a cell named {} that Adytum did not make.", spec.name),
                "Pick another name; Adytum does not touch cells it did not record.",
            )
            .into());
        }
        let (op, id) = self.ledger.intend_create(spec, parent_id)?;
        let made = match &spec.clone_from {
            Some(from) => driver.clone_cell(from, spec, &id).await,
            None => driver.create(spec, &id).await,
        };
        if let Err(e) = made {
            // A name someone took meanwhile is theirs: nothing of ours to clean up.
            let cleaned = match &e {
                Error::Refused(r) if r.code == RefusalCode::NameTaken => Ok(()),
                _ => self.remove_tolerant(&spec.name).await,
            };
            if cleaned.is_ok() {
                self.ledger.settle(op, Outcome::Failed(e.to_string()))?;
            }
            return Err(e);
        }
        self.ledger.settle(op, Outcome::Done)?;
        Ok(id)
    }

    /// Starts the cells it runs in first, outermost first.
    pub async fn start(&self, name: &CellName) -> Result<(), Error> {
        if let Some(parent) = self.get(name)?.spec.parent {
            self.bring_up(&parent).await?;
        }
        self.step(OpKind::Start, name).await
    }

    /// Stops the running cells inside it first, deepest first.
    pub async fn stop(&self, name: &CellName) -> Result<(), Error> {
        self.refresh(name).await?;
        let cells = self.list()?;
        find(&cells, name).ok_or_else(|| Error::NotRecorded(name.clone()))?;
        for inner in descendants(&cells, name).into_iter().filter(|c| c.state == CellState::Running) {
            self.step(OpKind::Stop, &inner.spec.name).await?;
        }
        self.step(OpKind::Stop, name).await
    }

    /// Removes the cells inside it first, deepest first. One whose host is
    /// not running goes with this cell. A cell already gone counts as
    /// removed; one the driver fails to remove is left `Broken`.
    /// A cell inside a stopped one has the cells it runs in started first,
    /// so its own driver can remove it.
    pub async fn remove(&self, name: &CellName) -> Result<(), Error> {
        self.refresh(name).await?;
        if let Some(parent) = self.get(name)?.spec.parent {
            self.bring_up(&parent).await?;
        }
        let cells = self.list()?;
        for inner in descendants(&cells, name) {
            if ancestors(&cells, &inner.spec.name).iter().all(|c| c.state == CellState::Running) {
                self.remove_one(&inner.spec.name).await?;
            }
        }
        self.remove_one(name).await
    }

    /// `name` and the cells it runs in, outermost first, each started unless
    /// it runs already.
    async fn bring_up(&self, name: &CellName) -> Result<(), Error> {
        let cells = self.list()?;
        let me = find(&cells, name).ok_or_else(|| Error::NotRecorded(name.clone()))?;
        let mut chain = ancestors(&cells, name);
        chain.reverse();
        chain.push(me);
        for cell in chain.into_iter().filter(|c| c.state != CellState::Running) {
            self.step(OpKind::Start, &cell.spec.name).await?;
        }
        Ok(())
    }

    async fn step(&self, kind: OpKind, name: &CellName) -> Result<(), Error> {
        let op = self.ledger.intend(kind, name)?;
        let result = match self.driver_of(name) {
            Ok(driver) if kind == OpKind::Start => driver.start(name).await,
            Ok(driver) => driver.stop(name).await,
            Err(e) => Err(e),
        };
        self.settle(op, result)
    }

    async fn remove_one(&self, name: &CellName) -> Result<(), Error> {
        let op = self.ledger.intend(OpKind::Remove, name)?;
        let removed = self.remove_tolerant(name).await;
        self.ledger.settle(op, removed.as_ref().map_or_else(|e| Outcome::Failed(e.to_string()), |()| Outcome::Done))?;
        removed
    }

    fn settle(&self, op: ledger::OpId, result: Result<(), Error>) -> Result<(), Error> {
        let outcome = result.as_ref().map_or_else(|e| Outcome::Failed(e.to_string()), |()| Outcome::Done);
        let settled = self.ledger.settle(op, outcome);
        result?;
        settled
    }

    /// Only the cell's own driver saying the cell is gone counts; a host
    /// that cannot be found is an error, not a removal.
    async fn remove_tolerant(&self, name: &CellName) -> Result<(), Error> {
        let id = self.get(name)?.id;
        match self.driver_of(name)?.remove(name, &id).await {
            Ok(()) => Ok(()),
            Err(Error::NotFound { name: gone }) if gone == name.as_str() => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Asks the hosts of `name`, of the cells it runs in and of the cells
    /// inside it for their states, outermost first, so a stop or a remove
    /// does not act on a state the ledger kept from before. A host that
    /// cannot be asked is left as recorded; reconcile reports it.
    async fn refresh(&self, name: &CellName) -> Result<(), Error> {
        let cells = self.list()?;
        let me = find(&cells, name).ok_or_else(|| Error::NotRecorded(name.clone()))?;
        let mut hosts: Vec<Option<CellName>> = Vec::new();
        let inner = descendants(&cells, name);
        for cell in ancestors(&cells, name).into_iter().rev().chain([me]).chain(inner.into_iter().rev()) {
            if !hosts.contains(&cell.spec.parent) {
                hosts.push(cell.spec.parent.clone());
            }
        }
        for host in hosts {
            let cells = self.list()?;
            if host.as_ref().is_some_and(|h| find(&cells, h).map(|c| c.state) != Some(CellState::Running)) {
                continue;
            }
            let here: Vec<&Cell> = cells.iter().filter(|c| c.spec.parent == host).collect();
            let kinds: BTreeSet<KindName> = here.iter().map(|c| c.spec.kind.name()).collect();
            let Ok(on) = self.host(&cells, host.as_ref()) else { continue };
            for kind in kinds {
                let Ok(listed) = (match self.drivers.driver(kind, on.clone()) {
                    Ok(driver) => driver.list().await,
                    Err(e) => Err(e),
                }) else {
                    continue;
                };
                for cell in here.iter().filter(|c| c.spec.kind.name() == kind) {
                    if let Some(l) = listed.iter().find(|l| l.name == cell.spec.name && l.id.is_none_or(|i| i == cell.id)) {
                        if l.state != cell.state {
                            self.ledger.set_state(&cell.spec.name, l.state)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
