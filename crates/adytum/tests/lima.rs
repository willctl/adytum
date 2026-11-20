//! The Lima driver against a fake runner: what limactl is asked, what is
//! refused before it runs, and what its failures become.

#![expect(clippy::unwrap_used, reason = "the helpers here fail where they stand, as a test does")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adytum::exec::Output;
use adytum::lima::Lima;
use adytum::testing::{failed, ok, FakeRunner};
use adytum::{CellName, CellSpec, CellState, Driver, DriverKind, Error, ExecOpts, Port, RefusalCode, Resources};

const NAME: &str = "adytum-test-golden";

fn name() -> CellName {
    NAME.parse().unwrap()
}

fn spec() -> CellSpec {
    CellSpec {
        name: name(),
        driver: DriverKind::Lima,
        image: adytum::lima::image(adytum::lima::DEFAULT_IMAGE).unwrap(),
        wall: "wall_test".parse().unwrap(),
        resources: Resources::default(),
        ports: vec![],
        automation_port: None,
        mounts: vec![],
        sealed: true,
        network: None,
        clone_from: None,
    }
}

fn clone_spec() -> CellSpec {
    CellSpec { name: "adytum-test-golden-clone".parse().unwrap(), clone_from: Some(name()), ports: vec![Port { guest: 8080, host: 18080, bind: [127, 0, 0, 1].into() }], ..spec() }
}

/// A scratch folder for one test, removed when it ends. Short, because Lima
/// keeps unix sockets under it.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Dir {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = PathBuf::from("/tmp").join(format!("adl-f-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Dir(dir)
}

fn lima(fake: FakeRunner) -> (Lima, Arc<FakeRunner>, Dir) {
    let home = scratch();
    let fake = Arc::new(fake);
    (Lima::new(fake.clone(), "limactl".into(), home.0.clone()).unwrap(), fake, home)
}

