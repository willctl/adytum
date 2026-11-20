//! A desktop cell, its view, and the Firefox recipe driven by an MCP
//! server in the cell, through the built binary. Skipped unless
//! `ADYTUM_LIVE=1`:
//!
//! ```text
//! ADYTUM_LIVE=1 cargo test -p adytum-cli --test desktop -- --nocapture --test-threads=1
//! ```

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    reason = "a live gate: helpers outside #[test] fail loudly where they stand, and the timings are printed on purpose"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_adytum");

fn tag() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{:05x}", (nanos ^ std::process::id().rotate_left(11)) & 0xf_ffff)
}

struct Env {
    home: PathBuf,
}

impl Env {
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let bin_dir = Path::new(BIN).parent().unwrap().to_owned();
        let path = std::env::join_paths(std::iter::once(bin_dir).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()))).unwrap();
        let mut cmd = Command::new(program);
        cmd.env_clear().env("ADYTUM_HOME", &self.home).env("HOME", std::env::var_os("HOME").unwrap_or_default()).env("PATH", path);
        cmd
    }

    fn adytum(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.command(BIN).args(args).stdin(Stdio::null()).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.adytum(args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    fn feed(&self, args: &[&str], input: &[u8]) {
        let mut child = self.command(BIN).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        assert!(child.wait().unwrap().success(), "{args:?}");
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Ok(out) = self.command(BIN).args(["ls", "--json"]).stdin(Stdio::null()).output() {
            let listed: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
            for cell in listed["cells"].as_array().into_iter().flatten() {
                if let Some(name) = cell["name"].as_str().filter(|n| n.starts_with("adytum-test-desk-")) {
                    let _ = self.command(BIN).args(["rm", name]).stdin(Stdio::null()).output();
                }
            }
        }
        for entry in std::fs::read_dir(self.home.join("lima")).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("adytum-test-desk-") {
                let _ = self.command("limactl").env("LIMA_HOME", self.home.join("lima")).args(["delete", "--force", "--tty=false", &name]).output();
            }
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn step<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let v = f();
    println!("  {label}: {:.2}s", t.elapsed().as_secs_f64());
    v
}

/// The view: on this machine's loopback, the screen share's banner arrives
/// through the cell's stream, and it stops with the view.
fn view(env: &Env, cell: &str) {
    let mut viewer = env.command(BIN).args(["view", cell, "--json"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut first = String::new();
    BufReader::new(viewer.stdout.take().unwrap()).read_line(&mut first).unwrap();
    let opened: Value = serde_json::from_str(first.trim()).unwrap_or_else(|e| panic!("{e}: {first}"));
    let address: std::net::SocketAddr = opened["address"].as_str().unwrap().parse().unwrap();
    assert!(address.ip().is_loopback(), "{opened}");
    assert_eq!(opened["secret"].as_str().unwrap().len(), 8);
    let mut socket = TcpStream::connect(address).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut banner = [0u8; 12];
    socket.read_exact(&mut banner).unwrap();
    assert_eq!(&banner, b"RFB 003.008\n");
    let listening = env.ok(&["exec", "--root", cell, "--", "ss", "-H", "-ltn", "sport = :5900"]);
    assert!(
        !listening.trim().is_empty() && listening.lines().all(|l| l.contains("127.0.0.1:5900") || l.contains("[::1]:5900")),
        "the screen share listens beyond the guest's loopback: {listening}"
    );
    drop(socket);
    Command::new("kill").args(["-INT", &viewer.id().to_string()]).status().unwrap();
    assert!(viewer.wait().unwrap().success(), "the view did not close cleanly");
    let (_, state, _) = env.adytum(&["exec", "--root", cell, "--", "systemctl", "is-active", "adytum-vnc.service"]);
    assert_ne!(state.trim(), "active", "the screen share outlived the view");
    let (code, _, _) = env.adytum(&["exec", "--root", cell, "--", "test", "-e", "/run/adytum/vnc/pass"]);
    assert_ne!(code, 0, "the view's password outlived it");
}

/// A stdio MCP client: one JSON-RPC message per line each way.
struct Mcp {
    child: Child,
    input: std::process::ChildStdin,
    lines: mpsc::Receiver<String>,
    next: u64,
}

impl Mcp {
    fn start(env: &Env, cell: &str) -> Mcp {
        let mut child = env
            .command(BIN)
            .args(["exec", cell, "--", "firefox-devtools-mcp", "--connect-existing", "--marionette-port", "2828", "--tools", "pages", "snapshot", "input", "screenshot"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Mcp { child, input, lines, next: 1 }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.input, "{message}").unwrap();
        self.input.flush().unwrap();
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).unwrap_or_else(|e| panic!("no answer to {method}: {e}"));
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
            if message["id"] == json!(id) {
                assert!(message.get("error").is_none(), "{method}: {message}");
                return message["result"].clone();
            }
        }
    }
}

#[test]
fn the_firefox_recipe_is_driven_by_mcp_over_the_cell_stream_and_the_desktop_is_viewed() {
    if std::env::var("ADYTUM_LIVE").as_deref() != Ok("1") {
        println!("skipped: set ADYTUM_LIVE=1 to run against real VMs");
        return;
    }
    let total = Instant::now();
    let env = Env { home: PathBuf::from("/tmp").join(format!("adl-desk-{}", tag())) };
    let cell = format!("adytum-test-desk-{}", tag());
    println!("desktop: {cell}, ADYTUM_HOME {}", env.home.display());

    let recipe = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/recipes/firefox.sh");
    step("the recipe (cell, Firefox, Node, the MCP server)", || {
        let out = env.command("sh").arg(&recipe).env("CELL", &cell).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    });
    step("view the desktop", || view(&env, &cell));

    // A page served inside the cell, on its loopback.
    env.ok(&["exec", cell.as_str(), "--", "mkdir", "-p", "/home/cell/page"]);
    let page = b"<!doctype html><html><head><title>Adytum test page</title></head><body><h1>Behind the wall</h1></body></html>\n";
    env.feed(&["exec", &cell, "--", "tee", "/home/cell/page/index.html"], page);
    env.ok(&[
        "exec",
        "--root",
        &cell,
        "--",
        "systemd-run",
        "--unit",
        "adytum-page",
        "--uid",
        "cell",
        "--gid",
        "cell",
        "python3",
        "-m",
        "http.server",
        "8000",
        "--bind",
        "127.0.0.1",
        "--directory",
        "/home/cell/page",
    ]);

    step("MCP: navigate and snapshot", || {
        let mut mcp = Mcp::start(&env, &cell);
        let init = mcp.call("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "adytum-live-test", "version": "1" } }));
        assert!(init["serverInfo"].is_object(), "{init}");
        mcp.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        let tools = mcp.call("tools/list", json!({}));
        let names: Vec<&str> = tools["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"navigate_page") && names.contains(&"take_snapshot"), "{names:?}");
        assert!(!names.contains(&"evaluate_script"), "the script module was left out: {names:?}");
        mcp.call("tools/call", json!({ "name": "navigate_page", "arguments": { "url": "http://127.0.0.1:8000/" } }));
        let snapshot = mcp.call("tools/call", json!({ "name": "take_snapshot", "arguments": {} }));
        assert!(snapshot.to_string().contains("Behind the wall"), "{snapshot}");
        drop(mcp.input);
        let _ = mcp.child.wait();
    });

    env.ok(&["rm", &cell]);
    drop(env);
    println!("total: {:.1}s", total.elapsed().as_secs_f64());
}
