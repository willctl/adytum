//! Kill tests: an operation is cut at each point a crash could cut it, the
//! ledger is reopened from its file, and `reconcile` must leave the ledger
//! and the driver agreeing with nothing of the cut operation behind.

use std::path::{Path, PathBuf};

use std::sync::Arc;

use super::ledger::{Ledger, OpKind, Outcome};
use super::Cells;
use crate::testing::{FakeDriver, Leaves, Op};
use crate::{Arch, CellName, CellSpec, CellState, DriverKind, ImagePin, Refusal, RefusalCode, Resources};
use crate::{Driver, Error};

type T = Result<(), Box<dyn std::error::Error>>;

fn name(n: &str) -> Result<CellName, Box<dyn std::error::Error>> {
    Ok(n.parse()?)
}

fn spec(n: &str) -> Result<CellSpec, Box<dyn std::error::Error>> {
    Ok(CellSpec {
        name: name(n)?,
        driver: DriverKind::Lima,
        image: ImagePin::new("https://example.test/a.img".into(), format!("sha256:{}", "0".repeat(64)), Arch::Aarch64)?,
        wall: "wall_a".parse()?,
        resources: Resources::default(),
        ports: vec![],
        automation_port: None,
        mounts: vec![],
        sealed: false,
        network: None,
        clone_from: None,
    })
}

/// Where a driver call dies, and what the crash leaves.
type Cut = (Op, Leaves);
const CREATE_BEFORE_TOOL: Cut = (Op::Create, Leaves::Nothing);
const CREATE_HALF_MADE: Cut = (Op::Create, Leaves::Cell(CellState::Broken));
const CREATE_AFTER_MADE: Cut = (Op::Create, Leaves::Done);
const REMOVE_BEFORE_TOOL: Cut = (Op::Remove, Leaves::Nothing);
const REMOVE_STARTED: Cut = (Op::Remove, Leaves::Cell(CellState::Removing));
const REMOVE_AFTER_GONE: Cut = (Op::Remove, Leaves::Done);

fn with_foreign() -> Result<FakeDriver, Box<dyn std::error::Error>> {
    let driver = FakeDriver::new();
    driver.add_cell(&name(FOREIGN)?, CellState::Running);
    Ok(driver)
}

