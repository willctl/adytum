#[cfg(any(feature = "lima", feature = "container"))]
pub(crate) mod copy;

use std::path::Path;
use std::time::Duration;

use crate::exec::{Duplex, Output};
use crate::{CellName, CellSpec, CellState, Error, GuestUser};
use async_trait::async_trait;

/// `argv` with `env` put in front through `env(1)`, refused if it holds a
/// NUL byte or a variable name `env` would misread.
/// The program, once the command has one, no NUL byte, and only variable
/// names a shell would take.
#[cfg(any(feature = "lima", feature = "openshell", feature = "container"))]
pub(crate) fn check_command<'a>(argv: &'a [String], env: &[(String, String)]) -> Result<&'a String, Error> {
    let refuse = |what: String| Err(Error::Unsupported { what });
    let Some(program) = argv.first() else { return refuse("an empty command".to_owned()) };
    if argv.iter().chain(env.iter().flat_map(|(k, v)| [k, v])).any(|a| a.contains('\0')) {
        return refuse("a NUL byte in a command".to_owned());
    }
    for (key, _) in env {
        let ok = key.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return refuse(format!("the variable name {key:?}"));
        }
    }
    Ok(program)
}

#[cfg(any(feature = "lima", feature = "openshell"))]
pub(crate) fn command_line(argv: &[String], env: &[(String, String)]) -> Result<Vec<String>, Error> {
    let program = check_command(argv, env)?;
    if env.is_empty() {
        return Ok(argv.to_vec());
    }
    let refuse = |what: String| Err(Error::Unsupported { what });
    if program.contains('=') {
        return refuse("a program name with = in it together with variables".to_owned());
    }
    Ok(std::iter::once("env".to_owned()).chain(env.iter().map(|(k, v)| format!("{k}={v}"))).chain(argv.iter().cloned()).collect())
}

#[cfg(any(feature = "lima", feature = "container"))]
pub(crate) fn check_guest_path(path: &str) -> Result<(), Error> {
    let bad = |rule: &'static str| Error::BadPath { path: path.to_owned(), rule };
    if !path.starts_with('/') {
        return Err(bad("a guest path must be absolute"));
    }
    if path.chars().any(char::is_control) {
        return Err(bad("it has a control character"));
    }
    if path.contains(':') {
        return Err(bad("limactl reads a colon as the end of an instance name"));
    }
    if path.split('/').any(|part| part == "..") {
        return Err(bad("it climbs out with .."));
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct ExecOpts {
    /// Values reach the guest as arguments of `env`, so keep secrets out.
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    pub deadline: Option<Duration>,
    /// Root instead of the cell's unprivileged user, for provisioning only.
    pub root: bool,
}

impl ExecOpts {
    /// As root, for provisioning and for the drivers of nested cells.
    pub fn root() -> ExecOpts {
        ExecOpts { root: true, ..ExecOpts::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: CellName,
    pub state: CellState,
}

/// What a kind of cell can do. Use it through [`Cells`](crate::Cells), which
/// records each change of a cell's life in the ledger.
#[async_trait]
pub trait Driver: Send + Sync {
    /// Makes a stopped cell.
    async fn create(&self, spec: &CellSpec) -> Result<(), Error>;

    /// Returns once commands can run in the cell.
    async fn start(&self, name: &CellName) -> Result<(), Error>;

    /// A command that ran and failed is an `Output`, not an error.
    async fn exec(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Output, Error>;

    /// The command's stdin and stdout as one byte stream. `opts.stdin` is
    /// refused: write to the stream instead.
    async fn stream(&self, name: &CellName, argv: &[String], opts: ExecOpts) -> Result<Duplex, Error>;

    /// The copy lands at `guest`: a file at that path, a folder's contents in
    /// that folder. Missing folders on the way are made. `owner` writes it
    /// and owns it, so it lands only where that user may write; `None` is
    /// the cell's own user. Nothing lands writable by group or others.
    async fn copy_in(&self, name: &CellName, host: &Path, guest: &str, owner: Option<&GuestUser>) -> Result<(), Error>;

    async fn copy_out(&self, name: &CellName, guest: &str, host: &Path) -> Result<(), Error>;

    async fn stop(&self, name: &CellName) -> Result<(), Error>;

    async fn remove(&self, name: &CellName) -> Result<(), Error>;

    /// Only cells named `adytum-*`.
    async fn list(&self) -> Result<Vec<Listed>, Error>;

    /// Makes `to` as a stopped copy of the stopped cell `from`, with the
    /// settings `to` lists.
    async fn clone_cell(&self, from: &CellName, to: &CellSpec) -> Result<(), Error> {
        let _ = (from, to);
        Err(Error::Unsupported { what: "cloning a cell".to_owned() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RefusalCode;

    #[test]
    fn a_name_outside_ours_is_refused_as_name_not_ours() {
        for bad in ["pane", "xenonet-vm1", "xenonet-base-ubuntu", "default", "adytum-", "Adytum-a"] {
            match bad.parse::<CellName>() {
                Err(Error::Refused(r)) => assert_eq!(r.code, RefusalCode::NameNotOurs, "{bad}"),
                other => panic!("{bad}: {other:?}"),
            }
        }
    }

    #[test]
    fn guest_paths_are_absolute_and_stay_put() {
        for ok in ["/", "/tmp/a b", "/tmp/..hidden", "/a/b.c"] {
            assert!(check_guest_path(ok).is_ok(), "{ok}");
        }
        for bad in ["tmp/a", "", "/tmp/../etc", "/..", "/a/..", "/a\nb", "/a:b", "-r"] {
            assert!(check_guest_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn variables_go_in_front_through_env_and_bad_ones_are_refused() {
        let argv = vec!["printenv".to_owned()];
        assert_eq!(command_line(&argv, &[]).unwrap(), ["printenv"]);
        assert_eq!(command_line(&argv, &[("A_1".into(), "x y".into())]).unwrap(), ["env", "A_1=x y", "printenv"]);
        for key in ["", "1A", "A-B", "A=B"] {
            assert!(command_line(&argv, &[(key.into(), "v".into())]).is_err(), "{key:?}");
        }
        assert!(command_line(&[], &[]).is_err());
        assert!(command_line(&["a\0b".to_owned()], &[]).is_err());
        assert!(command_line(&["X=1".to_owned()], &[("A".into(), "v".into())]).is_err());
    }

    #[test]
    fn a_name_of_ours_passes() {
        assert_eq!("adytum-test-1".parse::<CellName>().map(|n| n.to_string()).ok().as_deref(), Some("adytum-test-1"));
    }
}
