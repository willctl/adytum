//! The OpenShell driver against a fake runner. The argument lists come from
//! OpenShell 0.1.4's help text; no OpenShell was at hand to check them on.

#![expect(clippy::expect_used, reason = "test helpers fail where they stand, as a test does")]

use std::path::Path;
use std::sync::Arc;

use adytum::openshell::{CreateExtras, Distro, Host, ImageRef, Label, OpenShell, Profile};
use adytum::testing::{failed, ok, FakeRunner};
use adytum::{CellName, CellSpec, CellState, Kind, Resources};
use adytum::{Driver, Error, ExecOpts};

type Res = Result<(), Box<dyn std::error::Error>>;

const ID: adytum::CellId = adytum::CellId::from_bytes([7; 16]);

const NAME: &str = "adytum-test-golden";

fn name() -> CellName {
    NAME.parse().expect("a valid name")
}

fn spec() -> CellSpec {
    CellSpec {
        name: name(),
        kind: Kind::OpenShell,
        parent: None,
        wall: "wall_test".parse().expect("a valid wall"),
        resources: Resources::default(),
        ports: vec![],
        mounts: vec![],
        sealed: true,
        network: None,
        clone_from: None,
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
async fn a_lifecycle_asks_openshell_for_each_step() -> Res {
    let fake = Arc::new(FakeRunner::replies(vec![
        ok(r#"{"sandboxes":[]}"#),
        ok(""),
        ok(r#"{"name":"adytum-test-golden","phase":"Ready"}"#),
        ok(r#"{"sandboxes":[{"name":"adytum-test-golden","phase":"Ready"},{"name":"someone-elses","phase":"Ready"}]}"#),
        ok("hi there\n"),
        ok(""),
        ok(""),
        failed(1, "sandbox not found"),
    ]));
    let d = driver(&fake);
    assert_eq!(d.list().await?.len(), 0);
    d.create(&spec(), &ID).await?;
    d.start(&name()).await?;
    let listed = d.list().await?;
    assert_eq!((listed.len(), listed[0].name.as_str(), listed[0].state), (1, NAME, CellState::Running), "a sandbox of someone else's is left out");
    let opts = ExecOpts { env: vec![("A".into(), "1".into())], stdin: Some(b"in".to_vec()), deadline: Some(std::time::Duration::from_secs(20)), root: false };
    assert_eq!(d.exec(&name(), &["echo".into(), "hi there".into()], opts).await?.stdout_lossy(), "hi there\n");
    d.copy_in(&name(), Path::new("/tmp/src"), "/workspace", None).await?;
    d.remove(&name(), &ID).await?;
    d.remove(&name(), &ID).await?;
    let a = |parts: &[&str]| parts.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        fake.argv(),
        [
            a(&["openshell", "sandbox", "list", "-o", "json"]),
            a(&["openshell", "sandbox", "create", "--name", NAME, "--from", "example.invalid/agent:1", "--label", "job=adytum-test-golden", "--detach"]),
            a(&["openshell", "sandbox", "get", NAME, "--output", "json"]),
            a(&["openshell", "sandbox", "list", "-o", "json"]),
            a(&["openshell", "sandbox", "exec", NAME, "--no-login-shell", "--timeout", "20", "--", "env", "A=1", "echo", "hi there"]),
            a(&["openshell", "sandbox", "upload", NAME, "/tmp/src", "/workspace"]),
            a(&["openshell", "sandbox", "delete", NAME]),
            a(&["openshell", "sandbox", "delete", NAME]),
        ]
    );
    assert_eq!(fake.calls()[4].stdin, b"in");
    Ok(())
}

#[tokio::test]
async fn what_openshell_cannot_do_is_refused_before_any_command() -> Res {
    let fake = Arc::new(FakeRunner::new(|_| failed(1, "the sandbox was not found")));
    let d = driver(&fake);
    let refused = |r: Result<(), Error>| matches!(r, Err(Error::Unsupported { .. }));
    assert!(refused(d.stop(&name()).await));
    assert!(refused(d.copy_out(&name(), "/a", Path::new("/tmp/a")).await));
    assert!(refused(d.clone_cell(&name(), &spec(), &ID).await));
    for bad in [
        CellSpec { ports: vec![adytum::Port { guest: 1, host: 2, bind: [127, 0, 0, 1].into() }], ..spec() },
        CellSpec { mounts: vec![adytum::Mount { source: "/tmp".into(), target: "/t".into(), writable: false }], ..spec() },
        CellSpec { clone_from: Some(name()), ..spec() },
    ] {
        assert!(refused(d.create(&bad, &ID).await));
    }
    // A name past OpenShell's 19 characters, and no image to make it from.
    let long = CellSpec { name: "adytum-test-golden-long".parse()?, ..spec() };
    assert!(matches!(d.create(&long, &ID).await, Err(Error::Invalid { .. })));
    let bare = OpenShell::new(fake.clone(), Profile::Native, Host::new(false, None));
    assert!(refused(bare.create(&spec(), &ID).await));
    assert!(matches!(d.start(&"adytum-nope".parse()?).await, Err(Error::NotFound { .. })), "a sandbox OpenShell does not know");
    Ok(())
}

#[tokio::test]
async fn root_and_owners_are_refused_before_any_command() -> Res {
    // OpenShell runs a sandbox's commands and writes its files as the
    // sandbox's own user and has no way to ask for another, so the request
    // is refused rather than run as someone else.
    let fake = Arc::new(FakeRunner::new(|_| failed(1, "the sandbox was not found")));
    let d = driver(&fake);
    let refused = d.exec(&name(), &["id".into()], ExecOpts { root: true, ..ExecOpts::default() }).await;
    assert!(matches!(refused, Err(Error::Unsupported { .. })), "{refused:?}");
    let owned = d.copy_in(&name(), Path::new("/tmp/src"), "/workspace", Some(&"root".parse()?)).await;
    assert!(matches!(owned, Err(Error::Unsupported { .. })), "{owned:?}");
    assert_eq!(fake.calls(), []);
    Ok(())
}

#[tokio::test]
async fn a_command_that_would_read_as_a_variable_is_refused() -> Res {
    let fake = Arc::new(FakeRunner::new(|_| failed(1, "the sandbox was not found")));
    let d = driver(&fake);
    let with_env = |k: &str| ExecOpts { env: vec![(k.into(), "v".into())], ..ExecOpts::default() };
    assert!(d.exec(&name(), &[], ExecOpts::default()).await.is_err());
    assert!(d.exec(&name(), &["a".into()], with_env("bad name")).await.is_err());
    assert!(d.exec(&name(), &["a=b".into()], with_env("K")).await.is_err());
    assert!(d.exec(&name(), &["a\0b".into()], ExecOpts::default()).await.is_err());
    assert_eq!(fake.calls(), []);
    Ok(())
}

#[tokio::test]
async fn under_wsl_a_copy_goes_by_the_distributions_path_for_the_file() -> Res {
    let fake = Arc::new(FakeRunner::replies(vec![ok("/mnt/c/work/src\r\n"), ok("")]));
    let d = OpenShell::new(fake.clone(), Profile::Wsl(Distro::new("Ubuntu")?), Host::new(true, None));
    d.copy_in(&name(), Path::new("C:\\work\\src"), "/workspace", None).await?;
    let argv = fake.argv();
    assert_eq!(argv.len(), 2, "{argv:?}");
    assert!(argv[0].ends_with(&["--distribution".to_owned(), "Ubuntu".into(), "--exec".into(), "wslpath".into(), "-a".into(), "C:\\work\\src".into()]), "{argv:?}");
    assert!(argv[1].ends_with(&["upload".to_owned(), NAME.into(), "/mnt/c/work/src".into(), "/workspace".into()]), "{argv:?}");
    // A relative path is refused before wslpath could read it from another folder.
    let fake = Arc::new(FakeRunner::new(|_| ok("")));
    let d = OpenShell::new(fake.clone(), Profile::Wsl(Distro::new("Ubuntu")?), Host::new(true, None));
    assert!(matches!(d.copy_in(&name(), Path::new("work\\src"), "/workspace", None).await, Err(Error::BadPath { .. })));
    assert_eq!(fake.argv().len(), 0);
    Ok(())
}
