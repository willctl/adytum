//! Findings of a review of the boundaries, each turned into the check that
//! it now holds: what a cell prints is never read as the runtime's word,
//! a container is removed only when it is provably the one made, and a
//! parent that changes while it starts does not adopt the child.

#![expect(clippy::unwrap_used, reason = "the helpers here fail where they stand, as a test does")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use adytum::container::Oci;
use adytum::lima::Lima;
use adytum::testing::{failed, ok, FakeDriver, FakeRunner};
use adytum::*;
use async_trait::async_trait;

const IMAGE: &str = "docker.io/library/alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f";

fn n(s: &str) -> CellName {
    s.parse().unwrap()
}

fn container(name: &str, parent: Option<&str>, wall: &str) -> CellSpec {
    CellSpec {
        name: n(name),
        kind: Kind::Container(ContainerOptions::new(IMAGE.parse().unwrap())),
        parent: parent.map(n),
        wall: wall.parse().unwrap(),
        resources: Resources { cpus: 1, memory_mib: 512, disk_gib: 1 },
        ports: vec![],
        mounts: vec![],
        sealed: true,
        network: None,
        clone_from: None,
    }
}

fn lima(name: &str, wall: &str) -> CellSpec {
    let pin = ImagePin::new("https://example.test/a.img".into(), format!("sha256:{}", "0".repeat(64)), Arch::Aarch64).unwrap();
    CellSpec { kind: Kind::Lima(LimaOptions { containers: true, ..LimaOptions::new(pin) }), parent: None, resources: Resources::default(), ..container(name, None, wall) }
}

/// A scratch folder, removed when the test ends. Declared before the
/// handles that use it, it outlives them, as Windows needs to delete it.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let d = std::env::temp_dir().join(format!("adytum-boundaries-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Scratch(d)
}

#[tokio::test]
async fn a_containers_own_stderr_is_its_output_not_the_cell_gone() {
    let oci = Oci::nerdctl(Arc::new(FakeRunner::new(|_| failed(1, "No such container: whatever\n"))));
    let out = oci.exec(&n("adytum-c"), &["sh".into()], ExecOpts::default()).await.unwrap();
    assert_eq!(out.status, Some(1));
}

// Lima cells need a Mac or Linux host.
#[cfg(unix)]
#[tokio::test]
async fn stderr_through_a_lima_parent_is_never_the_parent_or_the_child_gone() {
    let runner = Arc::new(FakeRunner::new(|_| failed(1, "time=x level=fatal msg=\"snapshot abc does not exist\"\n")));
    let parent = Lima::new(runner, PathBuf::from("limactl"), PathBuf::from("/tmp/adytum-boundaries-lima")).unwrap();
    let host = Host::Cell { name: n("adytum-p"), driver: Arc::new(parent) };
    let oci = Oci::nerdctl(host.runner());
    let out = oci.exec(&n("adytum-c"), &["sh".into()], ExecOpts::default()).await.unwrap();
    assert_eq!(out.status, Some(1));
    let removed = oci.remove(&n("adytum-c"), &CellId::from_bytes([1; 16])).await;
    assert!(matches!(&removed, Err(Error::Tool { .. })), "a listing that failed is not a removal: {removed:?}");
}

struct Docker(Arc<FakeRunner>);

impl Drivers for Docker {
    fn driver(&self, _kind: KindName, _host: Host) -> Result<Arc<dyn Driver>, Error> {
        Ok(Arc::new(Oci::new(self.0.clone(), "docker")))
    }

    fn machine_kinds(&self) -> Vec<KindName> {
        vec![]
    }
}

