//! A sealed cell cannot reach the host's loopback services. Skipped unless
//! `ADYTUM_LIVE=1`:
//!
//! ```text
//! ADYTUM_LIVE=1 cargo test -p adytum-cli --test firewall -- --nocapture
//! ```
//!
//! Lima's user-mode network maps the guest address 192.168.5.2
//! (`host.lima.internal`, also the default gateway) to the host's
//! 127.0.0.1. Without a firewall in the guest, a program in a cell reaches
//! every loopback service on the host. The test opens a listener on the
//! host's loopback, then tries it from a cell at the three names it can have,
//! and checks the guest's own defences: the exec user is not an admin, cannot
//! lift the rules, and DNS still works.
//!
//! It uses an `ADYTUM_HOME` under /tmp, cells named `adytum-test-fw-*`, and
//! removes both at the end, also when a step fails.

#![expect(clippy::unwrap_used, clippy::print_stdout, reason = "a live gate: helpers outside #[test] fail loudly where they stand, and the results are printed on purpose")]

use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adytum::{CellName, CellSpec, Resources};
use adytum::{Driver, ExecOpts};

/// Guest side of a reachability probe: each argument is `host:port`.
const PROBE: &str = r#"
import socket, sys
for target in sys.argv[1:]:
    host, port = target.rsplit(":", 1)
    s = socket.socket()
    s.settimeout(4)
    try:
        s.connect((host, int(port)))
        print(target, "OPEN")
    except Exception as e:
        print(target, "blocked", type(e).__name__)
    finally:
        s.close()
"#;

fn live() -> bool {
    if std::env::var("ADYTUM_LIVE").as_deref() == Ok("1") {
        return true;
    }
    println!("skipped: set ADYTUM_LIVE=1 to run against a real VM");
    false
}

fn tag() -> String {
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let count = COUNT.fetch_add(0x1_1111, Ordering::Relaxed);
    format!("{:05x}", (nanos ^ std::process::id().rotate_left(11) ^ count) & 0xf_ffff)
}

