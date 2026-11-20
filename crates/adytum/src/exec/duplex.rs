use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use crate::exec::Output;
use crate::Error;

#[async_trait]
pub trait DuplexControl: Send {
    async fn finish(self: Box<Self>) -> Result<Output, Error>;

    fn kill(&mut self);

    /// The child's stderr as it comes, if it can be had apart; once taken,
    /// `finish` leaves it out of its output.
    fn take_stderr(&mut self) -> Option<UnboundedReceiver<Vec<u8>>> {
        None
    }
}

/// A child's stdin and stdout as one byte stream. Shutting it down closes
/// stdin, so the child sees end of input while its output can still be read.
pub struct Duplex {
    reader: Box<dyn AsyncRead + Send + Unpin>,
    writer: Option<Box<dyn AsyncWrite + Send + Unpin>>,
    control: Box<dyn DuplexControl>,
}

impl Duplex {
    pub fn from_parts(reader: Box<dyn AsyncRead + Send + Unpin>, writer: Box<dyn AsyncWrite + Send + Unpin>, control: Box<dyn DuplexControl>) -> Duplex {
        Duplex { reader, writer: Some(writer), control }
    }

    /// Closes both pipes, so a child still writing gets a broken pipe
    /// instead of blocking this call, then waits for it. A command deadline
    /// counts from here.
    pub async fn finish(self) -> Result<Output, Error> {
        let Duplex { reader, writer, control } = self;
        drop(writer);
        drop(reader);
        control.finish().await
    }

    pub fn kill(&mut self) {
        self.control.kill();
    }

    /// The same stream, with `end` applied to the output `finish` gives.
    #[must_use]
    pub fn map_end(self, end: impl FnOnce(Output) -> Output + Send + 'static) -> Duplex {
        let Duplex { reader, writer, control } = self;
        Duplex { reader, writer, control: Box::new(Mapped { inner: control, end: Box::new(end), tail: None }) }
    }

    /// The child's stderr in chunks as it comes, for a caller that shows it
    /// while the child runs. `None` when the far end cannot give it apart,
    /// or it was taken already; then `finish` has it.
    pub fn take_stderr(&mut self) -> Option<UnboundedReceiver<Vec<u8>>> {
        self.control.take_stderr()
    }
}

/// Keeps the end of a stderr that was taken, so `end` can read it; the
/// output `finish` gives then has no stderr, as the taker had it all.
struct Mapped {
    inner: Box<dyn DuplexControl>,
    end: Box<dyn FnOnce(Output) -> Output + Send>,
    tail: Option<(Kept, JoinHandle<()>)>,
}

const KEPT: usize = 4096;

type Kept = Arc<Mutex<Vec<u8>>>;

#[async_trait]
impl DuplexControl for Mapped {
    async fn finish(self: Box<Self>) -> Result<Output, Error> {
        let Mapped { inner, end, tail } = *self;
        let mut out = inner.finish().await?;
        let Some((kept, forward)) = tail else { return Ok(end(out)) };
        let _ = forward.await;
        out.stderr = kept.lock().map(|k| k.clone()).unwrap_or_default();
        let mut out = end(out);
        out.stderr.clear();
        Ok(out)
    }

    fn kill(&mut self) {
        self.inner.kill();
    }

    fn take_stderr(&mut self) -> Option<UnboundedReceiver<Vec<u8>>> {
        let mut from = self.inner.take_stderr()?;
        let (send, to) = tokio::sync::mpsc::unbounded_channel();
        let kept = Arc::new(Mutex::new(Vec::new()));
        let keep = kept.clone();
        let forward = tokio::spawn(async move {
            while let Some(chunk) = from.recv().await {
                if let Ok(mut k) = keep.lock() {
                    k.extend_from_slice(&chunk);
                    let over = k.len().saturating_sub(KEPT);
                    k.drain(..over);
                }
                let _ = send.send(chunk);
            }
        });
        self.tail = Some((kept, forward));
        Some(to)
    }
}

impl std::fmt::Debug for Duplex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Duplex").field("stdin_open", &self.writer.is_some()).finish_non_exhaustive()
    }
}

impl AsyncRead for Duplex {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the child's stdin is closed")
}

impl AsyncWrite for Duplex {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.writer.as_mut() {
            Some(w) => Pin::new(w).poll_write(cx, buf),
            None => Poll::Ready(Err(closed())),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.writer.as_mut() {
            Some(w) => Pin::new(w).poll_flush(cx),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(w) = self.writer.as_mut() {
            match Pin::new(w).poll_shutdown(cx) {
                Poll::Ready(Ok(())) => {}
                other => return other,
            }
        }
        // Dropping the pipe is what closes it; a child pipe's own shutdown
        // does nothing.
        self.writer = None;
        Poll::Ready(Ok(()))
    }
}
