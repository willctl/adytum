use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::exec::system::signal_group;
use crate::exec::Command;
use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
    pub timed_out: bool,
}

/// The last bytes of a child's stderr, filling while it runs.
#[derive(Debug, Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<u8>>>);

impl StderrTail {
    fn buffer(&self) -> MutexGuard<'_, VecDeque<u8>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn push(&self, bytes: &[u8], keep: usize) {
        let mut t = self.buffer();
        t.extend(bytes);
        let over = t.len().saturating_sub(keep);
        t.drain(..over);
    }

    pub fn text(&self) -> String {
        let bytes: Vec<u8> = self.buffer().iter().copied().collect();
        String::from_utf8_lossy(&bytes).trim().to_string()
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.buffer().iter().copied().collect()
    }
}

/// A long-lived child the caller drives itself, with plain `std` pipes. The
/// command's deadline is applied in [`Spawned::try_wait`]; dropping a
/// `Spawned` whose child still runs kills its group.
#[derive(Debug)]
pub struct Spawned {
    child: Child,
    pgid: Option<u32>,
    started: Instant,
    deadline: Option<std::time::Duration>,
    ended: bool,
    code: Option<i32>,
    timed_out: bool,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    /// `None` once [`Spawned::stderr_tail`] took it.
    pub stderr: Option<ChildStderr>,
}

impl Spawned {
    /// The command's `stdin_bytes` are not used: write to [`Spawned::stdin`].
    pub fn start(cmd: &Command) -> Result<Spawned, Error> {
        let mut c = cmd.to_std()?;
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|source| Error::Spawn { program: cmd.program().to_owned(), source })?;
        let pgid = if cfg!(unix) { Some(child.id()) } else { None };
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        Ok(Spawned { child, pgid, started: Instant::now(), deadline: cmd.deadline_limit(), ended: false, code: None, timed_out: false, stdin, stdout, stderr })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Reads stderr on a thread and keeps its last `keep` bytes.
    pub fn stderr_tail(&mut self, keep: usize) -> Option<StderrTail> {
        let mut pipe = self.stderr.take()?;
        let tail = StderrTail::default();
        let sink = tail.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.push(&buf[..n], keep);
            }
        });
        Some(tail)
    }

    /// Never waits. Past the deadline it kills the group, including what
    /// the child left running after it exited.
    pub fn try_wait(&mut self) -> Result<Option<Exit>, Error> {
        if !self.ended {
            let waited = self.child.try_wait().map_err(|source| Error::Child { what: "check on the command", source })?;
            if let Some(status) = waited {
                self.ended = true;
                self.code = status.code();
            }
        }
        let late = self.deadline.is_some_and(|d| self.started.elapsed() >= d);
        if late && (!self.ended || self.group_running()) {
            self.kill_group();
            self.timed_out = true;
            self.ended = true;
        }
        Ok(self.ended.then_some(Exit { code: self.code, timed_out: self.timed_out }))
    }

    /// Always false off unix.
    pub fn group_running(&self) -> bool {
        #[cfg(unix)]
        {
            use nix::sys::signal::killpg;
            use nix::unistd::Pid;
            let Some(pgid) = self.pgid.and_then(|p| i32::try_from(p).ok()) else { return false };
            // An exited member not yet reaped still counts.
            killpg(Pid::from_raw(pgid), None).is_ok()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// For a caller that puts the child in a job object, which takes
    /// `unsafe` that Adytum does not have.
    #[cfg(windows)]
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// On Windows only the child itself is killed.
    pub fn kill_group(&mut self) {
        match self.pgid {
            Some(pgid) => signal_group(pgid, true),
            None => {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if !self.ended && !matches!(self.child.try_wait(), Ok(Some(_))) {
            self.kill_group();
        }
    }
}
