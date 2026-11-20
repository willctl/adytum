//! Container cells in a Lima cell, through the built binary. Skipped unless
//! `ADYTUM_LIVE=1`:
//!
//! ```text
//! ADYTUM_LIVE=1 cargo test -p adytum-cli --test nest -- --nocapture --test-threads=1
//! ```
//!
//! Each scenario uses an `ADYTUM_HOME` of its own under /tmp, names its
//! cells `adytum-test-nest-*`, and removes them and the folder at the end,
//! also when a step fails.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    reason = "a live gate: helpers outside #[test] fail loudly where they stand, and the timings are printed on purpose"
)]

use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_adytum");
const ALPINE: &str = "docker.io/library/alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f";
/// Big enough that its pull is still running when the test looks for it.
const DEBIAN: &str = "docker.io/library/debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc";

struct Env {
    home: PathBuf,
}

fn live() -> bool {
    let on = std::env::var("ADYTUM_LIVE").as_deref() == Ok("1");
    if !on {
        println!("skipped: set ADYTUM_LIVE=1 to run against real VMs");
    }
    on
}

fn tag() -> String {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{:05x}", (nanos as usize ^ (std::process::id() as usize) << 7 ^ COUNT.fetch_add(0x1111, Ordering::Relaxed)) & 0xf_ffff)
}

fn os_env(cmd: &mut Command) -> &mut Command {
    cmd.env_clear().env("HOME", std::env::var_os("HOME").unwrap_or_default()).env("PATH", std::env::var_os("PATH").unwrap_or_default())
}

impl Env {
    fn new() -> Env {
        Env { home: PathBuf::from("/tmp").join(format!("adl-nest-{}", tag())) }
    }

    fn try_json(&self, args: &[&str]) -> Result<(i32, Value), String> {
        let mut all = args.to_vec();
        let at = all.iter().position(|a| *a == "--").unwrap_or(all.len());
        all.insert(at, "--json");
        let out = os_env(&mut Command::new(BIN)).args(&all).env("ADYTUM_HOME", &self.home).output().map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let value = serde_json::from_str(stdout.trim()).map_err(|e| format!("{args:?}: not JSON ({e}): {stdout}{}", String::from_utf8_lossy(&out.stderr)))?;
        Ok((out.status.code().unwrap_or(-1), value))
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        self.try_json(args).unwrap_or_else(|e| panic!("{e}"))
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, v) = self.json(args);
        assert_eq!(code, 0, "{args:?}: {v}");
        v
    }

    fn exec(&self, name: &str, argv: &[&str]) -> Value {
        let mut args = vec!["exec", name, "--"];
        args.extend_from_slice(argv);
        self.json(&args).1
    }

    fn text(&self, args: &[&str]) -> String {
        let out = os_env(&mut Command::new(BIN)).args(args).env("ADYTUM_HOME", &self.home).output().expect("the binary starts");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// As root in a Lima cell, past Adytum, to look at what it left.
    fn in_vm(&self, vm: &str, argv: &[&str]) -> String {
        let out = os_env(&mut Command::new("limactl"))
            .env("LIMA_HOME", self.home.join("lima"))
            .args(["shell", "--workdir=/", vm, "--", "sudo", "-n", "--"])
            .args(argv)
            .output()
            .expect("limactl starts");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let listed = self.try_json(&["ls"]).map(|(_, v)| v).unwrap_or_default();
        for cell in listed["cells"].as_array().into_iter().flatten().filter(|c| c["parent"].is_null()) {
            if let Some(name) = cell["name"].as_str().filter(|n| n.starts_with("adytum-test-nest-")) {
                let _ = self.try_json(&["rm", name]);
            }
        }
        for entry in std::fs::read_dir(self.home.join("lima")).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("adytum-test-nest-") {
                let _ = os_env(&mut Command::new("limactl")).env("LIMA_HOME", self.home.join("lima")).args(["delete", "--force", "--tty=false", &name]).output();
            }
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// `adytum` with `input` on its stdin: the exit code, stdout and stderr.
fn piped(home: &std::path::Path, args: &[&str], input: &[u8]) -> (i32, Vec<u8>, String) {
    use std::io::Write as _;
    let mut child = Command::new(BIN)
        .args(args)
        .env_clear()
        .env("ADYTUM_HOME", home)
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary starts");
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let feeder = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    feeder.join().unwrap();
    (out.status.code().unwrap_or(-1), out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A tar of one file, `<dir>/inner/hello.txt`, made on this machine.
fn small_tar(dir: &std::path::Path) -> Vec<u8> {
    std::fs::create_dir_all(dir.join("inner")).unwrap();
    std::fs::write(dir.join("inner/hello.txt"), "through stdin\n").unwrap();
    let out = Command::new("tar").args(["-c", "-f", "-", "-C"]).arg(dir).arg("inner").env("COPYFILE_DISABLE", "1").output().unwrap();
    assert!(out.status.success());
    out.stdout
}

/// `adytum exec` forwards its stdin and passes the exit code through.
fn exec_forwards_stdin(home: &std::path::Path, name: &str, folder: &str) {
    let tar = small_tar(&home.join("stdin-src"));
    let (code, _, err) = piped(home, &["exec", name, "--", "mkdir", "-p", folder], b"");
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = piped(home, &["exec", name, "--", "tar", "-x", "-f", "-", "-C", folder], &tar);
    assert_eq!(code, 0, "tar in the cell: {err}");
    let (code, out, err) = piped(home, &["exec", name, "--", "cat", &format!("{folder}/inner/hello.txt")], b"");
    assert_eq!((code, String::from_utf8_lossy(&out).into_owned()), (0, "through stdin\n".to_owned()), "{err}");
    let big: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
    let (code, out, _) = piped(home, &["exec", name, "--", "cat"], &big);
    assert_eq!((code, out.len()), (0, big.len()));
    assert!(out == big, "the bytes changed on the way through the stream");
    let (code, _, err) = piped(home, &["exec", name, "--", "sh", "-c", "cat >/dev/null; echo said >&2; exit 7"], b"ignored");
    assert_eq!(code, 7, "the command's exit code passes through: {err}");
    assert!(err.starts_with("said\n"), "{err}");
}

fn step<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let v = f();
    println!("  {label}: {:.2}s", t.elapsed().as_secs_f64());
    v
}

fn default_lima_names() -> Vec<String> {
    let out = os_env(&mut Command::new("limactl")).args(["list", "--format", "{{.Name}}"]).output();
    let mut names: Vec<String> = out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_owned).collect()).unwrap_or_default();
    names.sort();
    names
}

/// A listener on this machine's loopback that counts who reaches it.
fn host_listener() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let reached = Arc::new(AtomicUsize::new(0));
    let count = reached.clone();
    std::thread::spawn(move || {
        for _ in listener.incoming().flatten() {
            count.fetch_add(1, Ordering::Relaxed);
        }
    });
    (port, reached)
}

