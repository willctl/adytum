use std::sync::Arc;

use adytum::testing::{failed, ok, FakeDriver};
use adytum::{CellName, CellSpec, CellState};
use serde_json::Value;

use super::{run, Context};

struct Run {
    code: i32,
    out: String,
    err: String,
}

impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(self.out.trim()).unwrap_or_else(|e| panic!("stdout is not one JSON object: {e}: {:?}", self.out))
    }
}

struct Rig {
    driver: Arc<FakeDriver>,
    ctx: Context,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        let home = std::env::temp_dir().join(format!("adytum-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let driver = Arc::new(FakeDriver::new());
        driver.on_exec(|call| if call.argv[0] == "fail" { failed(3, "bad\n") } else { ok(&call.argv.join(" ")) });
        Rig { ctx: Context { driver: driver.clone(), home }, driver }
    }

    async fn go(&self, line: &str) -> Run {
        let tokens: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        self.go_tokens(&tokens).await
    }

    async fn go_tokens(&self, tokens: &[String]) -> Run {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(tokens, &self.ctx, &mut out, &mut err).await;
        Run { code, out: String::from_utf8(out).unwrap(), err: String::from_utf8(err).unwrap() }
    }

    fn calls(&self) -> Vec<String> {
        self.driver.calls().iter().map(ToString::to_string).collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.ctx.home);
    }
}

#[tokio::test]
async fn a_name_outside_ours_is_refused_before_the_driver_is_touched() {
    let rig = Rig::new("refuse");
    for cmd in ["start pane", "stop xenonet-vm1", "rm xenonet-base-ubuntu", "exec pane", "create default", "cp ./a pane:/tmp/a", "cp xenonet-vm2:/etc/hosts ./h"] {
        let tail = if cmd.starts_with("exec") { " --json -- true" } else { " --json" };
        let r = rig.go(&format!("{cmd}{tail}")).await;
        assert_eq!(r.code, 1, "{cmd}");
        assert_eq!(r.json()["error"]["code"], "name_not_ours", "{cmd}");
        assert!(r.err.is_empty(), "{cmd}: {:?}", r.err);
    }
    assert!(rig.calls().is_empty(), "{:?}", rig.calls());
}

#[tokio::test]
async fn a_human_error_reads_adytum_message_fix_on_stderr() {
    let rig = Rig::new("human");
    let r = rig.go("rm pane").await;
    assert_eq!(r.code, 1);
    assert_eq!(r.out, "");
    assert!(r.err.starts_with("adytum: \"pane\" is not a cell Adytum made") && r.err.contains("Adytum only touches cells named adytum-"), "{}", r.err);
}

#[tokio::test]
async fn create_start_ls_stop_rm_in_json() {
    let rig = Rig::new("life");
    let r = rig.go("create adytum-test-a --sealed --wall wall_work --json").await;
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(r.json()["name"], "adytum-test-a");
    assert_eq!(r.json()["state"], "stopped");
    assert_eq!(r.json()["sealed"], true);

    let r = rig.go("ls --json").await;
    assert_eq!(r.json()["cells"][0]["name"], "adytum-test-a");
    assert_eq!(r.json()["cells"][0]["wall"], "wall_work");

    assert_eq!(rig.go("start adytum-test-a --json").await.json()["state"], "running");
    assert_eq!(rig.go("ls --json").await.json()["cells"][0]["state"], "running");
    assert_eq!(rig.go("stop adytum-test-a --json").await.json()["state"], "stopped");
    assert_eq!(rig.go("rm adytum-test-a --json").await.json()["removed"], true);
    assert_eq!(rig.go("ls --json").await.json()["cells"].as_array().unwrap().len(), 0);
    assert_eq!(rig.go("ls").await.out, "no cells\n");
}

