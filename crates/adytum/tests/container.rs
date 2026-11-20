//! The container driver against a fake runner: what `nerdctl` is asked,
//! what is refused before it runs, and what its answers become.

#![expect(clippy::unwrap_used, reason = "the helpers here fail where they stand, as a test does")]

use std::path::PathBuf;
use std::sync::Arc;

use adytum::container::{parse_ps, Oci, CELL_USER};
use adytum::testing::{failed, ok, FakeRunner};
use adytum::{CellName, CellSpec, CellState, ContainerOptions, Driver, Error, ExecOpts, Kind, Port, RefusalCode, Resources};

const NAME: &str = "adytum-test-box";
const IMAGE: &str = "docker.io/library/alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f";

fn name() -> CellName {
    NAME.parse().unwrap()
}

fn spec() -> CellSpec {
    CellSpec {
        name: name(),
        kind: Kind::Container(ContainerOptions { image: IMAGE.parse().unwrap() }),
        parent: Some("adytum-test-host".parse().unwrap()),
        wall: "wall_test".parse().unwrap(),
        resources: Resources { cpus: 1, memory_mib: 512, disk_gib: 10 },
        ports: vec![],
        mounts: vec![],
        sealed: true,
        network: None,
        clone_from: None,
    }
}

fn strs(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn driver(fake: FakeRunner) -> (Oci, Arc<FakeRunner>) {
    let fake = Arc::new(fake);
    (Oci::nerdctl(fake.clone()), fake)
}

fn ps(entries: &[(&str, &str)]) -> String {
    entries.iter().map(|(n, s)| serde_json::json!({ "Names": n, "Status": s }).to_string() + "\n").collect()
}

/// A scratch folder for one test, removed when it ends.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Dir {
    let dir = std::env::temp_dir().join(format!("adytum-oci-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Dir(dir)
}

#[tokio::test]
async fn a_container_is_pulled_by_digest_and_made_without_privileges() {
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    oci.create(&spec()).await.unwrap();
    let calls = fake.argv();
    assert_eq!(calls[0], strs(&["nerdctl", "pull", "--quiet", IMAGE]));
    assert_eq!(
        calls[1],
        strs(&[
            "nerdctl",
            "create",
            "--name",
            NAME,
            "--label",
            "adytum.wall=wall_test",
            "--init",
            "--user",
            CELL_USER,
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            "4096",
            "--cpus",
            "1",
            "--memory",
            "512m",
            "--network",
            "none",
            IMAGE,
            "sleep",
            "infinity"
        ])
    );
    assert!(fake.calls().iter().all(|c| c.env.is_empty()), "nothing of this machine's environment goes into the cell");
}

#[tokio::test]
async fn an_open_container_on_this_machine_gets_the_network_and_its_loopback_ports() {
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    let open = CellSpec {
        sealed: false,
        parent: None,
        ports: vec![Port { guest: 80, host: 18080, bind: [127, 0, 0, 1].into() }, Port { guest: 81, host: 18081, bind: "::1".parse().unwrap() }],
        ..spec()
    };
    oci.create(&open).await.unwrap();
    let create = &fake.argv()[1];
    assert!(!create.contains(&"none".to_owned()), "{create:?}");
    let at = create.iter().position(|a| a == "--publish").unwrap();
    assert_eq!(create[at..at + 4], strs(&["--publish", "127.0.0.1:18080:80", "--publish", "[::1]:18081:81"]));
}

#[tokio::test]
async fn what_a_container_cannot_do_is_refused_before_anything_runs() {
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    let mount = adytum::Mount { source: "/tmp".into(), target: "/t".into(), writable: false };
    let refused = |r: Result<(), Error>| matches!(r, Err(Error::Unsupported { .. }));
    assert!(refused(oci.create(&CellSpec { parent: None, sealed: false, mounts: vec![mount.clone()], ..spec() }).await));
    assert!(refused(oci.create(&CellSpec { network: Some("lan".parse().unwrap()), ..spec() }).await));
    assert!(refused(oci.create(&CellSpec { clone_from: Some(name()), ..spec() }).await));
    assert!(refused(oci.create(&CellSpec { kind: Kind::OpenShell, ..spec() }).await));
    assert!(refused(oci.clone_cell(&name(), &spec()).await));
    // The host's rules first: a nested cell publishes nothing and mounts nothing.
    let nested_port = CellSpec { ports: vec![Port { guest: 80, host: 8080, bind: [127, 0, 0, 1].into() }], ..spec() };
    assert!(matches!(oci.create(&nested_port).await, Err(Error::Refused(r)) if r.code == RefusalCode::NestedPort));
    let nested_mount = CellSpec { sealed: false, mounts: vec![mount], ..spec() };
    assert!(matches!(oci.create(&nested_mount).await, Err(Error::Refused(r)) if r.code == RefusalCode::NestedMount));
    assert!(fake.calls().is_empty(), "{:?}", fake.argv());
}

#[tokio::test]
async fn commands_run_as_the_cell_user_or_root_with_variables_as_arguments() {
    let (oci, fake) = driver(FakeRunner::new(|_| ok("hi\n")));
    let out = oci.exec(&name(), &strs(&["id", "-u"]), ExecOpts { env: vec![("A_1".into(), "x y".into())], ..ExecOpts::default() }).await.unwrap();
    assert_eq!(out.stdout_lossy(), "hi\n");
    oci.exec(&name(), &strs(&["cat"]), ExecOpts { stdin: Some(b"in".to_vec()), ..ExecOpts::root() }).await.unwrap();
    let calls = fake.calls();
    assert_eq!(calls[0].argv, strs(&["nerdctl", "exec", "--user", CELL_USER, "-e", "A_1=x y", NAME, "id", "-u"]));
    assert_eq!(calls[1].argv, strs(&["nerdctl", "exec", "-i", "--user", "0:0", NAME, "cat"]));
    assert_eq!(calls[1].stdin, b"in");
    for bad in [vec![], strs(&["a\0b"])] {
        assert!(oci.exec(&name(), &bad, ExecOpts::default()).await.is_err(), "{bad:?}");
    }
    assert!(oci.exec(&name(), &strs(&["true"]), ExecOpts { env: vec![("A-B".into(), "v".into())], ..ExecOpts::default() }).await.is_err());
}

#[tokio::test]
async fn a_failed_command_is_its_output_without_nerdctls_own_line() {
    let (oci, _) =
        driver(FakeRunner::new(|_| failed(1, "dd: can't open '/etc/x': Permission denied\ntime=\"2026-11-20T08:28:56-06:00\" level=fatal msg=\"exec failed with exit code 1\"\n")));
    let out = oci.exec(&name(), &strs(&["dd", "of=/etc/x"]), ExecOpts::default()).await.unwrap();
    assert_eq!((out.status, out.stderr_lossy().as_str()), (Some(1), "dd: can't open '/etc/x': Permission denied\n"));
    let (oci, _) = driver(FakeRunner::new(|_| failed(1, "time=\"x\" level=fatal msg=\"no such container: adytum-test-box\"\n")));
    assert!(matches!(oci.exec(&name(), &strs(&["true"]), ExecOpts::default()).await, Err(Error::NotFound { .. })));
}

#[tokio::test]
async fn a_stream_is_an_interactive_exec() {
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    let duplex = oci.stream(&name(), &strs(&["cat"]), ExecOpts::root()).await.unwrap();
    duplex.finish().await.unwrap();
    assert_eq!(fake.argv()[0], strs(&["nerdctl", "exec", "-i", "--user", "0:0", NAME, "cat"]));
    assert!(matches!(oci.stream(&name(), &strs(&["cat"]), ExecOpts { stdin: Some(vec![]), ..ExecOpts::default() }).await, Err(Error::Unsupported { .. })));
}

#[test]
fn ps_reads_nerdctl_and_docker_and_leaves_out_what_is_not_ours() {
    let nerdctl = ps(&[("adytum-test-a", "Up"), ("adytum-test-b", "Exited (137) 16 seconds ago"), ("adytum-test-c", "Created"), ("someone", "Up"), ("adytum-test-d", "Paused")]);
    let got: Vec<(String, CellState)> = parse_ps(&nerdctl).unwrap().into_iter().map(|l| (l.name.to_string(), l.state)).collect();
    assert_eq!(
        got,
        [
            ("adytum-test-a".to_owned(), CellState::Running),
            ("adytum-test-b".to_owned(), CellState::Stopped),
            ("adytum-test-c".to_owned(), CellState::Stopped),
            ("adytum-test-d".to_owned(), CellState::Broken)
        ]
    );
    let docker = "{\"Names\":\"adytum-test-a\",\"State\":\"running\",\"Status\":\"Up 2 hours\"}\n{\"Names\":\"adytum-test-b\",\"State\":\"exited\",\"Status\":\"Exited (0)\"}\n";
    let got: Vec<CellState> = parse_ps(docker).unwrap().into_iter().map(|l| l.state).collect();
    assert_eq!(got, [CellState::Running, CellState::Stopped]);
    assert!(parse_ps("not json\n").is_err());
    assert_eq!(parse_ps("").unwrap(), [] as [adytum::Listed; 0]);
}

#[tokio::test]
async fn start_and_stop_need_the_container_and_remove_is_forced() {
    let (oci, fake) = driver(FakeRunner::new(|c| if c.argv[1] == "ps" { ok(&ps(&[(NAME, "Created")])) } else { ok("") }));
    oci.start(&name()).await.unwrap();
    oci.stop(&name()).await.unwrap();
    oci.remove(&name()).await.unwrap();
    let ours: Vec<Vec<String>> = fake.argv().into_iter().filter(|a| a[1] != "ps").collect();
    assert_eq!(ours, [strs(&["nerdctl", "start", NAME]), strs(&["nerdctl", "stop", NAME]), strs(&["nerdctl", "rm", "--force", NAME])]);
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    assert!(matches!(oci.start(&name()).await, Err(Error::NotFound { .. })));
    assert_eq!(fake.argv().len(), 1, "only the listing ran");
    let (oci, _) = driver(FakeRunner::new(|_| failed(1, "Error: No such container: adytum-test-box")));
    assert!(matches!(oci.remove(&name()).await, Err(Error::NotFound { .. })));
}

#[tokio::test]
async fn a_copy_in_is_unpacked_in_the_container_by_its_owner() {
    let dir = scratch("in");
    let tree = dir.0.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("a"), b"a").unwrap();
    std::fs::write(dir.0.join("f"), b"file bytes").unwrap();
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    oci.copy_in(&name(), &tree, "/tmp/tree", None).await.unwrap();
    oci.copy_in(&name(), &dir.0.join("f"), "/opt/f", Some(&"root".parse().unwrap())).await.unwrap();
    let as_user = |user: &str, rest: &[&str]| [strs(&["nerdctl", "exec", "-i", "--user", user, NAME]), strs(rest)].concat();
    let calls = fake.calls();
    assert_eq!(
        calls.iter().map(|c| c.argv.clone()).collect::<Vec<_>>(),
        [
            as_user(CELL_USER, &["mkdir", "-p", "--", "/tmp/tree"]),
            as_user(CELL_USER, &["tar", "-x", "-f", "-", "--no-same-owner", "--no-same-permissions", "-C", "/tmp/tree"]),
            as_user("root", &["mkdir", "-p", "--", "/opt"]),
            as_user("root", &["dd", "of=/opt/f", "bs=1M", "status=none"]),
        ]
    );
    assert!(!calls[1].stdin.is_empty(), "the host's tar sent the folder");
    assert_eq!(calls[3].stdin, b"file bytes");
}

#[tokio::test]
async fn a_copy_out_reads_as_the_cell_user_and_lands_at_the_host_path() {
    let dir = scratch("out");
    let (oci, fake) = driver(FakeRunner::new(|c| if c.argv.contains(&"test".to_owned()) { failed(1, "") } else { ok("from the container") }));
    let to = dir.0.join("got.txt");
    oci.copy_out(&name(), "/tmp/f", &to).await.unwrap();
    assert_eq!(std::fs::read_to_string(&to).unwrap(), "from the container");
    let as_cell = |rest: &[&str]| [strs(&["nerdctl", "exec", "-i", "--user", CELL_USER, NAME]), strs(rest)].concat();
    assert_eq!(fake.argv(), [as_cell(&["test", "-d", "/tmp/f"]), as_cell(&["cat", "/tmp/f"])]);
    for bad in ["tmp/f", "/a/../etc", "/a:b"] {
        assert!(matches!(oci.copy_out(&name(), bad, &to).await, Err(Error::BadPath { .. })), "{bad}");
        assert!(matches!(oci.copy_in(&name(), &to, bad, None).await, Err(Error::BadPath { .. })), "{bad}");
    }
    assert!(matches!(oci.copy_out(&name(), "/tmp/f", std::path::Path::new("relative")).await, Err(Error::BadPath { .. })));
}