fn status(v: &Value) -> i64 {
    v["status"].as_i64().unwrap_or(-1)
}

#[test]
fn containers_in_a_lima_cell_from_create_to_rm() {
    if !live() {
        return;
    }
    let before = default_lima_names();
    let total = Instant::now();
    let env = Env::new();
    let t = tag();
    let (vm, sealed, open) = (format!("adytum-test-nest-vm-{t}"), format!("adytum-test-nest-s-{t}"), format!("adytum-test-nest-o-{t}"));
    println!("nest: {vm} holds {sealed} and {open}, ADYTUM_HOME {}", env.home.display());

    step("create the Lima cell with containers", || env.ok(&["create", &vm, "--containers", "--wall", "wall_nest"]));
    let v = step("create a sealed container (starts the Lima cell, pulls)", || env.ok(&["create", &sealed, "--kind", "container", "--parent", &vm, "--image", ALPINE, "--sealed"]));
    assert_eq!((v["parent"].as_str(), v["wall"].as_str(), v["sealed"].as_bool()), (Some(vm.as_str()), Some("wall_nest"), Some(true)), "{v}");
    step("create an open container", || env.ok(&["create", &open, "--kind", "container", "--parent", &vm, "--image", ALPINE]));
    step("start both", || {
        env.ok(&["start", &sealed]);
        env.ok(&["start", &open]);
    });
    let tree = env.text(&["ls"]);
    assert!(tree.contains(&format!("{vm}  running  wall_nest  lima\n  {open}  running  wall_nest  container\n  {sealed}  running")), "{tree}");

    // Both run under gVisor: its kernel answers, not the Lima cell's.
    let vm_kernel = env.exec(&vm, &["uname", "-r"])["stdout"].as_str().unwrap().to_owned();
    for c in [&sealed, &open] {
        let dmesg = env.exec(c, &["dmesg"]);
        assert!(dmesg["stdout"].as_str().unwrap().contains("gVisor"), "{c} is not under gVisor: {dmesg}");
        let kernel = env.exec(c, &["uname", "-r"])["stdout"].as_str().unwrap().to_owned();
        assert_ne!(kernel, vm_kernel, "{c} shares the Lima cell's kernel");
        println!("  {c}: kernel {} (the Lima cell's is {})", kernel.trim(), vm_kernel.trim());
    }

    // The container's user: 1000, with no capability at all, and no way up.
    let id = env.exec(&sealed, &["id", "-u"]);
    assert_eq!(id["stdout"], "1000\n", "{id}");
    let caps = env.exec(&sealed, &["grep", "-E", "^Cap(Eff|Prm|Bnd)", "/proc/self/status"]);
    assert_eq!(caps["stdout"].as_str().unwrap().lines().filter(|l| l.ends_with("0000000000000000")).count(), 3, "{caps}");
    let nnp = env.exec(&sealed, &["grep", "NoNewPrivs", "/proc/self/status"]);
    assert!(nnp["stdout"].as_str().unwrap().contains('1'), "{nnp}");

    // A sealed container has no network, so neither the host's loopback nor
    // the Lima cell's way to it can be reached.
    let (port, reached) = host_listener();
    step("sealed: no network, no host", || {
        let devices = env.exec(&sealed, &["cat", "/proc/net/dev"]);
        let names: Vec<&str> = devices["stdout"].as_str().unwrap().lines().filter_map(|l| l.split_once(':').map(|(n, _)| n.trim())).collect();
        // gVisor lists not even a loopback for a container with no network.
        assert!(names.iter().all(|n| *n == "lo"), "{devices}");
        for target in ["192.168.5.2", "host.lima.internal", "127.0.0.1"] {
            let url = format!("http://{target}:{port}/");
            assert_ne!(status(&env.exec(&sealed, &["wget", "-q", "-T", "3", "-O", "/dev/null", &url])), 0, "the sealed container reached {url}");
        }
        assert_ne!(status(&env.exec(&sealed, &["nslookup", "example.com"])), 0, "a sealed container resolved a name");
    });
    // An open one reaches the internet through its parent, and still not the host.
    step("open: internet yes, host no", || {
        assert_eq!(status(&env.exec(&open, &["wget", "-q", "-T", "10", "-O", "/dev/null", "http://example.com/"])), 0, "the open container has no internet");
        for target in ["192.168.5.2", "host.lima.internal"] {
            let url = format!("http://{target}:{port}/");
            assert_ne!(status(&env.exec(&open, &["wget", "-q", "-T", "3", "-O", "/dev/null", &url])), 0, "the open container reached {url}");
        }
    });
    assert_eq!(reached.load(Ordering::Relaxed), 0, "something reached the host's listener");

    // Files in and out, byte for byte, owned by the container's user.
    let files = env.home.join("files");
    std::fs::create_dir_all(files.join("tree/sub")).unwrap();
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i * 7 + (i >> 8)) as u8).collect();
    std::fs::write(files.join("in.bin"), &bytes).unwrap();
    std::fs::write(files.join("tree/sub/open.txt"), "leaf\n").unwrap();
    std::fs::set_permissions(files.join("tree/sub/open.txt"), PermissionsExt::from_mode(0o666)).unwrap();
    let path = |p: &str| format!("{sealed}:{p}");
    step("cp in and out", || {
        env.ok(&["cp", files.join("in.bin").to_str().unwrap(), &path("/tmp/in.bin")]);
        env.ok(&["cp", &path("/tmp/in.bin"), files.join("out.bin").to_str().unwrap()]);
        assert_eq!(std::fs::read(files.join("out.bin")).unwrap(), bytes, "the file changed on the way");
        env.ok(&["cp", files.join("tree").to_str().unwrap(), &path("/tmp/tree")]);
        env.ok(&["cp", &path("/tmp/tree"), files.join("back").to_str().unwrap()]);
        assert_eq!(std::fs::read_to_string(files.join("back/sub/open.txt")).unwrap(), "leaf\n");
    });
    let owners = env.exec(&sealed, &["stat", "-c", "%u:%g %a", "/tmp/in.bin", "/tmp/tree/sub", "/tmp/tree/sub/open.txt"]);
    assert_eq!(owners["stdout"], "1000:1000 644\n1000:1000 755\n1000:1000 644\n", "{owners}");
    step("cp --owner root", || env.ok(&["cp", "--owner", "root", files.join("in.bin").to_str().unwrap(), &path("/opt/root.bin")]));
    assert_eq!(env.exec(&sealed, &["stat", "-c", "%u:%g %a", "/opt/root.bin"])["stdout"], "0:0 644\n");
    assert_ne!(status(&env.exec(&sealed, &["rm", "-f", "/opt/root.bin"])), 0, "the container's user changed root's copy");
    let (code, v) = env.json(&["cp", files.join("in.bin").to_str().unwrap(), &path("/etc/adytum-test")]);
    assert_eq!((code, v["error"]["code"].as_str()), (1, Some("tool_failed")), "the container's user wrote into /etc: {v}");

    step("exec forwards stdin through a container", || exec_forwards_stdin(&env.home, &sealed, "/tmp/t"));

    // Removing the Lima cell removes its containers first, then itself.
    let v = step("rm the Lima cell", || env.ok(&["rm", &vm]));
    assert_eq!(v["removed"], true);
    let (_, listed) = env.json(&["ls"]);
    assert_eq!(listed["cells"], serde_json::json!([]), "{listed}");
    assert!(!env.home.join("lima").join(&vm).exists());
    drop(env);
    assert_eq!(default_lima_names(), before, "Lima's own home changed");
    println!("total: {:.1}s", total.elapsed().as_secs_f64());
}