#[tokio::test]
async fn creating_a_name_that_exists_is_refused_by_policy() {
    let rig = Rig::new("taken");
    assert_eq!(rig.go("create adytum-test-a").await.code, 0);
    let r = rig.go("create adytum-test-a --json").await;
    assert_eq!(r.code, 1);
    assert_eq!(r.json()["error"]["code"], "name_taken");
    assert!(r.json()["error"]["fix"].as_str().unwrap().contains("remove"));
    assert_eq!(rig.calls().iter().filter(|c| c.starts_with("create")).count(), 1);
}

#[tokio::test]
async fn the_spec_a_create_builds_has_localhost_ports_and_no_mounts() {
    let rig = Rig::new("spec");
    let r = rig.go("create adytum-test-p --sealed --cpus 3 --memory 3072 --port 18080:80 --automation-port 9222").await;
    assert_eq!(r.code, 0, "{}", r.err);
    let spec = rig.driver.spec(&"adytum-test-p".parse().unwrap()).unwrap();
    assert_eq!((spec.resources.cpus, spec.resources.memory_mib), (3, 3072));
    assert_eq!(spec.ports.len(), 1);
    assert_eq!((spec.ports[0].host, spec.ports[0].guest, spec.ports[0].bind.to_string().as_str()), (18080, 80, "127.0.0.1"));
    assert_eq!(spec.automation_port, Some(9222));
    assert!(spec.mounts.is_empty() && spec.sealed);
    assert_eq!(spec.wall.as_str(), "wall_default");
}

#[tokio::test]
async fn publishing_the_automation_port_is_refused() {
    let rig = Rig::new("auto");
    let r = rig.go("create adytum-test-p --port 9222:9222 --automation-port 9222 --json").await;
    assert_eq!(r.code, 1);
    assert_eq!(r.json()["error"]["code"], "automation_port_published");
    assert!(rig.calls().iter().all(|c| !c.starts_with("create")));
}

#[tokio::test]
async fn usage_errors_exit_2_and_json_ones_are_shaped_like_other_errors() {
    let rig = Rig::new("usage");
    let r = rig.go("frobnicate").await;
    assert_eq!(r.code, 2);
    assert!(r.err.starts_with("adytum: There is no command"), "{}", r.err);
    let r = rig.go("start --json").await;
    assert_eq!(r.code, 2);
    assert_eq!(r.json()["error"]["code"], "usage");
    let r = rig.go("create adytum-test-a --image nonesuch --json").await;
    assert_eq!(r.code, 2);
    assert!(r.json()["error"]["fix"].as_str().unwrap().contains("ubuntu-24.04"));
    let r = rig.go("create adytum-test-a --wall Nope").await;
    assert_eq!(r.code, 2);
    assert_eq!(rig.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn exec_passes_arguments_whole_and_reports_the_status() {
    let rig = Rig::new("exec");
    assert_eq!(rig.go("create adytum-test-a").await.code, 0);
    let tokens: Vec<String> = ["exec", "adytum-test-a", "--json", "--", "echo", "$(id)", "a b"].iter().map(|s| (*s).to_owned()).collect();
    let r = rig.go_tokens(&tokens).await;
    assert_eq!(r.code, 0);
    assert_eq!(r.json()["status"], 0);
    assert_eq!(r.json()["stdout"], "echo $(id) a b");
    assert_eq!(rig.calls().iter().filter(|c| c.starts_with("exec")).collect::<Vec<_>>(), ["exec adytum-test-a echo $(id) a b"]);

    let r = rig.go("exec adytum-test-a -- fail").await;
    assert_eq!(r.code, 1);
    assert_eq!(r.err, "bad\nadytum: the command exited with status 3.\n");
    let r = rig.go("exec adytum-test-a --json -- fail").await;
    assert_eq!((r.code, r.json()["status"].as_i64()), (1, Some(3)));
}

#[tokio::test]
async fn cp_works_out_which_side_is_the_cell() {
    let rig = Rig::new("cp");
    assert_eq!(rig.go("create adytum-test-a").await.code, 0);
    assert_eq!(rig.go("cp /tmp/a adytum-test-a:/home/a").await.code, 0);
    assert_eq!(rig.go("cp adytum-test-a:/home/a /tmp/b").await.code, 0);
    let rel = rig.go("cp ./rel.txt adytum-test-a:/tmp/rel.txt").await;
    assert_eq!(rel.code, 0);
    let calls: Vec<String> = rig.calls().into_iter().filter(|c| c.starts_with("copy_")).collect();
    assert_eq!(calls[0], "copy_in adytum-test-a /tmp/a /home/a");
    assert_eq!(calls[1], "copy_out adytum-test-a /home/a /tmp/b");
    assert!(calls[2].starts_with("copy_in adytum-test-a /") && calls[2].ends_with("rel.txt /tmp/rel.txt"), "{}", calls[2]);
    assert_eq!(rig.go("cp /tmp/a /tmp/b").await.code, 2);
    assert_eq!(rig.go("cp adytum-test-a:/a adytum-test-b:/b").await.code, 2);
}

#[tokio::test]
async fn cp_gives_a_copy_into_a_cell_to_the_owner_named() {
    let rig = Rig::new("cp-owner");
    assert_eq!(rig.go("create adytum-test-a").await.code, 0);
    assert_eq!(rig.go("cp --owner root /tmp/a adytum-test-a:/opt/a").await.code, 0);
    assert_eq!(rig.go("cp --owner root adytum-test-a:/opt/a /tmp/b").await.code, 2, "a copy out has no owner");
    let bad = rig.go("cp --json --owner ../root /tmp/a adytum-test-a:/opt/a").await;
    assert_eq!((bad.code, bad.json()["error"]["code"].as_str()), (1, Some("invalid")));
    let copies: Vec<String> = rig.calls().into_iter().filter(|c| c.starts_with("copy_")).collect();
    assert_eq!(copies, ["copy_in adytum-test-a /tmp/a /opt/a (owner root)"]);
}

#[tokio::test]
async fn help_prints_usage_on_stdout() {
    let rig = Rig::new("help");
    let r = rig.go("--help").await;
    assert_eq!(r.code, 0);
    assert!(r.out.contains("adytum create <name>") && r.out.contains("Exit codes"));
    assert_eq!(r.err, "");
}

fn field<'a>(v: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(v, |v, k| &v[*k])
}

fn names(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|n| n.as_str().unwrap().to_owned()).collect()
}

