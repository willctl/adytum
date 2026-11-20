//! A person's view of a desktop cell's screen. The screen share listens
//! on the guest's loopback only, and this machine listens on its own
//! loopback only: each connection is carried through the cell's stream,
//! so no port is ever forwarded. VNC's own authentication is weak, so its
//! password is made for one view and removed when the view closes.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

use crate::exec::Output;
use crate::lima::guest::{EXEC_USER, VNC_PASSWORD, VNC_PORT, VNC_UNIT};
use crate::{CellName, Driver, Error, ExecOpts};

pub const SECRET_LEN: usize = 8;
const READY_ASKS: u32 = 40;
const READY_GAP: Duration = Duration::from_millis(250);

pub struct View {
    addr: SocketAddr,
    secret: String,
    task: Option<JoinHandle<()>>,
    driver: Arc<dyn Driver>,
    name: CellName,
}

impl fmt::Debug for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("View").field("address", &self.addr).field("cell", &self.name).finish_non_exhaustive()
    }
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

async fn root(driver: &dyn Driver, name: &CellName, what: &str, argv: &[&str], stdin: Option<Vec<u8>>) -> Result<Output, Error> {
    let out = driver.exec(name, &strings(argv), ExecOpts { stdin, ..ExecOpts::root() }).await?;
    if out.success() {
        return Ok(out);
    }
    Err(Error::Tool { what: what.to_owned(), status: out.status, stderr: crate::exec::tail(&out.stderr_lossy()) })
}

/// The command whose stdin and stdout reach `port` on the guest's loopback.
pub fn stream_argv(port: u16) -> Vec<String> {
    vec!["socat".to_owned(), "-".to_owned(), format!("TCP:127.0.0.1:{port}")]
}

