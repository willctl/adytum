//! Test doubles for code built on Adytum: a driver that keeps cells in
//! memory, or as folders, and a runner that answers commands without
//! starting them.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use async_trait::async_trait;
use tokio::io::{AsyncWrite, DuplexStream};

use crate::exec::{Command, Duplex, DuplexControl, Output, Runner};
use crate::{CellName, CellSpec, CellState, Driver, Error, ExecOpts, GuestUser, Listed};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An exit with status 0 and this stdout.
pub fn ok(stdout: &str) -> Output {
    Output { status: Some(0), stdout: stdout.as_bytes().to_vec(), ..Output::default() }
}

/// An exit with this status and stderr.
pub fn failed(status: i32, stderr: &str) -> Output {
    Output { status: Some(status), stderr: stderr.as_bytes().to_vec(), ..Output::default() }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The command's stdin bytes, or what was written to its duplex.
    pub stdin: Vec<u8>,
}

type Answer = Box<dyn FnMut(&Call) -> Output + Send>;
type Written = Arc<Mutex<Vec<u8>>>;

/// A [`Runner`] that answers every command with what a closure returns and
/// keeps every call. A duplex reads the answer's stdout and keeps what is
/// written to it.
pub struct FakeRunner {
    answer: Mutex<Answer>,
    calls: Mutex<Vec<(Call, Written)>>,
}

impl FakeRunner {
    pub fn new(answer: impl FnMut(&Call) -> Output + Send + 'static) -> FakeRunner {
        FakeRunner { answer: Mutex::new(Box::new(answer)), calls: Mutex::new(Vec::new()) }
    }

    /// Answers the calls in order with `replies`, then with an empty success.
    pub fn replies(replies: Vec<Output>) -> FakeRunner {
        let mut queue = replies.into_iter();
        FakeRunner::new(move |_| queue.next().unwrap_or_else(|| ok("")))
    }

    pub fn calls(&self) -> Vec<Call> {
        lock(&self.calls).iter().map(|(call, written)| Call { stdin: [call.stdin.as_slice(), &lock(written)].concat(), ..call.clone() }).collect()
    }

    pub fn argv(&self) -> Vec<Vec<String>> {
        self.calls().into_iter().map(|c| c.argv).collect()
    }

    fn take(&self, cmd: &Command) -> (Output, Written) {
        let env = cmd.env_vars().iter().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();
        let call = Call { argv: cmd.argv(), env, stdin: cmd.stdin().unwrap_or_default().to_vec() };
        let out = (lock(&self.answer))(&call);
        let written = Arc::new(Mutex::new(Vec::new()));
        lock(&self.calls).push((call, written.clone()));
        (out, written)
    }
}

#[async_trait]
impl Runner for FakeRunner {
    async fn run(&self, cmd: &Command) -> Result<Output, Error> {
        Ok(self.take(cmd).0)
    }

    async fn duplex(&self, cmd: &Command) -> Result<Duplex, Error> {
        let (out, written) = self.take(cmd);
        let control = Replayed(Output { stdout: Vec::new(), ..out.clone() });
        Ok(Duplex::from_parts(Box::new(io::Cursor::new(out.stdout)), Box::new(Kept(written)), Box::new(control)))
    }
}

struct Kept(Arc<Mutex<Vec<u8>>>);

impl AsyncWrite for Kept {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        lock(&self.0).extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct Replayed(Output);

/// A [`Duplex`] whose other end is the stream returned beside it, for a
/// test that plays the program at that end.
pub fn duplex() -> (Duplex, DuplexStream) {
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    let (read, write) = tokio::io::split(ours);
    (Duplex::from_parts(Box::new(read), Box::new(write), Box::new(Replayed(Output::default()))), theirs)
}

#[async_trait]
impl DuplexControl for Replayed {
    async fn finish(self: Box<Self>) -> Result<Output, Error> {
        Ok(self.0)
    }

    fn kill(&mut self) {}
}

/// A driver operation, for [`FakeDriver::fail_next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Create,
    Start,
    Stop,
    Remove,
    Clone,
}

/// What a failing operation leaves behind, as a crash at that point would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaves {
    /// The tool never ran.
    Nothing,
    /// The cell is left in this state.
    Cell(CellState),
    /// The work was done and the call died before it returned.
    Done,
}

/// A command a [`FakeDriver`] was asked to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    pub cell: CellName,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub stdin: Vec<u8>,
    pub root: bool,
}

