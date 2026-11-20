//! Kill tests: an operation is cut at each point a crash could cut it, the
//! ledger is reopened from its file, and `reconcile` must leave the ledger
//! and the driver agreeing with nothing of the cut operation behind.

use std::path::{Path, PathBuf};

use std::sync::Arc;

use super::ledger::{Ledger, OpKind, Outcome};
use super::Cells;
use crate::testing::{FakeDriver, Leaves, Op};
use crate::{Arch, CellName, CellSpec, CellState, ImagePin, Kind, LimaOptions, Refusal, RefusalCode, Resources};
use crate::{Driver, Error};

type T = Result<(), Box<dyn std::error::Error>>;

fn name(n: &str) -> Result<CellName, Box<dyn std::error::Error>> {
    Ok(n.parse()?)
}

fn spec(n: &str) -> Result<CellSpec, Box<dyn std::error::Error>> {
    Ok(CellSpec {
        name: name(n)?,
        kind: Kind::Lima(LimaOptions::new(ImagePin::new("https://example.test/a.img".into(), format!("sha256:{}", "0".repeat(64)), Arch::Aarch64)?)),
        parent: None,
        wall: "wall_a".parse()?,
        resources: Resources::default(),
        ports: vec![],
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
        Err(Error::SetUpAgain(why)) => assert!(why.contains("left as it is"), "{why}"),
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

// ---- cells in cells ----------------------------------------------------------

fn host_spec(n: &str) -> Result<CellSpec, Box<dyn std::error::Error>> {
    let mut s = spec(n)?;
    if let Kind::Lima(options) = &mut s.kind {
        options.containers = true;
    }
    Ok(s)
}

fn child_spec(n: &str, parent: &str) -> Result<CellSpec, Box<dyn std::error::Error>> {
    let image = format!("docker.io/library/alpine@sha256:{}", "0".repeat(64)).parse()?;
    Ok(CellSpec { kind: Kind::Container(crate::ContainerOptions { image }), parent: Some(name(parent)?), ..spec(n)? })
}

fn lines(driver: &FakeDriver) -> Vec<String> {
    driver.calls().iter().map(ToString::to_string).collect()
}

fn state_of(cells: &Cells, n: &str) -> Option<CellState> {
    cells.list().ok()?.into_iter().find(|c| c.spec.name.as_str() == n).map(|c| c.state)
}

/// A host with one container in it, both made through `Cells`.
async fn nest(cells: &Cells) -> T {
    cells.create(&host_spec("adytum-test-p")?).await?;
    cells.create(&child_spec("adytum-test-c", "adytum-test-p")?).await?;
    Ok(())
}

#[tokio::test]
async fn creating_a_cell_in_a_stopped_parent_starts_the_parent_first() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    assert_eq!(lines(&driver), ["create adytum-test-p", "start adytum-test-p", "create adytum-test-c"]);
    assert_eq!((state_of(&cells, "adytum-test-p"), state_of(&cells, "adytum-test-c")), (Some(CellState::Running), Some(CellState::Stopped)));
    assert_eq!(cells.get(&name("adytum-test-c")?)?.spec.parent, Some(name("adytum-test-p")?));
    assert_agree(&cells, &driver, &[])
}

#[tokio::test]
async fn starting_a_cell_starts_what_it_runs_in_first() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    cells.stop(&name("adytum-test-p")?).await?;
    let before = driver.calls().len();
    cells.start(&name("adytum-test-c")?).await?;
    assert_eq!(lines(&driver)[before..], ["start adytum-test-p", "start adytum-test-c"]);
    // A parent already running is not started again.
    cells.stop(&name("adytum-test-c")?).await?;
    let before = driver.calls().len();
    cells.start(&name("adytum-test-c")?).await?;
    assert_eq!(lines(&driver)[before..], ["start adytum-test-c"]);
    assert_agree(&cells, &driver, &[])
}

#[tokio::test]
async fn stopping_a_parent_stops_its_running_cells_first() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    cells.start(&name("adytum-test-c")?).await?;
    let before = driver.calls().len();
    cells.stop(&name("adytum-test-p")?).await?;
    assert_eq!(lines(&driver)[before..], ["stop adytum-test-c", "stop adytum-test-p"]);
    assert_eq!(state_of(&cells, "adytum-test-c"), Some(CellState::Stopped));
    assert_agree(&cells, &driver, &[])
}

#[tokio::test]
async fn removing_a_running_parent_removes_its_cells_first_through_the_ledger() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    let before = driver.calls().len();
    cells.remove(&name("adytum-test-p")?).await?;
    assert_eq!(lines(&driver)[before..], ["remove adytum-test-c", "remove adytum-test-p"]);
    assert!(cells.list()?.is_empty() && driver.cells().is_empty());
    assert_agree(&cells, &driver, &[])
}

#[tokio::test]
async fn the_cells_in_a_stopped_parent_go_with_it() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    cells.stop(&name("adytum-test-p")?).await?;
    let before = driver.calls().len();
    cells.remove(&name("adytum-test-p")?).await?;
    assert_eq!(lines(&driver)[before..], ["remove adytum-test-p"], "its host is down, so the child is not asked for");
    assert!(cells.list()?.is_empty(), "the child's row went with its parent's");
    assert_agree(&cells, &driver, &[])
}