fn scratch(tag: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("adytum-ledger-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The ledger and the driver hold the same cells in the same states and
/// nothing is left open.
fn assert_agree(cells: &Cells, driver: &FakeDriver, foreign: &[&str]) -> T {
    assert!(cells.ledger.open_ops()?.is_empty(), "an operation is still open");
    let ours: Vec<(CellName, CellState)> = cells.list()?.into_iter().map(|c| (c.spec.name, c.state)).collect();
    let theirs: Vec<(CellName, CellState)> = driver.cells().into_iter().filter(|(n, _)| !foreign.contains(&n.as_str())).collect();
    assert_eq!(ours, theirs);
    for f in foreign {
        assert_eq!(driver.state(&name(f)?), Some(CellState::Running), "{f} was touched");
    }
    Ok(())
}

const FOREIGN: &str = "adytum-foreign";

// The op ran, the process died: the ledger handle is dropped unsettled and
// reopened from the same file, as after a restart.
fn restart(dir: &Path, driver: &Arc<FakeDriver>) -> Result<Cells, Box<dyn std::error::Error>> {
    Ok(Cells::open(dir, driver.clone())?)
}

async fn kill_create(cut: Cut, tag: &str) -> T {
    let dir = scratch(tag)?;
    let driver = Arc::new(with_foreign()?);
    let want = spec("adytum-test-c")?;
    {
        let cells = Cells::open(&dir, driver.clone())?;
        driver.fail_next(cut.0, cut.1);
        cells.ledger.intend_create(&want)?;
        assert!(driver.create(&want).await.is_err());
        // dropped without settling
    }
    let cells = restart(&dir, &driver)?;
    assert_eq!(cells.ledger.open_ops()?.len(), 1);
    let report = cells.reconcile().await?;
    assert_eq!(report.removed, vec![want.name.clone()]);
    assert_eq!(report.unrecorded, vec![name(FOREIGN)?]);
    assert!(cells.list()?.is_empty(), "the cut create left a row");
    assert!(driver.state(&want.name).is_none(), "the cut create left a cell");
    assert_agree(&cells, &driver, &[FOREIGN])?;
    // The same name can be made again.
    cells.create(&want).await?;
    assert_agree(&cells, &driver, &[FOREIGN])?;
    assert_eq!(cells.list()?.first().map(|c| c.state), Some(CellState::Stopped));
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[tokio::test]
async fn create_cut_before_the_tool_runs() -> T {
    kill_create(CREATE_BEFORE_TOOL, "c1").await
}

#[tokio::test]
async fn create_cut_with_a_half_made_cell() -> T {
    kill_create(CREATE_HALF_MADE, "c2").await
}

#[tokio::test]
async fn create_cut_after_the_tool_made_the_cell() -> T {
    kill_create(CREATE_AFTER_MADE, "c3").await
}

async fn kill_remove(cut: Cut, tag: &str) -> T {
    let dir = scratch(tag)?;
    let driver = Arc::new(with_foreign()?);
    let cell = spec("adytum-test-r")?;
    {
        let cells = Cells::open(&dir, driver.clone())?;
        cells.create(&cell).await?;
        driver.fail_next(cut.0, cut.1);
        cells.ledger.intend(OpKind::Remove, &cell.name)?;
        assert!(driver.remove(&cell.name).await.is_err());
    }
    let cells = restart(&dir, &driver)?;
    assert_eq!(cells.ledger.open_ops()?.len(), 1);
    let report = cells.reconcile().await?;
    assert_eq!(report.finished, vec![cell.name.clone()]);
    assert!(cells.list()?.is_empty(), "the cut remove left a row");
    assert!(driver.state(&cell.name).is_none(), "the cut remove left a cell");
    assert_agree(&cells, &driver, &[FOREIGN])?;
    cells.create(&cell).await?;
    assert_agree(&cells, &driver, &[FOREIGN])?;
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[tokio::test]
async fn remove_cut_before_the_tool_runs() -> T {
    kill_remove(REMOVE_BEFORE_TOOL, "r1").await
}

#[tokio::test]
async fn remove_cut_after_it_started() -> T {
    kill_remove(REMOVE_STARTED, "r2").await
}

#[tokio::test]
async fn remove_cut_after_the_cell_was_gone() -> T {
    kill_remove(REMOVE_AFTER_GONE, "r3").await
}

#[tokio::test]
async fn a_failed_create_cleans_up_through_the_helper() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    let want = spec("adytum-test-f")?;
    driver.fail_next(CREATE_HALF_MADE.0, CREATE_HALF_MADE.1);
    assert!(matches!(cells.create(&want).await, Err(Error::Tool { .. })));
    assert_agree(&cells, &driver, &[])?;
    cells.create(&want).await?;
    assert_agree(&cells, &driver, &[])?;
    Ok(())
}

#[tokio::test]
async fn a_failed_remove_through_the_helper_marks_the_cell_broken_and_can_be_retried() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    let cell = spec("adytum-test-b")?;
    cells.create(&cell).await?;
    driver.fail_next(REMOVE_BEFORE_TOOL.0, REMOVE_BEFORE_TOOL.1);
    assert!(cells.remove(&cell.name).await.is_err());
    assert_eq!(cells.list()?.first().map(|c| c.state), Some(CellState::Broken));
    cells.remove(&cell.name).await?;
    assert_agree(&cells, &driver, &[])?;
    Ok(())
}

#[tokio::test]
async fn a_ledger_cell_the_driver_lost_loses_its_row_and_a_foreign_cell_is_only_reported() -> T {
    let driver = Arc::new(with_foreign()?);
    let cells = Cells::memory(driver.clone())?;
    let cell = spec("adytum-test-g")?;
    cells.create(&cell).await?;
    driver.lose(&cell.name);
    let report = cells.reconcile().await?;
    assert_eq!(report.removed, vec![cell.name.clone()]);
    assert_eq!(report.unrecorded, vec![name(FOREIGN)?]);
    assert_eq!(cells.list()?.len(), 0);
    assert_agree(&cells, &driver, &[FOREIGN])?;
    Ok(())
}

#[tokio::test]
async fn reconcile_syncs_a_changed_state() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    let cell = spec("adytum-test-s")?;
    cells.create(&cell).await?;
    driver.start(&cell.name).await?;
    cells.reconcile().await?;
    assert_eq!(cells.list()?.first().map(|c| c.state), Some(CellState::Running));
    Ok(())
}

#[tokio::test]
async fn an_interrupted_start_is_settled_and_the_state_follows_the_driver() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    let cell = spec("adytum-test-t")?;
    cells.create(&cell).await?;
    cells.ledger.intend(OpKind::Start, &cell.name)?;
    cells.reconcile().await?;
    assert_eq!(cells.ledger.open_ops()?.len(), 0);
    assert_eq!(cells.list()?.first().map(|c| c.state), Some(CellState::Stopped));
    Ok(())
}

#[tokio::test]
async fn a_cell_the_ledger_never_recorded_is_never_removed_or_replaced() -> T {
    let driver = Arc::new(with_foreign()?);
    let cells = Cells::memory(driver.clone())?;
    // Same name as the foreign cell: refused, and the foreign cell stays.
    let clash = spec(FOREIGN)?;
    match cells.create(&clash).await {
        Err(Error::Refused(r)) => assert_eq!(r.code, RefusalCode::NameTaken),
        other => panic!("{other:?}"),
    }
    assert!(cells.list()?.is_empty() && cells.ledger.open_ops()?.is_empty());
    assert!(matches!(cells.remove(&name(FOREIGN)?).await, Err(Error::NotRecorded(_))));
    assert_agree(&cells, &driver, &[FOREIGN])?;
    Ok(())
}

