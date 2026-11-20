mod ledger;
mod schema;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ledger::{Ledger, OpKind, Outcome};

use crate::{Cell, CellId, CellName, CellSpec, Driver, Error, Refusal, RefusalCode};

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
}

/// The cells of one Adytum folder: a driver, and a ledger that records each
/// create, start, stop and remove before the driver runs and settles it
/// after, so a crash at any point leaves something [`Cells::reconcile`]
/// can finish. Adytum never touches a cell the ledger did not record.
pub struct Cells {
    ledger: Ledger,
    driver: Arc<dyn Driver>,
}

impl Cells {
    /// Opens `<home>/adytum.sqlite`, making it if there is none.
    pub fn open(home: &Path, driver: Arc<dyn Driver>) -> Result<Cells, Error> {
        Ok(Cells { ledger: Ledger::open(home)?, driver })
    }

    #[cfg(test)]
    fn memory(driver: Arc<dyn Driver>) -> Result<Cells, Error> {
        Ok(Cells { ledger: Ledger::memory()?, driver })
    }

    pub fn driver(&self) -> &Arc<dyn Driver> {
        &self.driver
    }

    pub fn list(&self) -> Result<Vec<Cell>, Error> {
        self.ledger.cells()
    }

    pub fn get(&self, name: &CellName) -> Result<Cell, Error> {
        self.list()?.into_iter().find(|c| &c.spec.name == name).ok_or_else(|| Error::NotRecorded(name.clone()))
    }

    /// Settles every operation a crash left open, then makes the ledger
    /// agree with what the driver lists. Run it before other work: it
    /// treats every open operation as interrupted.
    pub async fn reconcile(&self) -> Result<Report, Error> {
        let mut report = Report::default();
        for op in self.ledger.open_ops()? {
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
        }
        let listed = self.driver.list().await?;
        for cell in self.ledger.cells()? {
            let name = &cell.spec.name;
            match listed.iter().find(|l| &l.name == name) {
                None => {
                    self.ledger.forget(name)?;
                    report.removed.push(name.clone());
                }
                Some(l) if l.state != cell.state => self.ledger.set_state(name, l.state)?,
                Some(_) => {}
            }
        }
        let known: Vec<CellName> = self.ledger.cells()?.into_iter().map(|c| c.spec.name).collect();
        report.unrecorded = listed.into_iter().map(|l| l.name).filter(|n| !known.contains(n)).collect();
        Ok(report)
    }

    /// Makes a stopped cell, or a copy of the stopped cell `spec.clone_from`
    /// names. Every rule runs before anything is written.
    pub async fn create(&self, spec: &CellSpec) -> Result<CellId, Error> {
        crate::policy::check_create(spec, &self.ledger.cells()?)?;
        if self.driver.list().await?.iter().any(|l| l.name == spec.name) {
            return Err(Refusal::new(
                RefusalCode::NameTaken,
                format!("The driver already has a cell named {} that Adytum did not make.", spec.name),
                "Pick another name; Adytum does not touch cells it did not record.",
            )
            .into());
        }
        let op = self.ledger.intend_create(spec)?;
        let made = match &spec.clone_from {
            Some(from) => self.driver.clone_cell(from, spec).await,
            None => self.driver.create(spec).await,
        };
        if let Err(e) = made {
            if self.remove_tolerant(&spec.name).await.is_ok() {
                self.ledger.settle(op, Outcome::Failed(e.to_string()))?;
            }
            return Err(e);
        }
        self.ledger.settle(op, Outcome::Done)?;
        Ok(self.get(&spec.name)?.id)
    }

    pub async fn start(&self, name: &CellName) -> Result<(), Error> {
        let op = self.ledger.intend(OpKind::Start, name)?;
        self.settle(op, self.driver.start(name).await)
    }

    pub async fn stop(&self, name: &CellName) -> Result<(), Error> {
        let op = self.ledger.intend(OpKind::Stop, name)?;
        self.settle(op, self.driver.stop(name).await)
    }

    /// A cell already gone counts as removed; one the driver fails to remove
    /// is left `Broken`.
    pub async fn remove(&self, name: &CellName) -> Result<(), Error> {
        let op = self.ledger.intend(OpKind::Remove, name)?;
        let removed = self.remove_tolerant(name).await;
        self.ledger.settle(op, removed.as_ref().map_or_else(|e| Outcome::Failed(e.to_string()), |()| Outcome::Done))?;
        removed
    }

    /// A held cell is not cloned.
    pub fn hold(&self, name: &CellName, eidolon: &str) -> Result<(), Error> {
        self.ledger.hold(name, eidolon)
    }

    pub fn release(&self, name: &CellName, eidolon: &str) -> Result<(), Error> {
        self.ledger.release(name, eidolon)
    }

    fn settle(&self, op: ledger::OpId, result: Result<(), Error>) -> Result<(), Error> {
        let outcome = result.as_ref().map_or_else(|e| Outcome::Failed(e.to_string()), |()| Outcome::Done);
        let settled = self.ledger.settle(op, outcome);
        result?;
        settled
    }

    async fn remove_tolerant(&self, name: &CellName) -> Result<(), Error> {
        match self.driver.remove(name).await {
            Ok(()) | Err(Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }
}