impl View {
    /// Starts the screen share with a new password and listens on this
    /// machine's loopback. `driver` is the cell's own, from
    /// [`Cells::driver_of`](crate::Cells::driver_of).
    pub async fn open(driver: Arc<dyn Driver>, name: CellName) -> Result<View, Error> {
        let unit = driver.exec(&name, &strings(&["systemctl", "cat", VNC_UNIT]), ExecOpts::root()).await?;
        if !unit.success() {
            return Err(Error::Unsupported { what: format!("a view of {name}, which was made without a desktop") });
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.map_err(|source| Error::Io { what: "listen on this machine's loopback", source })?;
        let addr = listener.local_addr().map_err(|source| Error::Io { what: "read the view's address", source })?;
        let secret = make_secret()?;
        let folder = VNC_PASSWORD.rsplit_once('/').map_or("/run/adytum/vnc", |(f, _)| f);
        root(&*driver, &name, "make the screen share's folder", &["install", "-d", "-o", EXEC_USER, "-g", EXEC_USER, "-m", "0700", folder], None).await?;
        // dd, not tee: tee would print the password back.
        let of = format!("of={VNC_PASSWORD}");
        root(&*driver, &name, "give the screen share its password", &["runuser", "-u", EXEC_USER, "--", "dd", &of, "status=none"], Some(secret.clone().into_bytes())).await?;
        root(&*driver, &name, "keep the password private", &["chmod", "0600", VNC_PASSWORD], None).await?;
        root(&*driver, &name, "start the screen share", &["systemctl", "restart", VNC_UNIT], None).await?;
        let listening = format!("sport = :{VNC_PORT}");
        for ask in 1..=READY_ASKS {
            let out = root(&*driver, &name, "look for the screen share", &["ss", "-H", "-ltn", &listening], None).await?;
            if !out.stdout.is_empty() {
                break;
            }
            if ask == READY_ASKS {
                return Err(Error::Tool { what: "the screen share".to_owned(), status: None, stderr: "It did not start listening.".to_owned() });
            }
            tokio::time::sleep(READY_GAP).await;
        }
        let task = tokio::spawn(accept_loop(listener, driver.clone(), name.clone()));
        Ok(View { addr, secret, task: Some(task), driver, name })
    }

    pub fn address(&self) -> SocketAddr {
        self.addr
    }

    pub fn url(&self) -> String {
        format!("vnc://{}", self.addr)
    }

    /// The password for this view alone.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Stops listening and stops the screen share; the password goes with it.
    pub async fn close(mut self) -> Result<(), Error> {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        root(&*self.driver, &self.name, "stop the screen share", &["systemctl", "stop", VNC_UNIT], None).await?;
        root(&*self.driver, &self.name, "remove the password", &["rm", "-f", "--", VNC_PASSWORD], None).await.map(drop)
    }
}

impl Drop for View {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Eight random letters and digits. Bytes of 248 and over are thrown away
/// so every character is equally likely.
fn make_secret() -> Result<String, Error> {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(SECRET_LEN);
    while out.len() < SECRET_LEN {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| Error::Io { what: "get random bytes for a password", source: std::io::Error::other(e.to_string()) })?;
        for b in bytes.into_iter().filter(|b| *b < 248) {
            if out.len() < SECRET_LEN {
                out.push(char::from(ALPHABET[usize::from(b) % ALPHABET.len()]));
            }
        }
    }
    Ok(out)
}

async fn accept_loop(listener: TcpListener, driver: Arc<dyn Driver>, name: CellName) {
    // Dropping the set, when this task is aborted, ends every connection.
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((socket, _)) = accepted {
                    connections.spawn(serve(socket, driver.clone(), name.clone()));
                } else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn serve(mut socket: TcpStream, driver: Arc<dyn Driver>, name: CellName) {
    if let Ok(mut stream) = driver.stream(&name, &stream_argv(VNC_PORT), ExecOpts::default()).await {
        let _ = copy_bidirectional(&mut socket, &mut stream).await;
        let _ = stream.finish().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{duplex, failed, ok, FakeDriver};
    use crate::CellState;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn a_secret_is_eight_letters_and_digits_and_differs_each_time() {
        let (a, b) = (make_secret().unwrap(), make_secret().unwrap());
        assert_eq!(a.len(), SECRET_LEN);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn a_view_listens_on_loopback_and_carries_each_connection_through_the_stream() {
        let name: CellName = "adytum-test-desk".parse().unwrap();
        let fake = FakeDriver::new();
        fake.add_cell(&name, CellState::Running);
        fake.on_exec(|e| if e.argv[0] == "ss" { ok("LISTEN 0 32 127.0.0.1:5900 0.0.0.0:*\n") } else { ok("") });
        fake.on_stream(|argv| {
            assert_eq!(argv, ["socat", "-", "TCP:127.0.0.1:5900"]);
            let (ours, mut theirs) = duplex();
            tokio::spawn(async move { theirs.write_all(b"RFB 003.008\n").await });
            Ok(ours)
        });
        let driver: Arc<dyn Driver> = Arc::new(fake.clone());
        let view = View::open(driver, name.clone()).await.unwrap();
        assert!(view.address().ip().is_loopback());
        assert_eq!(view.url(), format!("vnc://{}", view.address()));
        let mut socket = TcpStream::connect(view.address()).await.unwrap();
        let mut banner = [0u8; 12];
        socket.read_exact(&mut banner).await.unwrap();
        assert_eq!(&banner, b"RFB 003.008\n");
        let secret = view.secret().to_owned();
        let password = fake.calls().into_iter().find_map(|c| match c {
            crate::testing::DriverCall::Exec(e) if e.argv.iter().any(|a| a.starts_with("of=")) => Some(e),
            _ => None,
        });
        let password = password.unwrap();
        assert_eq!(password.stdin, secret.as_bytes(), "the password goes in as stdin, never as an argument");
        assert!(password.root && password.argv.iter().all(|a| !a.contains(&secret)));
        view.close().await.unwrap();
        let lines: Vec<String> = fake.calls().iter().map(ToString::to_string).collect();
        assert!(lines.ends_with(&[format!("exec {name} (root) systemctl stop adytum-vnc.service"), format!("exec {name} (root) rm -f -- /run/adytum/vnc/pass")]), "{lines:?}");
    }

    #[tokio::test]
    async fn a_cell_without_a_desktop_has_no_view() {
        let name: CellName = "adytum-test-plain".parse().unwrap();
        let fake = FakeDriver::new();
        fake.add_cell(&name, CellState::Running);
        fake.on_exec(|_| failed(1, "No files found for adytum-vnc.service."));
        assert!(matches!(View::open(Arc::new(fake), name).await, Err(Error::Unsupported { .. })));
    }
}