fn listed(cells: &[(&str, &str)]) -> Output {
    ok(&cells.iter().map(|(n, s)| format!("{{\"name\":\"{n}\",\"status\":\"{s}\"}}\n")).collect::<Vec<_>>().concat())
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn shell(rest: &[&str]) -> Vec<String> {
    argv(&["limactl", "shell", "--workdir=/", NAME, "--"]).into_iter().chain(argv(rest)).collect()
}

fn as_cell(rest: &[&str]) -> Vec<String> {
    shell(&[&["sudo", "-n", "-u", "cell", "-H", "--"][..], rest].concat())
}

#[tokio::test]
async fn list_leaves_out_instances_that_are_not_ours() {
    let (lima, _, _home) = lima(FakeRunner::replies(vec![listed(&[("pane", "Running"), ("adytum-test-a", "Running"), ("xenonet-vm1", "Stopped"), ("adytum-test-b", "Stopped")])]));
    let cells: Vec<(String, CellState)> = lima.list().await.unwrap().into_iter().map(|c| (c.name.to_string(), c.state)).collect();
    assert_eq!(cells, [("adytum-test-a".to_owned(), CellState::Running), ("adytum-test-b".to_owned(), CellState::Stopped)]);
}

#[tokio::test]
async fn limactl_gets_only_lima_home_path_and_home() {
    let (lima, fake, home) = lima(FakeRunner::new(|c| if c.argv[1] == "list" { listed(&[(NAME, "Stopped")]) } else { ok("") }));
    lima.start(&name()).await.unwrap();
    lima.stop(&name()).await.unwrap();
    lima.remove(&name()).await.unwrap();
    for call in fake.calls() {
        assert_eq!(call.env.keys().filter(|k| !["PATH", "HOME"].contains(&k.as_str())).collect::<Vec<_>>(), ["LIMA_HOME"], "{:?}", call.argv);
        assert_eq!(call.env["LIMA_HOME"], home.0.to_string_lossy());
    }
}

#[tokio::test]
async fn a_failing_create_surfaces_its_stderr_and_leaves_no_template() {
    let (lima, fake, home) = lima(FakeRunner::replies(vec![failed(1, "level=fatal msg=\"disk is full\"\n")]));
    let err = lima.create(&spec()).await.unwrap_err();
    assert!(matches!(err, Error::Tool { status: Some(1), .. }) && err.to_string().contains("disk is full"), "{err}");
    let template = home.0.join(format!("_templates/{NAME}.yaml"));
    assert_eq!(fake.argv(), [argv(&["limactl", "create", &format!("--name={NAME}"), "--tty=false", &template.to_string_lossy()])]);
    assert!(!template.exists());
}

#[tokio::test]
async fn a_missing_cell_is_not_found_and_limactl_start_never_runs() {
    let (lima, fake, _home) = lima(FakeRunner::new(|_| ok("")));
    assert!(matches!(lima.start(&name()).await, Err(Error::NotFound { .. })));
    assert!(matches!(lima.stop(&name()).await, Err(Error::NotFound { .. })));
    assert!(fake.argv().iter().all(|a| a[1] == "list"), "{:?}", fake.argv());
}

const LATE: &str = "level=fatal msg=\"hostagent (\\\"/h/adytum-test-golden/ha.pid\\\") did not start up in 5s\"\n";

fn start_calls(starts: usize) -> Vec<Vec<String>> {
    let (list, start, stop) = (argv(&["limactl", "list", "--json"]), argv(&["limactl", "start", "--tty=false", NAME]), argv(&["limactl", "stop", "--force", "--tty=false", NAME]));
    let mut calls = vec![list, start.clone()];
    if starts == 2 {
        calls.extend([stop, start]);
    }
    calls
}

#[tokio::test]
async fn a_start_whose_host_agent_was_late_is_stopped_and_tried_once_more() {
    let (lima, fake, _home) = lima(FakeRunner::replies(vec![listed(&[(NAME, "Stopped")]), failed(1, LATE), ok(""), ok("")]));
    lima.start(&name()).await.unwrap();
    assert_eq!(fake.argv(), start_calls(2));
}

#[tokio::test]
async fn a_host_agent_late_twice_is_a_clear_error() {
    let (lima, fake, _home) = lima(FakeRunner::replies(vec![listed(&[(NAME, "Stopped")]), failed(1, LATE), ok(""), failed(1, LATE)]));
    let err = lima.start(&name()).await.unwrap_err();
    assert!(matches!(&err, Error::Tool { .. }) && err.to_string().contains("tried twice"), "{err}");
    assert_eq!(fake.argv(), start_calls(2));
}

#[tokio::test]
async fn any_other_start_failure_is_not_retried() {
    let (lima, fake, _home) = lima(FakeRunner::replies(vec![listed(&[(NAME, "Stopped")]), failed(1, "level=fatal msg=\"disk full\"\n")]));
    assert!(lima.start(&name()).await.unwrap_err().to_string().contains("disk full"));
    assert_eq!(fake.argv(), start_calls(1));
}

#[tokio::test]
async fn exec_hands_each_argument_over_whole_as_the_cell_user() {
    let (lima, fake, _home) = lima(FakeRunner::replies(vec![ok("done")]));
    let out = lima.exec(&name(), &argv(&["printf", "%s\n", "$(id)", "a b", ""]), ExecOpts::default()).await.unwrap();
    assert_eq!(out.stdout_lossy(), "done");
    assert_eq!(fake.argv(), [as_cell(&["printf", "%s\n", "$(id)", "a b", ""])]);
}

#[tokio::test]
async fn root_runs_as_root_only_when_the_options_ask() {
    let (lima, fake, _home) = lima(FakeRunner::new(|_| ok("")));
    lima.exec(&name(), &argv(&["apt-get", "update"]), ExecOpts { root: true, ..ExecOpts::default() }).await.unwrap();
    lima.exec(&name(), &argv(&["id"]), ExecOpts { root: true, env: vec![("K".into(), "v".into())], ..ExecOpts::default() }).await.unwrap();
    lima.exec(&name(), &argv(&["id"]), ExecOpts::default()).await.unwrap();
    assert_eq!(fake.argv(), [shell(&["sudo", "-n", "--", "apt-get", "update"]), shell(&["sudo", "-n", "--", "env", "K=v", "id"]), as_cell(&["id"])]);
}

#[tokio::test]
async fn variables_go_through_env_and_a_bad_command_never_reaches_limactl() {
    let (lima, fake, _home) = lima(FakeRunner::new(|_| ok("")));
    lima.exec(&name(), &argv(&["sh", "-c", "true"]), ExecOpts { env: vec![("A".into(), "1 2".into())], ..ExecOpts::default() }).await.unwrap();
    let with = |k: &str| ExecOpts { env: vec![(k.into(), "x".into())], ..ExecOpts::default() };
    for (cmd, opts) in
        [(argv(&["true"]), with("A B")), (argv(&["B=1"]), with("A")), (vec![], ExecOpts::default()), (argv(&["a\0b"]), ExecOpts { root: true, ..ExecOpts::default() })]
    {
        assert!(matches!(lima.exec(&name(), &cmd, opts).await, Err(Error::Unsupported { .. })), "{cmd:?}");
    }
    assert_eq!(fake.argv(), [as_cell(&["env", "A=1 2", "sh", "-c", "true"])]);
}

#[tokio::test]
async fn a_missing_instance_is_not_found_from_exec_and_copy_in() {
    let missing = "level=fatal msg=\"instance `adytum-test-golden` does not exist, run `limactl create adytum-test-golden` to create a new instance\"\n";
    let (lima, _, home) = lima(FakeRunner::new(move |_| failed(1, missing)));
    std::fs::write(home.0.join("x"), b"x").unwrap();
    assert!(matches!(lima.exec(&name(), &argv(&["true"]), ExecOpts::default()).await, Err(Error::NotFound { .. })));
    assert!(matches!(lima.copy_in(&name(), &home.0.join("x"), "/tmp/x", None).await, Err(Error::NotFound { .. })));
}

#[tokio::test]
async fn a_file_is_written_by_the_cell_user_so_it_owns_it() {
    let (lima, fake, home) = lima(FakeRunner::new(|_| ok("")));
    let file = home.0.join("a.bin");
    std::fs::write(&file, b"\x00bytes\xff").unwrap();
    lima.copy_in(&name(), &file, "/home/cell/in/a.bin", None).await.unwrap();
    let calls = fake.calls();
    assert_eq!(
        calls.iter().map(|c| c.argv.clone()).collect::<Vec<_>>(),
        [as_cell(&["mkdir", "-p", "--", "/home/cell/in"]), as_cell(&["dd", "of=/home/cell/in/a.bin", "bs=1M", "status=none"])]
    );
    assert_eq!(calls[1].stdin, b"\x00bytes\xff");
}

#[tokio::test]
async fn a_folder_goes_as_tar_of_its_entries_and_the_cell_user_unpacks_it() {
    let (lima, fake, home) = lima(FakeRunner::new(|c| if c.argv[0] == "tar" { ok("TAR BYTES") } else { ok("") }));
    let tree = home.0.join("tree");
    std::fs::create_dir_all(tree.join("b")).unwrap();
    std::fs::write(tree.join("-a"), b"a").unwrap();
    lima.copy_in(&name(), &tree, "/tmp/tree", None).await.unwrap();
    let calls = fake.calls();
    let tar = argv(&["tar", "-c", "-f", "-", "--no-xattrs", "--no-acls", "-C", &tree.to_string_lossy(), "./-a", "./b"]);
    assert_eq!(
        calls.iter().map(|c| c.argv.clone()).collect::<Vec<_>>(),
        [as_cell(&["mkdir", "-p", "--", "/tmp/tree"]), as_cell(&["tar", "-x", "-f", "-", "--no-same-owner", "--no-same-permissions", "-C", "/tmp/tree"]), tar]
    );
    assert_eq!(calls[1].stdin, b"TAR BYTES");
    assert_eq!(calls[2].env.get("COPYFILE_DISABLE").map(String::as_str), Some("1"));
}

#[tokio::test]
async fn a_copy_with_an_owner_is_made_and_unpacked_by_that_user() {
    let (lima, fake, home) = lima(FakeRunner::new(|c| if c.argv[0] == "tar" { ok("TAR BYTES") } else { ok("") }));
    let tree = home.0.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("a"), b"a").unwrap();
    let file = home.0.join("f");
    std::fs::write(&file, b"f").unwrap();
    let root: adytum::GuestUser = "root".parse().unwrap();
    lima.copy_in(&name(), &tree, "/opt/tree", Some(&root)).await.unwrap();
    lima.copy_in(&name(), &file, "/home/eidolon/f", Some(&"eidolon".parse().unwrap())).await.unwrap();
    let as_user = |user: &str, rest: &[&str]| shell(&[&["sudo", "-n", "-u", user, "-H", "--"][..], rest].concat());
    let guest_calls: Vec<Vec<String>> = fake.argv().into_iter().filter(|a| a[0] == "limactl").collect();
    assert_eq!(
        guest_calls,
        [
            as_user("root", &["mkdir", "-p", "--", "/opt/tree"]),
            as_user("root", &["tar", "-x", "-f", "-", "--no-same-owner", "--no-same-permissions", "-C", "/opt/tree"]),
            as_user("eidolon", &["mkdir", "-p", "--", "/home/eidolon"]),
            as_user("eidolon", &["dd", "of=/home/eidolon/f", "bs=1M", "status=none"]),
        ]
    );
}