#[tokio::test]
async fn a_failed_remove_inside_stops_the_parents_remove() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    driver.fail_next(Op::Remove, Leaves::Nothing);
    assert!(cells.remove(&name("adytum-test-p")?).await.is_err());
    assert_eq!((state_of(&cells, "adytum-test-p"), state_of(&cells, "adytum-test-c")), (Some(CellState::Running), Some(CellState::Broken)));
    cells.remove(&name("adytum-test-p")?).await?;
    assert_eq!(cells.list()?, []);
    Ok(())
}

#[tokio::test]
async fn reconcile_leaves_the_cells_of_a_stopped_parent_as_recorded() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    cells.stop(&name("adytum-test-p")?).await?;
    // Even a child the driver no longer has: its host cannot be asked.
    driver.lose(&name("adytum-test-c")?);
    let report = cells.reconcile().await?;
    assert_eq!(report, super::Report::default());
    assert_eq!(state_of(&cells, "adytum-test-c"), Some(CellState::Stopped));
    Ok(())
}

#[tokio::test]
async fn reconcile_asks_a_running_parent_for_its_cells() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    driver.start(&name("adytum-test-c")?).await?;
    cells.reconcile().await?;
    assert_eq!(state_of(&cells, "adytum-test-c"), Some(CellState::Running), "the state the host gives");
    driver.lose(&name("adytum-test-c")?);
    let report = cells.reconcile().await?;
    assert_eq!(report.removed, [name("adytum-test-c")?]);
    assert_eq!(cells.list()?.len(), 1);
    assert_agree(&cells, &driver, &[])
}

#[tokio::test]
async fn a_parent_that_vanished_takes_its_records_along() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    driver.lose(&name("adytum-test-p")?);
    let report = cells.reconcile().await?;
    assert_eq!(report.removed, [name("adytum-test-p")?]);
    assert_eq!(cells.list()?, []);
    Ok(())
}

#[tokio::test]
async fn a_create_cut_inside_a_parent_is_removed_through_the_parent() -> T {
    let dir = scratch("nest-cut")?;
    let driver = Arc::new(FakeDriver::new());
    let child = child_spec("adytum-test-c", "adytum-test-p")?;
    {
        let cells = Cells::open(&dir, driver.clone())?;
        cells.create(&host_spec("adytum-test-p")?).await?;
        cells.start(&name("adytum-test-p")?).await?;
        cells.ledger.intend_create(&child)?;
        driver.fail_next(Op::Create, Leaves::Cell(CellState::Broken));
        let inside = cells.driver_of(&child.name)?;
        assert!(inside.create(&child).await.is_err());
    }
    let cells = restart(&dir, &driver)?;
    let report = cells.reconcile().await?;
    assert_eq!(report.removed, std::slice::from_ref(&child.name));
    assert!(lines(&driver).last().is_some_and(|l| l == "remove adytum-test-c"), "{:?}", lines(&driver));
    assert_eq!(cells.list()?.len(), 1);
    assert_agree(&cells, &driver, &[])?;
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[tokio::test]
async fn an_operation_left_open_on_a_cell_that_went_with_its_parent_is_settled() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    cells.ledger.intend(OpKind::Start, &name("adytum-test-c")?)?;
    cells.stop(&name("adytum-test-c")?).await.ok();
    // The parent is removed while the child's start is still open.
    cells.stop(&name("adytum-test-p")?).await?;
    cells.remove(&name("adytum-test-p")?).await?;
    assert_eq!(cells.ledger.open_ops()?.len(), 1);
    cells.reconcile().await?;
    assert_eq!(cells.ledger.open_ops()?, []);
    Ok(())
}

#[tokio::test]
async fn a_foreign_cell_inside_a_parent_is_not_replaced() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    cells.create(&host_spec("adytum-test-p")?).await?;
    cells.start(&name("adytum-test-p")?).await?;
    // Made in the parent by someone else, so its listing shows it.
    driver.create(&child_spec("adytum-test-c", "adytum-test-p")?).await?;
    match cells.create(&child_spec("adytum-test-c", "adytum-test-p")?).await {
        Err(Error::Refused(r)) => assert_eq!(r.code, RefusalCode::NameTaken),
        other => panic!("{other:?}"),
    }
    let report = cells.reconcile().await?;
    assert!(report.unrecorded.is_empty(), "a parent with no recorded cells in it is not listed");
    Ok(())
}

#[tokio::test]
async fn a_driver_inside_a_stopped_parent_cannot_be_reached() -> T {
    let driver = Arc::new(FakeDriver::new());
    let cells = Cells::memory(driver.clone())?;
    nest(&cells).await?;
    driver.stop(&name("adytum-test-p")?).await?;
    let inside = cells.driver_of(&name("adytum-test-c")?)?;
    assert!(matches!(inside.exec(&name("adytum-test-c")?, &["true".into()], crate::ExecOpts::default()).await, Err(Error::Tool { .. })));
    assert!(matches!(cells.driver_of(&name("adytum-test-x")?), Err(Error::NotRecorded(_))));
    Ok(())
}

#[test]
fn a_ledger_this_adytum_cannot_read_says_how_to_start_over() {
    let fix = Error::SetUpAgain("x".to_owned()).fix().to_owned();
    assert!(fix.contains("limactl delete") && fix.contains("move adytum.sqlite"), "{fix}");
}