/// What a [`FakeDriver`] was asked, in order. Each prints as one line, such
/// as `create adytum-a` or `exec adytum-a uname -a`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverCall {
    /// A change of a cell's life; for a clone, the cell is the copy.
    Life(Op, CellName),
    Exec(Exec),
    Stream {
        cell: CellName,
        argv: Vec<String>,
    },
    CopyIn {
        cell: CellName,
        host: PathBuf,
        guest: String,
        owner: Option<GuestUser>,
    },
    CopyOut {
        cell: CellName,
        guest: String,
        host: PathBuf,
    },
}

impl fmt::Display for DriverCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Life(op, cell) => write!(f, "{} {cell}", format!("{op:?}").to_lowercase()),
            Self::Exec(e) => write!(f, "exec {}{} {}", e.cell, if e.root { " (root)" } else { "" }, e.argv.join(" ")),
            Self::Stream { cell, argv } => write!(f, "stream {cell} {}", argv.join(" ")),
            Self::CopyIn { cell, host, guest, owner } => {
                write!(f, "copy_in {cell} {} {guest}", host.display())?;
                owner.as_ref().map_or(Ok(()), |o| write!(f, " (owner {o})"))
            }
            Self::CopyOut { cell, guest, host } => write!(f, "copy_out {cell} {guest} {}", host.display()),
        }
    }
}

type ExecAnswer = Box<dyn FnMut(&Exec) -> Output + Send>;
type StreamAnswer = Box<dyn FnMut(&[String]) -> Result<Duplex, Error> + Send>;

#[derive(Default)]
struct FakeState {
    cells: BTreeMap<CellName, CellState>,
    specs: BTreeMap<CellName, CellSpec>,
    faults: Vec<(Op, Leaves)>,
    calls: Vec<DriverCall>,
}

/// A [`Driver`] that keeps cells in memory. Commands answer with their
/// arguments joined by spaces, unless [`FakeDriver::on_exec`] says
/// otherwise, and streams are unsupported unless [`FakeDriver::on_stream`]
/// supplies them. Copies only record what was asked, unless the driver was
/// made with [`FakeDriver::in_folders`].
#[derive(Default)]
pub struct FakeDriver {
    state: Mutex<FakeState>,
    exec: Mutex<Option<ExecAnswer>>,
    stream: Mutex<Option<StreamAnswer>>,
    folders: Option<PathBuf>,
}

impl FakeDriver {
    pub fn new() -> FakeDriver {
        FakeDriver::default()
    }

    /// A driver whose cells keep their files as folders: cell `adytum-a` is
    /// `<root>/adytum-a`, and a guest path is the same path under it. Copies
    /// in and out copy real files, keeping modes and symlinks. Cells still
    /// live in memory, so add or create them as with [`FakeDriver::new`].
    pub fn in_folders(root: impl Into<PathBuf>) -> FakeDriver {
        FakeDriver { folders: Some(root.into()), ..FakeDriver::default() }
    }

    /// Where `guest` in `cell` is on the host, for a driver made by
    /// [`FakeDriver::in_folders`].
    pub fn folder(&self, cell: &CellName, guest: &str) -> Option<PathBuf> {
        self.folders.as_ref().map(|root| root.join(cell.as_str()).join(guest.trim_start_matches('/')))
    }

    /// A cell no ledger recorded.
    pub fn add_cell(&self, name: &CellName, state: CellState) {
        lock(&self.state).cells.insert(name.clone(), state);
    }

    /// How `exec` answers.
    pub fn on_exec(&self, answer: impl FnMut(&Exec) -> Output + Send + 'static) {
        *lock(&self.exec) = Some(Box::new(answer));
    }

    /// What `stream` returns, given the command's argv.
    pub fn on_stream(&self, answer: impl FnMut(&[String]) -> Result<Duplex, Error> + Send + 'static) {
        *lock(&self.stream) = Some(Box::new(answer));
    }

    /// Makes the next call of `op` fail, leaving what `leaves` says.
    pub fn fail_next(&self, op: Op, leaves: Leaves) {
        lock(&self.state).faults.push((op, leaves));
    }

    pub fn state(&self, name: &CellName) -> Option<CellState> {
        lock(&self.state).cells.get(name).copied()
    }

    pub fn cells(&self) -> Vec<(CellName, CellState)> {
        lock(&self.state).cells.iter().map(|(n, s)| (n.clone(), *s)).collect()
    }

    pub fn spec(&self, name: &CellName) -> Option<CellSpec> {
        lock(&self.state).specs.get(name).cloned()
    }

    /// Takes a cell away, as when someone removes it behind Adytum's back.
    pub fn lose(&self, name: &CellName) {
        lock(&self.state).cells.remove(name);
    }