#[tokio::test]
async fn an_empty_folder_is_only_made() {
    let (lima, fake, home) = lima(FakeRunner::new(|_| ok("")));
    std::fs::create_dir_all(home.0.join("empty")).unwrap();
    lima.copy_in(&name(), &home.0.join("empty"), "/tmp/empty", None).await.unwrap();
    assert_eq!(fake.argv(), [as_cell(&["mkdir", "-p", "--", "/tmp/empty"])]);
}

#[tokio::test]
async fn a_copy_the_cell_user_may_not_make_is_a_tool_failure_with_its_stderr() {
    let (lima, _, home) = lima(FakeRunner::new(|c| if c.argv.contains(&"dd".to_owned()) { failed(1, "dd: failed to open '/etc/x': Permission denied") } else { ok("") }));
    std::fs::write(home.0.join("x"), b"x").unwrap();
    let err = lima.copy_in(&name(), &home.0.join("x"), "/etc/x", None).await.unwrap_err();
    assert!(matches!(&err, Error::Tool { .. }) && err.to_string().contains("Permission denied"), "{err}");
}

#[tokio::test]
async fn copy_paths_are_checked_before_any_call() {
    let (lima, fake, _home) = lima(FakeRunner::new(|_| ok("")));
    for guest in ["tmp/x", "/tmp/../etc/passwd", "/a:b", ""] {
        assert!(matches!(lima.copy_in(&name(), Path::new("/etc/hosts"), guest, None).await, Err(Error::BadPath { .. })), "{guest}");
        assert!(matches!(lima.copy_out(&name(), guest, Path::new("/tmp/out")).await, Err(Error::BadPath { .. })), "{guest}");
    }
    assert!(matches!(lima.copy_in(&name(), Path::new("relative.txt"), "/tmp/x", None).await, Err(Error::BadPath { .. })));
    for host in ["relative.txt", "/tmp/a:b"] {
        assert!(matches!(lima.copy_out(&name(), "/tmp/x", Path::new(host)).await, Err(Error::BadPath { .. })), "{host}");
    }
    assert_eq!(fake.calls(), []);
}