#[tokio::test]
async fn a_name_taken_during_the_pull_is_refused_and_the_other_container_is_left_alone() {
    let runner = Arc::new(FakeRunner::new(|c| match c.argv[1].as_str() {
        "create" => failed(125, "Error response from daemon: Conflict. The container name \"/adytum-x\" is already in use by container \"abc\"."),
        _ => ok(""),
    }));
    let home = scratch("conflict");
    let cells = Cells::open(&home, Arc::new(Docker(runner.clone()))).unwrap();
    let made = cells.create(&container("adytum-x", None, "wall_a")).await;
    assert!(matches!(&made, Err(Error::Refused(r)) if r.code == RefusalCode::NameTaken), "{made:?}");
    let argv = runner.argv();
    assert!(argv.iter().all(|a| a[1] != "rm"), "{argv:?}");
    assert_eq!(cells.list().unwrap(), [], "nothing recorded");
}

/// Swaps the parent for one behind another wall while it starts.
struct Racing {
    fake: FakeDriver,
    home: PathBuf,
    fired: Arc<AtomicBool>,
}

struct RacingDriver {
    inner: Arc<dyn Driver>,
    fake: FakeDriver,
    home: PathBuf,
    fired: Arc<AtomicBool>,
}

impl Drivers for Racing {
    fn driver(&self, kind: KindName, host: Host) -> Result<Arc<dyn Driver>, Error> {
        Ok(Arc::new(RacingDriver { inner: self.fake.driver(kind, host)?, fake: self.fake.clone(), home: self.home.clone(), fired: self.fired.clone() }))
    }

    fn machine_kinds(&self) -> Vec<KindName> {
        vec![KindName::Lima]
    }
}

#[async_trait]
impl Driver for RacingDriver {
    async fn create(&self, s: &CellSpec, id: &CellId) -> Result<(), Error> {
        self.inner.create(s, id).await
    }
    async fn start(&self, name: &CellName) -> Result<(), Error> {
        if name.as_str() == "adytum-p" && !self.fired.swap(true, Ordering::SeqCst) {
            let other = Cells::open(&self.home, Arc::new(self.fake.clone()))?;
            other.remove(name).await?;
            let mut p = lima("adytum-p", "wall_b");
            p.sealed = false;
            other.create(&p).await?;
        }
        self.inner.start(name).await
    }
    async fn exec(&self, a: &CellName, b: &[String], c: ExecOpts) -> Result<exec::Output, Error> {
        self.inner.exec(a, b, c).await
    }
    async fn stream(&self, a: &CellName, b: &[String], c: ExecOpts) -> Result<exec::Duplex, Error> {
        self.inner.stream(a, b, c).await
    }
    async fn copy_in(&self, a: &CellName, b: &Path, c: &str, d: Option<&GuestUser>) -> Result<(), Error> {
        self.inner.copy_in(a, b, c, d).await
    }
    async fn copy_out(&self, a: &CellName, b: &str, c: &Path) -> Result<(), Error> {
        self.inner.copy_out(a, b, c).await
    }
    async fn stop(&self, a: &CellName) -> Result<(), Error> {
        self.inner.stop(a).await
    }
    async fn remove(&self, a: &CellName, id: &CellId) -> Result<(), Error> {
        self.inner.remove(a, id).await
    }
    async fn list(&self) -> Result<Vec<Listed>, Error> {
        self.inner.list().await
    }
}

#[tokio::test]
async fn a_parent_swapped_while_it_starts_does_not_adopt_the_child() {
    let home = scratch("race");
    let fake = FakeDriver::new();
    let cells = Cells::open(&home, Arc::new(Racing { fake: fake.clone(), home: home.to_path_buf(), fired: Arc::new(AtomicBool::new(false)) })).unwrap();
    cells.create(&lima("adytum-p", "wall_a")).await.unwrap();
    let made = cells.create(&container("adytum-c", Some("adytum-p"), "wall_a")).await;
    assert!(matches!(&made, Err(Error::Refused(r)) if r.code == RefusalCode::ParentWall || r.code == RefusalCode::ParentMissing), "{made:?}");
    assert!(cells.get(&n("adytum-c")).is_err(), "the child was not recorded");
    assert!(fake.state(&n("adytum-c")).is_none(), "the child was not made");
    assert_eq!(cells.get(&n("adytum-p")).unwrap().spec.wall.as_str(), "wall_b");
}