fn spec_named(rig: &Rig, name: &str) -> CellSpec {
    rig.driver.spec(&name.parse().unwrap()).unwrap()
}

#[tokio::test]
async fn what_reconcile_did_is_said_on_stderr_and_in_the_json() {
    let rig = Rig::new("reconcile");
    assert_eq!(rig.go("create adytum-test-half --sealed").await.code, 0);
    assert_eq!(rig.go("create adytum-test-seed --sealed").await.code, 0);
    // The driver lost a recorded cell.
    rig.driver.lose(&"adytum-test-half".parse().unwrap());
    let r = rig.go("ls --json").await;
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(names(&r.json()["reconciled"]["removed"]), ["adytum-test-half"]);
    assert!(r.err.contains("adytum-test-half"), "{}", r.err);
    assert_eq!(r.json()["cells"].as_array().unwrap().len(), 1);
    // The name is free again.
    assert_eq!(rig.go("create adytum-test-half").await.code, 0);
}

#[tokio::test]
async fn a_cell_the_ledger_never_recorded_is_reported_and_never_touched() {
    let rig = Rig::new("unrecorded");
    let mut spec = {
        assert_eq!(rig.go("create adytum-test-seed").await.code, 0);
        spec_named(&rig, "adytum-test-seed")
    };
    spec.name = "adytum-test-stranger".parse().unwrap();
    rig.driver.add_cell(&spec.name, CellState::Running);
    let before = rig.calls().len();

    let r = rig.go("ls --json").await;
    assert_eq!(names(&r.json()["reconciled"]["unrecorded"]), ["adytum-test-stranger"]);
    assert_eq!(r.json()["cells"].as_array().unwrap().len(), 1);
    assert!(rig.go("ls").await.out.contains("adytum-test-stranger  (not recorded by adytum; left alone)"));
    for cmd in [
        "start adytum-test-stranger --json",
        "stop adytum-test-stranger --json",
        "rm adytum-test-stranger --json",
        "exec adytum-test-stranger --json -- true",
        "cp /tmp/a adytum-test-stranger:/tmp/a --json",
    ] {
        let r = rig.go(cmd).await;
        assert_eq!((r.code, r.json()["error"]["code"].as_str()), (1, Some("not_recorded")), "{cmd}");
    }
    let touched: Vec<_> = rig.calls().into_iter().skip(before).collect();
    assert_eq!(touched, Vec::<String>::new(), "only listings may reach the driver");
    assert_eq!(rig.driver.state(&"adytum-test-stranger".parse().unwrap()), Some(CellState::Running));
}

