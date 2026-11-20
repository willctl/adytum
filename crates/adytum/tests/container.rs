//! The container driver against a fake runner: what `nerdctl` is asked,
//! what is refused before it runs, and what its answers become.

#![expect(clippy::unwrap_used, reason = "the helpers here fail where they stand, as a test does")]

use std::path::PathBuf;
use std::sync::Arc;

use adytum::container::{parse_ps, Oci, CELL_USER};
use adytum::testing::{failed, ok, FakeRunner};
use adytum::{CellName, CellSpec, CellState, ContainerOptions, Driver, Error, ExecOpts, Kind, Port, RefusalCode, Resources};

const ID: adytum::CellId = adytum::CellId::from_bytes([7; 16]);

const NAME: &str = "adytum-test-box";
const IMAGE: &str = "docker.io/library/alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f";

fn name() -> CellName {
    NAME.parse().unwrap()
}

fn spec() -> CellSpec {
    CellSpec {
        name: name(),
        kind: Kind::Container(ContainerOptions::new(IMAGE.parse().unwrap())),
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

/// nerdctl's `ps` lines, each container made with `ID`.
fn ps(entries: &[(&str, &str)]) -> String {
    entries
        .iter()
        .map(|(n, s)| serde_json::json!({ "Names": n, "Status": s, "Labels": format!("nerdctl/name={n},adytum.cell={ID},adytum.wall=wall_test") }).to_string() + "\n")
        .collect()
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
    oci.create(&spec(), &ID).await.unwrap();
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
            "--label",
            &format!("adytum.cell={ID}"),
            "--init",
            "--user",
            CELL_USER,
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            "4096",
            "--log-driver",
            "none",
            "--cpus",
            "1",
            "--memory",
            "512m",
            "--runtime",
            "io.containerd.runsc.v1",
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
async fn an_open_container_in_a_cell_gets_its_network_and_runc_only_when_asked() {
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    let open = CellSpec { sealed: false, ..spec() };
    oci.create(&open, &ID).await.unwrap();
    let create = &fake.argv()[1];
    assert!(!create.contains(&"--network".to_owned()), "{create:?}");
    let runc = CellSpec { kind: Kind::Container(ContainerOptions { runtime: adytum::Runtime::Runc, ..ContainerOptions::new(IMAGE.parse().unwrap()) }), ..open.clone() };
    oci.create(&runc, &ID).await.unwrap();
    assert!(!fake.argv()[3].contains(&"--runtime".to_owned()), "{:?}", fake.argv()[3]);
    assert!(matches!(oci.create(&CellSpec { sealed: true, ..runc }, &ID).await, Err(Error::Refused(r)) if r.code == RefusalCode::RuncSealed));
}

/// On this machine a network leads to the machine itself: docker's bridge,
/// and Docker Desktop's host.docker.internal.
#[tokio::test]
async fn a_container_on_this_machine_is_sealed_or_refused() {
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    let here = CellSpec { parent: None, sealed: false, ..spec() };
    assert!(matches!(oci.create(&here, &ID).await, Err(Error::Refused(r)) if r.code == RefusalCode::OpenContainerHere));
    let ports = CellSpec { parent: None, ports: vec![Port { guest: 80, host: 18080, bind: [127, 0, 0, 1].into() }], ..spec() };
    assert!(matches!(oci.create(&ports, &ID).await, Err(Error::Unsupported { .. })), "a sealed container has no network to publish from");
    assert!(fake.calls().is_empty(), "{:?}", fake.argv());
    oci.create(&CellSpec { parent: None, ..spec() }, &ID).await.unwrap();
    assert!(fake.argv()[1].windows(2).any(|w| w == ["--network", "none"]));
}

#[tokio::test]
async fn what_a_container_cannot_do_is_refused_before_anything_runs() {
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    let mount = adytum::Mount { source: "/tmp".into(), target: "/t".into(), writable: false };
    let refused = |r: Result<(), Error>| matches!(r, Err(Error::Unsupported { .. }));
    assert!(matches!(oci.create(&CellSpec { parent: None, sealed: false, mounts: vec![mount.clone()], ..spec() }, &ID).await, Err(Error::Refused(_))));
    assert!(refused(oci.create(&CellSpec { network: Some("lan".parse().unwrap()), ..spec() }, &ID).await));
    assert!(refused(oci.create(&CellSpec { clone_from: Some(name()), ..spec() }, &ID).await));
    assert!(refused(oci.create(&CellSpec { kind: Kind::OpenShell, ..spec() }, &ID).await));
    assert!(refused(oci.clone_cell(&name(), &spec(), &ID).await));
    // The host's rules first: a nested cell publishes nothing and mounts nothing.
    let nested_port = CellSpec { ports: vec![Port { guest: 80, host: 8080, bind: [127, 0, 0, 1].into() }], ..spec() };
    assert!(matches!(oci.create(&nested_port, &ID).await, Err(Error::Refused(r)) if r.code == RefusalCode::NestedPort));
    let nested_mount = CellSpec { sealed: false, mounts: vec![mount], ..spec() };
    assert!(matches!(oci.create(&nested_mount, &ID).await, Err(Error::Refused(r)) if r.code == RefusalCode::NestedMount));
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
    // nerdctl exits 1 whatever the command did; its line says what the command did.
    let (oci, _) = driver(FakeRunner::new(|_| failed(1, "said\ntime=\"x\" level=fatal msg=\"exec failed with exit code 7\"\n")));
    let out = oci.exec(&name(), &strs(&["sh"]), ExecOpts::default()).await.unwrap();
    assert_eq!((out.status, out.stderr_lossy().as_str()), (Some(7), "said\n"));
    let duplex = oci.stream(&name(), &strs(&["sh"]), ExecOpts::default()).await.unwrap();
    assert_eq!(duplex.finish().await.unwrap().status, Some(7), "a stream ends the same way");
    // What a command prints is its own, even when it reads like the runtime's.
    let (oci, _) = driver(FakeRunner::new(|_| failed(1, "No such container: whatever\n")));
    let out = oci.exec(&name(), &strs(&["sh"]), ExecOpts::default()).await.unwrap();
    assert_eq!((out.status, out.stderr_lossy().as_str()), (Some(1), "No such container: whatever\n"));
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
    assert!(parse_ps(&nerdctl).unwrap().iter().all(|l| l.id == Some(ID)));
    let label = format!("adytum.cell={ID}");
    let docker = format!(
        "{{\"Names\":\"adytum-test-a\",\"State\":\"running\",\"Status\":\"Up 2 hours\",\"Labels\":\"{label}\"}}\n{{\"Names\":\"adytum-test-b\",\"State\":\"exited\",\"Status\":\"Exited (0)\",\"Labels\":\"{label}\"}}\n"
    );
    let got: Vec<CellState> = parse_ps(&docker).unwrap().into_iter().map(|l| l.state).collect();
    assert_eq!(got, [CellState::Running, CellState::Stopped]);
    // Named like ours, but not made by Adytum: no label, or a label that is not an id.
    let unlabelled = "{\"Names\":\"adytum-test-x\",\"Status\":\"Up\",\"Labels\":\"\"}\n{\"Names\":\"adytum-test-y\",\"Status\":\"Up\",\"Labels\":\"adytum.cell=yes\"}\n";
    assert_eq!(parse_ps(unlabelled).unwrap(), [] as [adytum::Listed; 0]);
    assert!(parse_ps("not json\n").is_err());
    assert_eq!(parse_ps("").unwrap(), [] as [adytum::Listed; 0]);
}

#[tokio::test]
async fn start_and_stop_need_the_container_and_remove_is_forced() {
    let (oci, fake) = driver(FakeRunner::new(|c| if c.argv[1] == "ps" { ok(&ps(&[(NAME, "Created")])) } else { ok("") }));
    oci.start(&name()).await.unwrap();
    oci.stop(&name()).await.unwrap();
    oci.remove(&name(), &ID).await.unwrap();
    let ours: Vec<Vec<String>> = fake.argv().into_iter().filter(|a| a[1] != "ps").collect();
    assert_eq!(ours, [strs(&["nerdctl", "start", NAME]), strs(&["nerdctl", "stop", NAME]), strs(&["nerdctl", "rm", "--force", NAME])]);
    let (oci, fake) = driver(FakeRunner::new(|_| ok("")));
    assert!(matches!(oci.start(&name()).await, Err(Error::NotFound { .. })));
    assert_eq!(fake.argv().len(), 1, "only the listing ran");
    let (oci, _) = driver(FakeRunner::new(|_| failed(1, "Error: No such container: adytum-test-box")));
    assert!(matches!(oci.remove(&name(), &ID).await, Err(Error::Tool { .. })), "a failed listing is not a removal");
}

#[tokio::test]
async fn a_container_made_with_another_id_is_never_removed() {
    let other = adytum::CellId::from_bytes([8; 16]);
    let (oci, fake) = driver(FakeRunner::new(|c| if c.argv[1] == "ps" { ok(&ps(&[(NAME, "Up")])) } else { ok("") }));
    assert!(matches!(oci.remove(&name(), &other).await, Err(Error::NotFound { name }) if name == NAME));
    assert!(fake.argv().iter().all(|a| a[1] != "rm"), "{:?}", fake.argv());
}

#[tokio::test]
async fn a_name_taken_while_the_image_was_pulled_is_a_refusal() {
    let (oci, _) = driver(FakeRunner::new(|c| match c.argv[1].as_str() {
        "create" => failed(1, "time=\"x\" level=fatal msg=\"name \\\"adytum-test-box\\\" is already used by ID \\\"abc\\\"\""),
        _ => ok(""),
    }));
    assert!(matches!(oci.create(&spec(), &ID).await, Err(Error::Refused(r)) if r.code == RefusalCode::NameTaken));
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