/// `adytum create` of a container killed with SIGKILL while its image is
/// being pulled: the next command removes what was begun and closes the
/// operation, and the Lima cell holds no container after.
#[test]
fn a_container_create_killed_part_way_leaves_nothing_after_reconcile() {
    if !live() {
        return;
    }
    let before = default_lima_names();
    let total = Instant::now();
    let env = Env::new();
    let t = tag();
    let (vm, child) = (format!("adytum-test-nest-k-{t}"), format!("adytum-test-nest-kc-{t}"));
    println!("kill: {child} in {vm}, ADYTUM_HOME {}", env.home.display());
    step("create and start a sealed Lima cell with containers", || {
        env.ok(&["create", &vm, "--sealed", "--containers"]);
        env.ok(&["start", &vm]);
    });

    let mut create = os_env(&mut Command::new(BIN))
        .args(["create", &child, "--kind", "container", "--parent", &vm, "--image", DEBIAN])
        .env("ADYTUM_HOME", &env.home)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the binary starts");
    let pulling = format!("{vm} -- sudo -n -- nerdctl pull");
    let waited = Instant::now();
    loop {
        let found = Command::new("pgrep").args(["-f", &pulling]).output().is_ok_and(|o| o.status.success());
        if found {
            break;
        }
        assert!(waited.elapsed() < Duration::from_secs(120), "the pull never began");
        assert!(create.try_wait().unwrap().is_none(), "adytum create ended before the pull was seen");
        std::thread::sleep(Duration::from_millis(20));
    }
    create.kill().expect("SIGKILL");
    create.wait().unwrap();
    // A crash takes this machine's processes with it, limactl's included.
    let _ = Command::new("pkill").args(["-9", "-f", &pulling]).output();
    println!("  killed adytum create {:.2}s after it started waiting", waited.elapsed().as_secs_f64());
    std::thread::sleep(Duration::from_secs(2));

    let (code, v) = step("ls after the kill (reconcile)", || env.json(&["ls"]));
    assert_eq!(code, 0, "{v}");
    let removed: Vec<&str> = v["reconciled"]["removed"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(removed, [child.as_str()], "{v}");
    let names: Vec<&str> = v["cells"].as_array().unwrap().iter().filter_map(|c| c["name"].as_str()).collect();
    assert_eq!(names, [vm.as_str()], "{v}");
    let inside = env.in_vm(&vm, &["nerdctl", "ps", "--all", "--format", "{{.Names}}"]);
    assert!(!inside.contains(&child), "the killed create left a container: {inside}");
    let (_, doctor) = env.json(&["doctor"]);
    assert!(doctor["checks"].as_array().unwrap().iter().any(|c| c["id"] == "ledger-settled" && c["ok"] == true), "{doctor}");

    step("create the same name", || env.ok(&["create", &child, "--kind", "container", "--parent", &vm, "--image", ALPINE]));
    step("rm the Lima cell", || env.ok(&["rm", &vm]));
    drop(env);
    assert_eq!(default_lima_names(), before, "Lima's own home changed");
    println!("total: {:.1}s", total.elapsed().as_secs_f64());
}
