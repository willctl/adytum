use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::system::{resolve, signal_group};
use crate::{Command, ExecError};

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// The exit code, or `None` if a signal ended it.
    pub code: Option<i32>,
    /// The command's deadline passed and its group was stopped.
    pub timed_out: bool,
}

/// The last bytes of a child's standard error, filling while it runs.
/// Clones share one buffer.
#[derive(Debug, Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<u8>>>);

impl StderrTail {
    fn buffer(&self) -> MutexGuard<'_, VecDeque<u8>> {
        // A panic while holding the lock leaves plain bytes behind.
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn push(&self, bytes: &[u8], keep: usize) {
        let mut t = self.buffer();
        t.extend(bytes);
        let over = t.len().saturating_sub(keep);
        t.drain(..over);
    }

    /// What the child has written so far, lossy, trimmed of white space.
    pub fn text(&self) -> String {
        let bytes: Vec<u8> = self.buffer().iter().copied().collect();
        String::from_utf8_lossy(&bytes).trim().to_string()
    }

    /// The bytes themselves.
    pub fn bytes(&self) -> Vec<u8> {
        self.buffer().iter().copied().collect()
    }
}

/// A child started under adytum-exec's rules and handed to the caller: an
/// empty environment plus the variables the command names, its own process
/// group, and a group kill. Unlike [`crate::Runner::run`] nothing waits
/// for it here, so the caller owns the loop. That suits callers that keep
/// a long-lived process, record its pid, or drain its log themselves.
///
/// The command's deadline is checked each time [`Spawned::try_wait`] runs:
/// once it has passed, the group is killed and the exit says so. Dropping a
/// `Spawned` whose child still runs kills its group.
///
/// The pipes are plain `std` pipes, so no async runtime is needed; convert
/// them with `tokio::process::ChildStdin::from_std` where one is.
#[derive(Debug)]
pub struct Spawned {
    child: Child,
    pgid: Option<u32>,
    started: Instant,
    deadline: Option<std::time::Duration>,
    ended: bool,
    code: Option<i32>,
    timed_out: bool,
    /// The child's standard input, always a pipe. Dropping it closes it.
    pub stdin: Option<ChildStdin>,
    /// The child's standard output, always a pipe.
    pub stdout: Option<ChildStdout>,
    /// The child's standard error as a pipe, until [`Spawned::stderr_tail`]
    /// or the caller takes it.
    pub stderr: Option<ChildStderr>,
}

impl Spawned {
    /// Starts `cmd`. Its stdin, stdout and stderr are pipes; `stdin_bytes`
    /// is not used here, the caller writes to [`Spawned::stdin`].
    pub fn start(cmd: &Command) -> Result<Spawned, ExecError> {
        let program = resolve(cmd)?;
        let mut c = std::process::Command::new(&program);
        c.args(cmd.arg_list()).env_clear().envs(cmd.env_vars()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(dir) = cmd.working_dir() {
            c.current_dir(dir);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            c.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            c.creation_flags(0x0800_0000);
            for a in cmd.raw_arg_list() {
                c.raw_arg(a);
            }
        }
        let mut child = c.spawn().map_err(|source| ExecError::Spawn { program: cmd.program().to_owned(), source })?;
        let pgid = if cfg!(unix) { Some(child.id()) } else { None };
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        Ok(Spawned { child, pgid, started: Instant::now(), deadline: cmd.deadline_limit(), ended: false, code: None, timed_out: false, stdin, stdout, stderr })
    }

    /// The child's process id (and its group's, on unix).
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Reads the child's stderr in the background and keeps the last
    /// `keep` bytes, readable at any time. Returns `None` if stderr was
    /// already taken.
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

    /// Whether the child has ended, without waiting. Applies the command's
    /// deadline: past it, the group is killed and the exit has `timed_out`.
    /// The deadline also covers what the child left running in its group
    /// after it exited, so a caller that waits for the group
    /// ([`Spawned::group_running`]) keeps calling this.
    pub fn try_wait(&mut self) -> Result<Option<Exit>, ExecError> {
        if !self.ended {
            let waited = self.child.try_wait().map_err(|source| ExecError::Io { what: "check on the command", source })?;
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

    /// Whether anything is still alive in the child's process group (on
    /// unix; always false elsewhere), so a caller can wait for what the
    /// child started to end too.
    pub fn group_running(&self) -> bool {
        #[cfg(unix)]
        {
            use nix::sys::signal::killpg;
            use nix::unistd::Pid;
            let Some(pgid) = self.pgid.and_then(|p| i32::try_from(p).ok()) else { return false };
            // Signal 0 only asks whether the group has a member. A
            // member that has exited but isn't reaped yet still counts.
            killpg(Pid::from_raw(pgid), None).is_ok()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Windows only: the child itself, for a caller that puts it in a job
    /// object of its own (adytum-exec has no process group there, and
    /// stays free of `unsafe`).
    #[cfg(windows)]
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Kills the child and everything in its process group now (on
    /// Windows only the child), then reaps the child.
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