#[tokio::test]
async fn a_refusal_comes_before_any_intent() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    let mut bad = spec("adytum-test-p")?;
    bad.sealed = true;
    bad.mounts.push(crate::Mount { source: "/tmp".into(), target: "/mnt".into(), writable: false });
    assert!(matches!(cells.create(&bad).await, Err(Error::Refused(_))));
    assert!(cells.ledger.open_ops()?.is_empty() && cells.list()?.is_empty() && driver.cells().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_cell_that_holds_an_eidolon_cannot_be_cloned() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    let base = spec("adytum-test-h")?;
    cells.create(&base).await?;
    cells.hold(&base.name, "eid_one")?;
    let mut copy = spec("adytum-test-h2")?;
    copy.clone_from = Some(base.name.clone());
    match cells.create(&copy).await {
        Err(Error::Refused(r)) => assert_eq!(r.code, RefusalCode::CloneOpenEidolon),
        other => panic!("{other:?}"),
    }
    cells.release(&base.name, "eid_one")?;
    cells.create(&copy).await?;
    Ok(())
}

#[test]
fn a_settled_operation_cannot_be_settled_twice() -> T {
    let ledger = Ledger::memory()?;
    let op = ledger.intend_create(&spec("adytum-test-d")?)?;
    ledger.settle(op, Outcome::Done)?;
    assert!(matches!(ledger.settle(op, Outcome::Done), Err(Error::Ledger(_))));
    Ok(())
}

#[test]
fn a_ledger_with_another_version_is_refused_and_left_alone() -> T {
    let dir = scratch("version")?;
    drop(Ledger::open(&dir)?);
    let file = dir.join(super::ledger::FILE);
    {
        let c = rusqlite::Connection::open(&file)?;
        c.pragma_update(None, "user_version", 12345)?;
    }
    let before = std::fs::read(&file)?;
    match Ledger::open(&dir) {
        Err(Error::SetUpAgain(why)) => assert!(why.contains("set Adytum up again"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&file)?, before);
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn two_handles_on_one_file_share_one_order_of_writes() -> T {
    let dir = scratch("two")?;
    let a = Ledger::open(&dir)?;
    let b = Ledger::open(&dir)?;
    let specs: Vec<CellSpec> = (0..20).map(|i| spec(&format!("adytum-test-w{i}"))).collect::<Result<_, _>>()?;
    std::thread::scope(|s| -> T {
        let ha = s.spawn(|| specs.iter().step_by(2).try_for_each(|sp| a.intend_create(sp).map(drop)));
        let hb = s.spawn(|| specs.iter().skip(1).step_by(2).try_for_each(|sp| b.intend_create(sp).map(drop)));
        ha.join().map_err(|_| "thread a")??;
        hb.join().map_err(|_| "thread b")??;
        Ok(())
    })?;
    assert_eq!(a.cells()?.len(), 20);
    assert_eq!(b.open_ops()?.len(), 20);
    // The same name through the other handle is refused, not doubled.
    assert!(matches!(b.intend_create(&specs[0]), Err(Error::Refused(Refusal { code: RefusalCode::NameTaken, .. }))));
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

const HELPER_ENV: &str = "ADYTUM_LEDGER_HELPER";

/// Runs only as the child of `a_real_process_killed_after_intend_is_reconciled`.
#[test]
fn helper() -> T {
    let Ok(dir) = std::env::var(HELPER_ENV) else { return Ok(()) };
    let ledger = Ledger::open(Path::new(&dir))?;
    ledger.intend_create(&spec("adytum-test-kill")?)?;
    std::process::abort();
}

#[tokio::test]
async fn a_real_process_killed_after_intend_is_reconciled() -> T {
    let dir = scratch("proc")?;
    let status = std::process::Command::new(std::env::current_exe()?).args(["--exact", "cells::tests::helper", "--nocapture"]).env(HELPER_ENV, &dir).status()?;
    assert!(!status.success(), "the helper should have aborted");
    // The cell the dead process may have made: the driver has it half-made.
    let driver = Arc::new(with_foreign()?);
    driver.add_cell(&name("adytum-test-kill")?, CellState::Broken);
    let cells = Cells::open(&dir, driver.clone())?;
    assert_eq!(cells.ledger.open_ops()?.len(), 1, "the intent was written before the crash");
    let report = cells.reconcile().await?;
    assert_eq!(report.removed, vec![name("adytum-test-kill")?]);
    assert_agree(&cells, &driver, &[FOREIGN])?;
    cells.create(&spec("adytum-test-kill")?).await?;
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