#[tokio::test]
async fn a_clone_across_walls_is_refused_as_wall_clone_before_the_driver_clones() {
    let rig = Rig::new("wallclone");
    assert_eq!(rig.go("create adytum-test-a --wall wall_a").await.code, 0);
    let r = rig.go("clone adytum-test-a adytum-test-b --wall wall_b --json").await;
    assert_eq!(r.code, 1);
    assert_eq!(r.json()["error"]["code"], "wall_clone");
    assert!(rig.calls().iter().all(|c| !c.starts_with("clone")), "{:?}", rig.calls());
    // Nothing was recorded for the refused clone.
    assert_eq!(rig.go("ls --json").await.json()["cells"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_clone_of_a_cell_holding_an_eidolon_is_refused_until_it_is_released() {
    let rig = Rig::new("holdclone");
    assert_eq!(rig.go("create adytum-test-a --wall wall_a").await.code, 0);
    let r = rig.go("hold adytum-test-a eid_1 --json").await;
    assert_eq!((r.code, r.json()["held"].as_bool()), (0, Some(true)), "{}", r.err);

    let r = rig.go("clone adytum-test-a adytum-test-b --json").await;
    assert_eq!(r.code, 1);
    assert_eq!(r.json()["error"]["code"], "clone_open_eidolon");
    assert!(rig.calls().iter().all(|c| !c.starts_with("clone")));

    assert_eq!(rig.go("release adytum-test-a eid_1").await.code, 0);
    let r = rig.go("clone adytum-test-a adytum-test-b --json").await;
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!((field(&r.json(), &["state"]).as_str(), field(&r.json(), &["wall"]).as_str()), (Some("stopped"), Some("wall_a")));
    assert!(rig.calls().iter().any(|c| c == "clone adytum-test-b"));
    let listed = rig.go("ls --json").await.json();
    assert_eq!(listed["cells"].as_array().unwrap().len(), 2);
    // The copy's spec says where it came from and publishes nothing of the source's.
    let spec = spec_named(&rig, "adytum-test-b");
    assert_eq!(spec.clone_from.as_ref().map(CellName::as_str), Some("adytum-test-a"));
    assert_eq!(spec.ports.len(), 0);
}

#[tokio::test]
async fn hold_and_release_need_a_recorded_cell_and_a_plain_id() {
    let rig = Rig::new("hold");
    let r = rig.go("hold adytum-test-nope eid_1 --json").await;
    assert_eq!(r.json()["error"]["code"], "not_recorded");
    assert_eq!(rig.go("hold adytum-test-nope bad/id").await.code, 2);
    let r = rig.go("hold pane eid_1 --json").await;
    assert_eq!(r.json()["error"]["code"], "name_not_ours");
}

#[tokio::test]
async fn start_and_stop_are_recorded_and_settled() {
    let rig = Rig::new("ops");
    assert_eq!(rig.go("create adytum-test-a").await.code, 0);
    assert_eq!(rig.go("start adytum-test-a").await.code, 0);
    assert_eq!(rig.go("ls --json").await.json()["cells"][0]["state"], "running");
    assert_eq!(rig.go("stop adytum-test-a").await.code, 0);
    assert_eq!(rig.go("ls --json").await.json()["cells"][0]["state"], "stopped");
}