    pub fn calls(&self) -> Vec<DriverCall> {
        lock(&self.state).calls.clone()
    }

    /// Records the call, then refuses it if the cell does not exist.
    fn ask(&self, name: &CellName, call: DriverCall) -> Result<(), Error> {
        let mut st = lock(&self.state);
        st.calls.push(call);
        if st.cells.contains_key(name) {
            Ok(())
        } else {
            Err(Error::NotFound { name: name.to_string() })
        }
    }

    fn step(&self, op: Op, name: &CellName, work: impl FnOnce(&mut FakeState) -> Result<(), Error>) -> Result<(), Error> {
        let mut st = lock(&self.state);
        st.calls.push(DriverCall::Life(op, name.clone()));
        let Some(at) = st.faults.iter().position(|(o, _)| *o == op) else { return work(&mut st) };
        let (_, leaves) = st.faults.remove(at);
        match leaves {
            Leaves::Nothing => {}
            Leaves::Cell(state) => {
                st.cells.insert(name.clone(), state);
            }
            Leaves::Done => work(&mut st)?,
        }
        Err(Error::Tool { what: format!("the fake driver's {op:?}"), status: None, stderr: "killed".to_owned() })
    }

    fn set(st: &mut FakeState, name: &CellName, state: CellState) -> Result<(), Error> {
        match st.cells.get_mut(name) {
            Some(s) => {
                *s = state;
                Ok(())
            }
            None => Err(Error::NotFound { name: name.to_string() }),
        }
    }
}

fn io(what: &'static str) -> impl FnOnce(io::Error) -> Error {
    move |source| Error::Io { what, source }
}

/// Copies a file, or what is inside a folder, keeping modes and symlinks.
fn copy(from: &Path, to: &Path) -> Result<(), Error> {
    let meta = std::fs::symlink_metadata(from).map_err(io("look at what is to be copied"))?;
    if !meta.is_dir() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(io("make a folder"))?;
        }
        std::fs::copy(from, to).map_err(io("copy a file"))?;
        return Ok(());
    }
    std::fs::create_dir_all(to).map_err(io("make a folder"))?;
    for child in std::fs::read_dir(from).map_err(io("read a folder"))? {
        let child = child.map_err(io("read a folder"))?;
        let (src, dst) = (child.path(), to.join(child.file_name()));
        let kind = child.file_type().map_err(io("look at a file"))?;
        if kind.is_symlink() {
            let target = std::fs::read_link(&src).map_err(io("read a symlink"))?;
            if std::fs::symlink_metadata(&dst).is_ok() {
                std::fs::remove_file(&dst).map_err(io("replace a symlink"))?;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &dst).map_err(io("make a symlink"))?;
        } else if kind.is_dir() {
            copy(&src, &dst)?;
            std::fs::set_permissions(&dst, std::fs::metadata(&src).map_err(io("look at a folder"))?.permissions()).map_err(io("set a folder's mode"))?;
        } else {
            std::fs::copy(&src, &dst).map_err(io("copy a file"))?;
        }
    }
    Ok(())
}

#[async_trait]
impl Driver for FakeDriver {
    async fn create(&self, spec: &CellSpec) -> Result<(), Error> {
        self.step(Op::Create, &spec.name, |st| {
            st.cells.insert(spec.name.clone(), CellState::Stopped);
            st.specs.insert(spec.name.clone(), spec.clone());
            Ok(())
        })
    }

    async fn start(&self, name: &CellName) -> Result<(), Error> {
        self.step(Op::Start, name, |st| FakeDriver::set(st, name, CellState::Running))
    }

