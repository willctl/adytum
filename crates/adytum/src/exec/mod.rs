//! The only place Adytum starts a process. On unix a deadline stops the
//! child's whole process group: SIGTERM, a grace, then SIGKILL. On Windows
//! only the child itself is stopped.

mod command;
mod duplex;
mod spawned;
mod system;

pub use command::Command;
pub use duplex::{Duplex, DuplexControl};
pub use spawned::{Exit, Spawned, StderrTail};
pub use system::SystemRunner;

use async_trait::async_trait;

use crate::Error;

#[cfg(any(feature = "lima", feature = "openshell"))]
pub(crate) fn tail(text: &str) -> String {
    const KEPT: usize = 2000;
    let text = text.trim();
    let mut start = text.len().saturating_sub(KEPT);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    if start == 0 {
        text.to_owned()
    } else {
        format!("...{}", &text[start..])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// `None` when a signal or the deadline ended the child.
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0) && !self.timed_out
    }

    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, cmd: &Command) -> Result<Output, Error>;

    /// How bytes reach a program in a cell without a forwarded port.
    async fn duplex(&self, cmd: &Command) -> Result<Duplex, Error>;
}
