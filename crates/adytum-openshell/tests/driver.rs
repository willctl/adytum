//! The driver through a fake runner. The golden transcript is written by
//! hand (no OpenShell was available to record one), so it pins our argument
//! lists, not OpenShell's behaviour.

use std::path::Path;
use std::sync::Arc;

use adytum_driver::{Driver, DriverError, ExecOpts};
use adytum_model::{CellName, CellSpec, CellState, DriverKind, Resources};
use adytum_openshell::{CreateExtras, Host, ImageRef, Label, OpenShell, Profile};
use adytum_testkit::FakeRunner;

type Res = Result<(), Box<dyn std::error::Error>>;

const NAME: &str = "adytum-test-golden";

fn name() -> CellName {
    NAME.parse().expect("a valid name")
}

fn spec() -> CellSpec {
    CellSpec {
        name: name(),
        driver: DriverKind::Docker,
        image: adytum_model::ImagePin::new("https://example.invalid/i.img".into(), format!("sha256:{}", "a".repeat(64)), adytum_model::Arch::Aarch64).expect("a pin"),
        wall: "wall_test".parse().expect("a valid wall"),
        resources: Resources::default(),
        ports: vec![],
        automation_port: None,
        mounts: vec![],
        sealed: true,
        network: None,
        clone_from: None,
        secrets: vec![],
    }
}

fn extras() -> CreateExtras {
    CreateExtras {
        image: ImageRef::new("example.invalid/agent:1").expect("an image"),
        provider: None,
        policy: None,
        label: Some(Label::new("job", NAME).expect("a label")),
        command: vec![],
    }
}

fn driver(fake: &Arc<FakeRunner>) -> OpenShell {
    OpenShell::new(fake.clone(), Profile::Native, Host::new(false, None)).with_extras(extras())
}

#[tokio::test]
async fn the_hand_written_lifecycle_replays() -> Res {
    let fake = Arc::new(FakeRunner::from_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/openshell-0.1.4-by-hand/lifecycle.json"))?);
    let d = driver(&fake);
    assert_eq!(d.list().await?.len(), 0);
    d.create(&spec()).await?;
    d.start(&name()).await?;
    let listed = d.list().await?;
    assert_eq!((listed.len(), listed[0].name.as_str(), listed[0].state), (1, NAME, CellState::Running), "a sandbox of someone else's is left out");
    let opts = ExecOpts { env: vec![("A".into(), "1".into())], stdin: Some(b"in".to_vec()), deadline: Some(std::time::Duration::from_secs(20)), root: false };
    let out = d.exec(&name(), &["echo".into(), "hi there".into()], opts).await?;
    assert_eq!(out.stdout_lossy(), "hi there\n");
    d.copy_in(&name(), Path::new("/tmp/src"), "/workspace").await?;
    d.remove(&name()).await?;
    d.remove(&name()).await?;
    fake.assert_done();
    Ok(())
}

#[tokio::test]
async fn what_openshell_cannot_do_is_refused_before_any_command() -> Res {
    let fake = Arc::new(FakeRunner::from_json(r#"{"tool":"openshell","version":"0","calls":[]}"#)?);
    let d = driver(&fake);
    let refused = |r: Result<(), DriverError>| matches!(r, Err(DriverError::Unsupported { .. }));
    assert!(refused(d.stop(&name()).await));
    assert!(refused(d.copy_out(&name(), "/a", Path::new("/tmp/a")).await));
    assert!(refused(d.clone(&name(), &spec()).await));
    for bad in [
        CellSpec { ports: vec![adytum_model::Port { guest: 1, host: 2, bind: [127, 0, 0, 1].into() }], ..spec() },
        CellSpec { mounts: vec![adytum_model::Mount { source: "/tmp".into(), target: "/t".into(), writable: false }], ..spec() },
        CellSpec { secrets: vec![adytum_model::SecretRef { name: "k".into() }], ..spec() },
        CellSpec { clone_from: Some(name()), ..spec() },
        CellSpec { automation_port: Some(9222), ..spec() },
    ] {
        assert!(refused(d.create(&bad).await));
    }
    // A name past OpenShell's 19 characters, and no image to make it from.
    let long = CellSpec { name: "adytum-test-golden-long".parse()?, ..spec() };
    assert!(refused(d.create(&long).await));
    let bare = OpenShell::new(fake.clone(), Profile::Native, Host::new(false, None));
    assert!(refused(bare.create(&spec()).await));
    assert!(matches!(d.start(&"adytum-nope".parse()?).await, Err(DriverError::Tool { .. } | DriverError::Exec(_))), "a sandbox the fake never heard of fails there");
    Ok(())
}

#[tokio::test]
async fn root_is_refused_before_any_command() -> Res {
    // OpenShell runs a sandbox's commands as the sandbox's own user and has
    // no way to ask for root, so the request is refused rather than run as
    // someone else.
    let fake = Arc::new(FakeRunner::from_json(r#"{"tool":"openshell","version":"0","calls":[]}"#)?);
    let d = driver(&fake);
    let refused = d.exec(&name(), &["id".into()], ExecOpts { root: true, ..ExecOpts::default() }).await;
    assert!(matches!(refused, Err(DriverError::Unsupported { .. })), "{refused:?}");
    fake.assert_done();
    Ok(())
}

#[tokio::test]
async fn a_command_that_would_read_as_a_variable_is_refused() -> Res {
    let fake = Arc::new(FakeRunner::from_json(r#"{"tool":"openshell","version":"0","calls":[]}"#)?);
    let d = driver(&fake);
    let with_env = |k: &str| ExecOpts { env: vec![(k.into(), "v".into())], ..ExecOpts::default() };
    assert!(d.exec(&name(), &[], ExecOpts::default()).await.is_err());
    assert!(d.exec(&name(), &["a".into()], with_env("bad name")).await.is_err());
    assert!(d.exec(&name(), &["a=b".into()], with_env("K")).await.is_err());
    assert!(d.exec(&name(), &["a\0b".into()], ExecOpts::default()).await.is_err());
    fake.assert_done();
    Ok(())
}