fn free_gib() -> f64 {
    let out = Command::new("df").args(["-k", "/"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let kib: f64 = text.lines().nth(1).and_then(|l| l.split_whitespace().nth(3)).and_then(|v| v.parse().ok()).unwrap();
    kib / 1024.0 / 1024.0
}

/// A listener that accepts and counts connections until dropped.
struct Listener {
    port: u16,
    accepted: Arc<std::sync::atomic::AtomicU32>,
    stop: Arc<AtomicBool>,
}

impl Listener {
    fn on(addr: &str) -> Listener {
        let socket = TcpListener::bind(format!("{addr}:0")).unwrap();
        let port = socket.local_addr().unwrap().port();
        socket.set_nonblocking(true).unwrap();
        let (accepted, stop) = (Arc::new(std::sync::atomic::AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        let (a, s) = (accepted.clone(), stop.clone());
        std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if socket.accept().is_ok() {
                    a.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        Listener { port, accepted, stop }
    }

    fn accepted(&self) -> u32 {
        self.accepted.load(Ordering::Relaxed)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The address the host would use to reach the internet, found without
/// sending anything.
fn host_lan_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("203.0.113.1:9").ok()?;
    match s.local_addr().ok()? {
        SocketAddr::V4(a) if !a.ip().is_loopback() => Some(a.ip().to_string()),
        _ => None,
    }
}

/// Removes the cells and the folder at the end, also on a panic.
struct Home {
    path: PathBuf,
}

impl Home {
    fn new() -> Home {
        Home { path: PathBuf::from("/tmp").join(format!("axh-{}", tag())) }
    }

    fn lima(&self) -> adytum::lima::Lima {
        adytum::lima::Lima::system(adytum::lima::lima_home_in(&self.path)).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Ok(entries) = std::fs::read_dir(self.path.join("lima")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("adytum-test-fw-") {
                    let _ = Command::new("limactl")
                        .args(["delete", "--force", "--tty=false", &name])
                        .env_clear()
                        .env("LIMA_HOME", self.path.join("lima"))
                        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
                        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                        .output();
                }
            }
        }
        // Lima keeps a named network's switch apart from its cells.
        let listed = Command::new("limactl")
            .args(["network", "list"])
            .env_clear()
            .env("LIMA_HOME", self.path.join("lima"))
            .env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .output();
        for line in listed.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default().lines().skip(1) {
            if let Some(name) = line.split_whitespace().next().filter(|n| n.starts_with("adytum-")) {
                let _ = Command::new("limactl")
                    .args(["network", "delete", "--force", "--tty=false", name])
                    .env_clear()
                    .env("LIMA_HOME", self.path.join("lima"))
                    .env("HOME", std::env::var_os("HOME").unwrap_or_default())
                    .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                    .output();
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn spec(name: &str) -> CellSpec {
    CellSpec {
        name: name.parse().unwrap(),
        kind: adytum::Kind::Lima(adytum::LimaOptions::new(adytum::lima::image(adytum::lima::DEFAULT_IMAGE).unwrap())),
        parent: None,
        wall: "wall_fw".parse().unwrap(),
        resources: Resources { cpus: 1, memory_mib: 1024, ..Resources::default() },
        ports: vec![],
        mounts: vec![],
        sealed: true,
        network: None,
        clone_from: None,
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

struct Said {
    status: Option<i32>,
    out: String,
}

async fn run(lima: &adytum::lima::Lima, cell: &CellName, parts: &[&str]) -> Said {
    let o = lima.exec(cell, &argv(parts), ExecOpts::default()).await.unwrap();
    Said { status: o.status, out: o.stdout_lossy() + &o.stderr_lossy() }
}

async fn run_root(lima: &adytum::lima::Lima, cell: &CellName, parts: &[&str]) -> Said {
    let o = lima.exec(cell, &argv(parts), ExecOpts { root: true, ..ExecOpts::default() }).await.unwrap();
    Said { status: o.status, out: o.stdout_lossy() + &o.stderr_lossy() }
}

async fn probe(lima: &adytum::lima::Lima, cell: &CellName, targets: &[String]) -> String {
    let mut parts = vec!["python3", "-c", PROBE];
    parts.extend(targets.iter().map(String::as_str));
    run(lima, cell, &parts).await.out
}

#[test]
fn a_sealed_cell_cannot_reach_the_hosts_loopback() {
    if !live() {
        return;
    }
    let free = free_gib();
    println!("free disk: {free:.1} GiB");
    assert!(free > 6.0, "under 6 GB free: not starting a VM");
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(loopback_is_blocked());
}

async fn loopback_is_blocked() {
    let total = Instant::now();
    let home = Home::new();
    let lima = home.lima();
    let cell: CellName = format!("adytum-test-fw-{}", tag()).parse().unwrap();
    println!("cell {cell}, ADYTUM_HOME {}", home.path.display());

    let t = Instant::now();
    lima.create(&spec(cell.as_str())).await.unwrap();
    println!("  create: {:.1}s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    lima.start(&cell).await.unwrap();
    println!("  start: {:.1}s", t.elapsed().as_secs_f64());

    // A service on the host's loopback only.
    let loopback = Listener::on("127.0.0.1");
    let route = run(&lima, &cell, &["ip", "-4", "route", "show", "default"]).await.out;
    let gateway = route.split_whitespace().skip_while(|w| *w != "via").nth(1).unwrap_or("").to_owned();
    println!("  default route: {}", route.trim());
    let targets: Vec<String> = ["192.168.5.2", "host.lima.internal", gateway.as_str()].iter().map(|h| format!("{h}:{}", loopback.port)).collect();
    let t = Instant::now();
    let seen = probe(&lima, &cell, &targets).await;
    println!("  loopback probe ({:.1}s), listener accepted {} connection(s):\n{seen}", t.elapsed().as_secs_f64(), loopback.accepted());

    // A service on every address of the host: the LAN address is the host's too.
    let wildcard = Listener::on("0.0.0.0");
    if let Some(ip) = host_lan_ip() {
        let seen = probe(&lima, &cell, &[format!("{ip}:{}", wildcard.port)]).await;
        println!("  host LAN address probe (a listener on 0.0.0.0), accepted {}:\n{seen}", wildcard.accepted());
    }

    // The exec user and the guest's own rules.
    let who = run(&lima, &cell, &["id"]).await.out;
    println!("  exec user: {}", who.trim());
    let sudo = run(&lima, &cell, &["sudo", "-n", "true"]).await;
    println!("  sudo -n true: status {:?} {}", sudo.status, sudo.out.trim());
    let flush = run(&lima, &cell, &["sh", "-c", "nft flush ruleset 2>&1 || /usr/sbin/nft flush ruleset 2>&1"]).await;
    println!("  nft flush ruleset: status {:?} {}", flush.status, flush.out.trim());
    let dns = run(&lima, &cell, &["getent", "hosts", "example.com"]).await;
    println!("  getent hosts example.com: status {:?} {}", dns.status, dns.out.trim());
    let resolv = run(&lima, &cell, &["sh", "-c", "grep -h nameserver /etc/resolv.conf /run/systemd/resolve/resolv.conf 2>/dev/null; resolvectl dns 2>&1 | head -3"]).await;
    println!("  resolvers: {}", resolv.out.trim());

    // The rules.
    assert_eq!(loopback.accepted(), 0, "the host's loopback listener took a connection from the cell");
    for line in seen.lines() {
        assert!(line.contains("blocked"), "reachable from the cell: {line}");
    }
    assert!(wildcard.accepted() == 0, "the host's LAN address reached the host's own services");
    assert!(!who.contains("sudo") && !who.contains("(adytum)"), "the exec user is the admin user: {who}");
    assert_ne!(sudo.status, Some(0), "sudo works for the exec user");
    assert_ne!(flush.status, Some(0), "the exec user flushed the firewall: {}", flush.out);
    assert_eq!(dns.status, Some(0), "DNS does not resolve: {}", dns.out);

    // The rules survived the attempt, and root sees them.
    let rules = run_root(&lima, &cell, &["nft", "list", "ruleset"]).await;
    assert!(rules.out.contains("192.168.5.2"), "no rule names the host's address:\n{}", rules.out);
    let again = probe(&lima, &cell, &targets).await;
    assert!(again.lines().all(|l| l.contains("blocked")), "reachable after the flush attempt:\n{again}");
    assert_eq!(loopback.accepted(), 0);

    // A restart loads the rules again before any command runs.
    let t = Instant::now();
    lima.stop(&cell).await.unwrap();
    lima.start(&cell).await.unwrap();
    println!("  restart: {:.1}s", t.elapsed().as_secs_f64());
    let after_restart = probe(&lima, &cell, &targets).await;
    println!("  after a restart:\n{after_restart}");
    assert!(after_restart.lines().all(|l| l.contains("blocked")), "reachable after a restart:\n{after_restart}");
    assert_eq!(loopback.accepted(), 0);
    assert_eq!(run(&lima, &cell, &["id", "-un"]).await.out.trim(), "cell");
    assert_ne!(run(&lima, &cell, &["sudo", "-n", "true"]).await.status, Some(0));
    // The admin user Lima logs in as is not a way round: no key, no su.
    let su = run(&lima, &cell, &["sh", "-c", "ssh -o BatchMode=yes -o ConnectTimeout=3 -o StrictHostKeyChecking=no adytum@127.0.0.1 true 2>&1; su -c true adytum </dev/null 2>&1"])
        .await;
    println!("  ssh/su to the admin user: {}", su.out.trim().replace('\n', " | "));
    assert_ne!(su.status, Some(0), "the exec user became the admin user");

    lima.stop(&cell).await.unwrap();
    lima.remove(&cell).await.unwrap();
    println!("total: {:.1}s", total.elapsed().as_secs_f64());
}

#[test]
fn a_cell_on_a_named_network_cannot_reach_the_host_either() {
    if !live() {
        return;
    }
    let free = free_gib();
    println!("free disk: {free:.1} GiB");
    assert!(free > 6.0, "under 6 GB free: not starting a VM");
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(named_network_is_walled());
}

/// On a named network the cell's only card is on Lima's own switch, and the
/// switch's gateway is the host.
async fn named_network_is_walled() {
    let home = Home { path: PathBuf::from("/tmp").join(format!("axh-{}", &tag()[..4])) };
    let lima = home.lima();
    let cell: CellName = format!("adytum-test-fw-{}", tag()).parse().unwrap();
    let mut s = spec(cell.as_str());
    s.network = Some("lan".parse().unwrap());
    {
        lima.create(&s).await.unwrap();
        lima.start(&cell).await.unwrap();
        let loopback = Listener::on("127.0.0.1");
        let route = run(&lima, &cell, &["ip", "-4", "route", "show", "default"]).await.out;
        let gateway = route.split_whitespace().skip_while(|w| *w != "via").nth(1).unwrap_or("").to_owned();
        let targets: Vec<String> = [gateway.as_str(), "192.168.5.2", "host.lima.internal"].iter().map(|h| format!("{h}:{}", loopback.port)).collect();
        let seen = probe(&lima, &cell, &targets).await;
        println!("  default route {}; probe:\n{seen}", route.trim());
        assert_eq!(loopback.accepted(), 0, "the host's loopback listener took a connection from the cell");
        assert!(seen.lines().all(|l| l.contains("blocked")), "reachable from the cell:\n{seen}");
        let dns = run(&lima, &cell, &["getent", "hosts", "example.com"]).await;
        assert_eq!(dns.status, Some(0), "DNS does not resolve: {}", dns.out);
    }
    lima.remove(&cell).await.unwrap();
}
