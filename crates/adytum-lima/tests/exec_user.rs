//! Who a cell's commands run as: the unprivileged exec user, never Lima's
//! admin user, and root only through the method made for provisioning.

use std::sync::Arc;

use adytum_driver::{Driver, ExecOpts};
use adytum_lima::Lima;
use adytum_model::CellName;
use adytum_testkit::{Call, FakeRunner, Transcript};

fn lima(calls: &[&[&str]]) -> (Lima, Arc<FakeRunner>, std::path::PathBuf) {
    let home = std::path::PathBuf::from("/tmp").join(format!("adl-u-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("a scratch folder");
    let calls =
        calls.iter().map(|argv| Call { argv: argv.iter().map(|a| (*a).to_owned()).collect(), stdin: None, stdout: String::new(), stderr: String::new(), status: 0 }).collect();
    let transcript = Transcript { tool: "limactl".into(), version: "2.1.2".into(), calls };
    let fake = Arc::new(FakeRunner::from_transcript(transcript).with_var("lima_home", &home));
    (Lima::new(fake.clone(), "limactl".into(), home.clone()), fake, home)
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

const SHELL: [&str; 4] = ["limactl", "shell", "--workdir=/", "adytum-test-golden"];

fn call<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    SHELL.iter().copied().chain(rest.iter().copied()).collect()
}

#[tokio::test]
async fn exec_and_stream_run_as_the_unprivileged_user_and_exec_root_as_root() {
    let plain = call(&["--", "sudo", "-n", "-u", "cell", "-H", "--", "id"]);
    let with_env = call(&["--", "sudo", "-n", "-u", "cell", "-H", "--", "env", "K=v", "id"]);
    let root = call(&["--", "sudo", "-n", "--", "apt-get", "update"]);
    let root_env = call(&["--", "sudo", "-n", "--", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "update"]);
    let (lima, fake, home) = lima(&[&plain, &with_env, &root, &root_env]);
    let name = "adytum-test-golden".parse::<CellName>().expect("a valid name");
    lima.exec(&name, &args(&["id"]), ExecOpts::default()).await.unwrap();
    lima.exec(&name, &args(&["id"]), ExecOpts { env: vec![("K".into(), "v".into())], ..ExecOpts::default() }).await.unwrap();
    lima.exec_root(&name, &args(&["apt-get", "update"]), ExecOpts::default()).await.unwrap();
    let env = ExecOpts { env: vec![("DEBIAN_FRONTEND".into(), "noninteractive".into())], ..ExecOpts::default() };
    lima.exec_root(&name, &args(&["apt-get", "update"]), env).await.unwrap();
    fake.assert_done();
    let _ = std::fs::remove_dir_all(home);
}

#[tokio::test]
async fn a_bad_command_is_refused_for_root_too() {
    let (lima, fake, home) = lima(&[]);
    let name = "adytum-test-golden".parse::<CellName>().expect("a valid name");
    assert!(lima.exec_root(&name, &[], ExecOpts::default()).await.is_err());
    assert!(lima.exec_root(&name, &args(&["a\0b"]), ExecOpts::default()).await.is_err());
    // A name that is not Adytum's never reaches limactl, as root or not.
    if let Ok(other) = "xenonet-vm1".parse::<CellName>() {
        assert!(lima.exec_root(&other, &args(&["id"]), ExecOpts::default()).await.is_err());
    }
    fake.assert_done();
    let _ = std::fs::remove_dir_all(home);
}