#[tokio::test]
async fn a_spec_that_breaks_a_host_rule_never_reaches_limactl() {
    let (lima, fake, _home) = lima(FakeRunner::new(|_| ok("")));
    let mut sealed = spec();
    sealed.mounts.push(adytum::Mount { source: "/Users".into(), target: "/mnt".into(), writable: true });
    assert!(matches!(lima.create(&sealed).await, Err(Error::Refused(r)) if r.code == RefusalCode::SealedCellMount));
    let mut to = clone_spec();
    to.ports[0].bind = "0.0.0.0".parse().unwrap();
    assert!(matches!(lima.clone_cell(&name(), &to).await, Err(Error::Refused(r)) if r.code == RefusalCode::PortNotLocalhost));
    assert_eq!(fake.calls(), []);
}

#[tokio::test]
async fn a_home_too_long_for_lima_sockets_or_not_absolute_is_refused() {
    let fake = Arc::new(FakeRunner::new(|_| ok("")));
    let long = Lima::new(fake.clone(), "limactl".into(), PathBuf::from(format!("/{}", "a".repeat(90)))).unwrap();
    assert!(matches!(long.create(&spec()).await, Err(Error::BadPath { .. })));
    assert!(matches!(Lima::new(fake.clone(), "limactl".into(), "relative/lima".into()), Err(Error::BadPath { .. })));
    assert_eq!(fake.calls(), []);
}

#[tokio::test]
async fn a_clone_asks_for_the_specs_settings_and_needs_a_stopped_free_source() {
    let both = listed(&[(NAME, "Stopped"), ("adytum-test-golden-clone", "Stopped")]);
    let (lima, fake, _home) = lima(FakeRunner::replies(vec![listed(&[(NAME, "Stopped")]), ok(""), listed(&[(NAME, "Running")]), both, ok("")]));
    lima.clone_cell(&name(), &clone_spec()).await.unwrap();
    let forwards = r#".portForwards = [{"guestPort":8080,"hostPort":18080,"hostIP":"127.0.0.1","proto":"tcp"},{"guestIP":"127.0.0.1","proto":"any","guestPortRange":[1,65535],"ignore":true},{"guestIP":"0.0.0.0","proto":"any","guestPortRange":[1,65535],"ignore":true},{"guestIP":"::","proto":"any","guestPortRange":[1,65535],"ignore":true}]"#;
    let set = ["--set", ".cpus = 2", "--set", ".memory = \"2048MiB\"", "--set", ".mounts = []", "--set", forwards];
    assert_eq!(fake.argv()[1], argv(&[&["limactl", "clone", "--tty=false"][..], &set, &[NAME, "adytum-test-golden-clone"]].concat()));
    assert!(matches!(lima.clone_cell(&name(), &clone_spec()).await, Err(Error::Unsupported { .. })), "a running source");
    assert!(matches!(lima.clone_cell(&name(), &clone_spec()).await, Err(Error::Refused(r)) if r.code == RefusalCode::NameTaken));
    assert!(matches!(lima.clone_cell(&name(), &clone_spec()).await, Err(Error::NotFound { .. })), "a missing source");
    assert_eq!(fake.argv().iter().filter(|a| a[1] == "clone").count(), 1);
}