    async fn exec(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Output, Error> {
        let exec = Exec { cell: name.clone(), argv: argv.to_vec(), env: opts.env.into_iter().collect(), stdin: opts.stdin.unwrap_or_default(), root: opts.root };
        self.ask(name, DriverCall::Exec(exec.clone()))?;
        Ok(match lock(&self.exec).as_mut() {
            Some(answer) => answer(&exec),
            None => ok(&argv.join(" ")),
        })
    }

    async fn stream(&self, name: &CellName, argv: &[String]) -> Result<Duplex, Error> {
        self.ask(name, DriverCall::Stream { cell: name.clone(), argv: argv.to_vec() })?;
        match lock(&self.stream).as_mut() {
            Some(answer) => answer(argv),
            None => Err(Error::Unsupported { what: "a stream in the fake driver".to_owned() }),
        }
    }

    async fn copy_in(&self, name: &CellName, host: &Path, guest: &str, owner: Option<&GuestUser>) -> Result<(), Error> {
        self.ask(name, DriverCall::CopyIn { cell: name.clone(), host: host.to_path_buf(), guest: guest.to_owned(), owner: owner.cloned() })?;
        self.folder(name, guest).map_or(Ok(()), |to| copy(host, &to))
    }

    async fn copy_out(&self, name: &CellName, guest: &str, host: &Path) -> Result<(), Error> {
        self.ask(name, DriverCall::CopyOut { cell: name.clone(), guest: guest.to_owned(), host: host.to_path_buf() })?;
        self.folder(name, guest).map_or(Ok(()), |from| copy(&from, host))
    }

    async fn stop(&self, name: &CellName) -> Result<(), Error> {
        self.step(Op::Stop, name, |st| FakeDriver::set(st, name, CellState::Stopped))
    }

    async fn remove(&self, name: &CellName) -> Result<(), Error> {
        self.step(Op::Remove, name, |st| {
            st.cells.remove(name);
            Ok(())
        })
    }

    async fn clone_cell(&self, from: &CellName, to: &CellSpec) -> Result<(), Error> {
        self.step(Op::Clone, &to.name, |st| match st.cells.get(from) {
            Some(CellState::Stopped) => {
                st.cells.insert(to.name.clone(), CellState::Stopped);
                st.specs.insert(to.name.clone(), to.clone());
                Ok(())
            }
            Some(_) => Err(Error::Unsupported { what: format!("cloning {from} while it is not stopped") }),
            None => Err(Error::NotFound { name: from.to_string() }),
        })
    }

    async fn list(&self) -> Result<Vec<Listed>, Error> {
        Ok(self.cells().into_iter().map(|(name, state)| Listed { name, state }).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn name(n: &str) -> CellName {
        n.parse().unwrap()
    }

    #[tokio::test]
    async fn a_folder_driver_copies_real_files_both_ways() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("adytum-fake-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let host = root.join("host");
        std::fs::create_dir_all(host.join("sub"))?;
        std::fs::write(host.join("sub/a.txt"), "a")?;
        let fake = FakeDriver::in_folders(root.join("cells"));
        let a = name("adytum-a");
        fake.add_cell(&a, CellState::Running);
        let root_user: GuestUser = "root".parse()?;
        fake.copy_in(&a, &host, "/opt/in", Some(&root_user)).await?;
        let landed = fake.folder(&a, "/opt/in/sub/a.txt").ok_or("a folder driver")?;
        assert_eq!(landed, root.join("cells/adytum-a/opt/in/sub/a.txt"));
        assert_eq!(std::fs::read_to_string(&landed)?, "a");
        fake.copy_out(&a, "/opt/in/sub", &root.join("back")).await?;
        assert_eq!(std::fs::read_to_string(root.join("back/a.txt"))?, "a");
        let lines: Vec<String> = fake.calls().iter().map(ToString::to_string).collect();
        assert_eq!(lines[0], format!("copy_in adytum-a {} /opt/in (owner root)", host.display()));
        assert!(fake.copy_in(&name("adytum-b"), &host, "/x", None).await.is_err(), "a cell that does not exist");
        assert!(FakeDriver::new().folder(&a, "/x").is_none());
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[tokio::test]
    async fn exec_and_streams_answer_as_the_test_says() -> Result<(), Box<dyn std::error::Error>> {
        let fake = FakeDriver::new();
        let a = name("adytum-a");
        fake.add_cell(&a, CellState::Running);
        fake.on_exec(|e| if e.root { failed(1, "no root here") } else { ok(&format!("{} {}", e.cell, e.argv.join(" "))) });
        fake.on_stream(|argv| {
            let (ours, mut theirs) = duplex();
            let greeting = format!("hello {}", argv.join(" "));
            tokio::spawn(async move { theirs.write_all(greeting.as_bytes()).await });
            Ok(ours)
        });
        assert_eq!(fake.exec(&a, &["id".into()], ExecOpts::default()).await?.stdout_lossy(), "adytum-a id");
        assert_eq!(fake.exec(&a, &["id".into()], ExecOpts { root: true, ..ExecOpts::default() }).await?.status, Some(1));
        let mut stream = fake.stream(&a, &["socat".into(), "-".into()]).await?;
        let mut got = vec![0; 13];
        stream.read_exact(&mut got).await?;
        assert_eq!(got, b"hello socat -");
        let lines: Vec<String> = fake.calls().iter().map(ToString::to_string).collect();
        assert_eq!(lines, ["exec adytum-a id", "exec adytum-a (root) id", "stream adytum-a socat -"]);
        Ok(())
    }
}
