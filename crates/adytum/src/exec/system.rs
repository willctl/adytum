use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::exec::duplex::{Duplex, DuplexControl};
use crate::exec::{Command, Output, Runner};
use crate::Error;

const GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct SystemRunner {
    grace: Duration,
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemRunner {
    pub fn new() -> SystemRunner {
        SystemRunner { grace: GRACE }
    }

    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> SystemRunner {
        self.grace = grace;
        self
    }
}

struct Started {
    child: Child,
    pgid: Option<u32>,
}

fn spawn(cmd: &Command, stdin: Stdio) -> Result<Started, Error> {
    let mut c = tokio::process::Command::from(cmd.to_std()?);
    c.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let child = c.spawn().map_err(|source| Error::Spawn { program: cmd.program().to_owned(), source })?;
    let pgid = if cfg!(unix) { child.id() } else { None };
    Ok(Started { child, pgid })
}

#[cfg(unix)]
pub(crate) fn signal_group(pgid: u32, kill: bool) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let Ok(raw) = i32::try_from(pgid) else { return };
    let _ = killpg(Pid::from_raw(raw), if kill { Signal::SIGKILL } else { Signal::SIGTERM });
}

#[cfg(not(unix))]
pub(crate) fn signal_group(_pgid: u32, _kill: bool) {}

async fn stop(child: &mut Child, pgid: Option<u32>, grace: Duration) {
    match pgid {
        Some(pgid) => {
            signal_group(pgid, false);
            let _ = tokio::time::timeout(grace, child.wait()).await;
            signal_group(pgid, true);
        }
        None => {
            let _ = child.start_kill();
        }
    }
    let _ = child.wait().await;
}

/// What [`Runner::run`] keeps of stdout and of stderr each; a command that
/// writes more is stopped, since its output is held in memory.
pub const MAX_OUTPUT: usize = 64 * 1024 * 1024;

/// Set by a reader that went past [`MAX_OUTPUT`]. The flag, not the notice,
/// decides: a command whose pipe closed may exit before the notice is seen.
#[derive(Default)]
struct Over {
    hit: AtomicBool,
    notice: Notify,
}

fn spawn_reader<R>(mut stream: R, over: Arc<Over>) -> JoinHandle<Vec<u8>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut all = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) if all.len() + n > MAX_OUTPUT => {
                    over.hit.store(true, Ordering::SeqCst);
                    over.notice.notify_one();
                    break;
                }
                Ok(n) => all.extend_from_slice(&buf[..n]),
            }
        }
        all
    })
}

/// Chunks of a pipe as they come, so stderr can be shown while the child
/// runs and is never left to fill its pipe.
async fn forward_chunks<R: AsyncRead + Unpin>(mut pipe: R, send: UnboundedSender<Vec<u8>>) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if send.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

async fn write_stdin(mut stdin: tokio::process::ChildStdin, bytes: Vec<u8>) {
    let _ = stdin.write_all(&bytes).await;
    let _ = stdin.shutdown().await;
}

async fn drive(grace: Duration, cmd: &Command) -> Result<Output, Error> {
    let stdin = if cmd.stdin().is_some() { Stdio::piped() } else { Stdio::null() };
    let Started { mut child, pgid } = spawn(cmd, stdin)?;
    if let (Some(bytes), Some(pipe)) = (cmd.stdin(), child.stdin.take()) {
        tokio::spawn(write_stdin(pipe, bytes.to_vec()));
    }
    let over = Arc::new(Over::default());
    let out = child.stdout.take().map(|s| spawn_reader(s, over.clone()));
    let err = child.stderr.take().map(|s| spawn_reader(s, over.clone()));

    let deadline = async {
        match cmd.deadline_limit() {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending().await,
        }
    };
    let mut timed_out = false;
    let status = tokio::select! {
        waited = child.wait() => Some(waited.map_err(|source| Error::Child { what: "wait for the command", source })?),
        () = deadline => { timed_out = true; None }
        () = over.notice.notified() => None,
    };
    let code = match status {
        Some(s) => s.code(),
        None => {
            stop(&mut child, pgid, grace).await;
            None
        }
    };
    let stdout = collect(out, status.is_some()).await;
    let stderr = collect(err, status.is_some()).await;
    if over.hit.load(Ordering::SeqCst) {
        return Err(Error::Tool {
            what: cmd.program().to_string_lossy().into_owned(),
            status: None,
            stderr: format!("It wrote more than {} MiB, and was stopped: run a command that writes less, or stream it.", MAX_OUTPUT / (1024 * 1024)),
        });
    }
    Ok(Output { status: code, stdout, stderr, timed_out })
}

/// After a stop, a straggler that escaped the group and holds a pipe open
/// gets one second.
async fn collect(reader: Option<JoinHandle<Vec<u8>>>, patient: bool) -> Vec<u8> {
    let Some(handle) = reader else { return Vec::new() };
    if patient {
        handle.await.unwrap_or_default()
    } else {
        tokio::time::timeout(Duration::from_secs(1), handle).await.ok().and_then(Result::ok).unwrap_or_default()
    }
}

#[async_trait]
impl Runner for SystemRunner {
    async fn run(&self, cmd: &Command) -> Result<Output, Error> {
        drive(self.grace, cmd).await
    }

    async fn duplex(&self, cmd: &Command) -> Result<Duplex, Error> {
        let Started { mut child, pgid } = spawn(cmd, Stdio::piped())?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(Error::Child { what: "open the command's pipes", source: std::io::Error::other("the pipes were not created") });
        };
        let (send, stderr) = tokio::sync::mpsc::unbounded_channel();
        let reader = child.stderr.take().map(|pipe| tokio::spawn(forward_chunks(pipe, send)));
        let control = SystemControl { child, pgid, stderr: Some(stderr), reader, deadline: cmd.deadline_limit(), grace: self.grace };
        Ok(Duplex::from_parts(Box::new(stdout), Box::new(stdin), Box::new(control)))
    }
}

struct SystemControl {
    child: Child,
    pgid: Option<u32>,
    stderr: Option<UnboundedReceiver<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    deadline: Option<Duration>,
    grace: Duration,
}

#[async_trait]
impl DuplexControl for SystemControl {
    async fn finish(mut self: Box<Self>) -> Result<Output, Error> {
        let mut timed_out = false;
        let code = match self.deadline {
            Some(limit) => match tokio::time::timeout(limit, self.child.wait()).await {
                Ok(waited) => waited.map_err(|source| Error::Child { what: "wait for the command", source })?.code(),
                Err(_) => {
                    timed_out = true;
                    stop(&mut self.child, self.pgid, self.grace).await;
                    None
                }
            },
            None => self.child.wait().await.map_err(|source| Error::Child { what: "wait for the command", source })?.code(),
        };
        if let Some(reader) = self.reader.take() {
            if timed_out {
                let _ = tokio::time::timeout(Duration::from_secs(1), reader).await;
            } else {
                let _ = reader.await;
            }
        }
        let mut stderr = Vec::new();
        if let Some(chunks) = self.stderr.as_mut() {
            while let Ok(chunk) = chunks.try_recv() {
                stderr.extend_from_slice(&chunk);
            }
        }
        Ok(Output { status: code, stdout: Vec::new(), stderr, timed_out })
    }

    fn take_stderr(&mut self) -> Option<UnboundedReceiver<Vec<u8>>> {
        self.stderr.take()
    }

    fn kill(&mut self) {
        match self.pgid {
            Some(pgid) => signal_group(pgid, true),
            None => {
                let _ = self.child.start_kill();
            }
        }
    }
}
