use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Error;

/// A program and its arguments, each reaching the child as given; never a
/// shell string. The child's environment starts empty.
#[derive(Clone, PartialEq, Eq)]
pub struct Command {
    program: OsString,
    args: Vec<OsString>,
    raw_args: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
    cwd: Option<PathBuf>,
    deadline: Option<Duration>,
    stdin: Option<Vec<u8>>,
}

impl Command {
    #[must_use]
    pub fn new(program: impl AsRef<OsStr>) -> Command {
        Command { program: program.as_ref().to_owned(), args: Vec::new(), raw_args: Vec::new(), env: BTreeMap::new(), cwd: None, deadline: None, stdin: None }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Command {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(args.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    /// Passed to the child unquoted, for a program that parses its own
    /// command line.
    #[cfg(windows)]
    #[must_use]
    pub fn raw_arg(mut self, arg: impl AsRef<OsStr>) -> Command {
        self.raw_args.push(arg.as_ref().to_owned());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Command {
        self.env.insert(key.as_ref().to_owned(), value.as_ref().to_owned());
        self
    }

    /// Copies the named variables from this process, if they are set.
    #[must_use]
    pub fn pass_env(mut self, names: &[&str]) -> Command {
        for name in names {
            if let Some(value) = std::env::var_os(name) {
                self.env.insert(OsString::from(name), value);
            }
        }
        self
    }

    #[must_use]
    pub fn cwd(mut self, dir: impl AsRef<Path>) -> Command {
        self.cwd = Some(dir.as_ref().to_owned());
        self
    }

    #[must_use]
    pub fn deadline(mut self, deadline: Duration) -> Command {
        self.deadline = Some(deadline);
        self
    }

    #[must_use]
    pub fn stdin_bytes(mut self, bytes: Vec<u8>) -> Command {
        self.stdin = Some(bytes);
        self
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }

    pub fn arg_list(&self) -> &[OsString] {
        &self.args
    }

    pub fn env_vars(&self) -> &BTreeMap<OsString, OsString> {
        &self.env
    }

    pub fn working_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn deadline_limit(&self) -> Option<Duration> {
        self.deadline
    }

    pub fn stdin(&self) -> Option<&[u8]> {
        self.stdin.as_deref()
    }

    pub fn argv(&self) -> Vec<String> {
        std::iter::once(&self.program).chain(self.args.iter()).map(|a| a.to_string_lossy().into_owned()).collect()
    }

    /// Every child starts from here: its own process group, and on Windows
    /// no console window.
    pub(crate) fn to_std(&self) -> Result<std::process::Command, Error> {
        let mut c = std::process::Command::new(resolve(self)?);
        c.args(&self.args).env_clear().envs(&self.env);
        if let Some(dir) = &self.cwd {
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
            for a in &self.raw_args {
                c.raw_arg(a);
            }
        }
        Ok(c)
    }
}

/// A bare name is looked up in the command's `PATH`, else this process's.
fn resolve(cmd: &Command) -> Result<PathBuf, Error> {
    let program = cmd.program();
    if program.is_empty() {
        return Err(Error::Unsupported { what: "a command with no program".to_owned() });
    }
    let as_path = Path::new(program);
    if as_path.components().count() > 1 || program.to_string_lossy().contains(std::path::MAIN_SEPARATOR) {
        return if as_path.is_file() { Ok(as_path.to_path_buf()) } else { Err(Error::ProgramNotFound { program: program.to_owned() }) };
    }
    let path = cmd.env_vars().get(OsStr::new("PATH")).cloned().or_else(|| std::env::var_os("PATH")).unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
        if cfg!(windows) {
            let exe = dir.join(format!("{}.exe", program.to_string_lossy()));
            if is_executable(&exe) {
                return Ok(exe);
            }
        }
    }
    Err(Error::ProgramNotFound { program: program.to_owned() })
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Environment values may be secrets, so Debug names the variables only.
impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("raw_args", &self.raw_args)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("cwd", &self.cwd)
            .field("deadline", &self.deadline)
            .field("stdin_bytes", &self.stdin.as_ref().map(Vec::len))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_names_variables_but_never_values() {
        let cmd = Command::new("tool").env("TOKEN", "hunter2-secret").arg("x");
        let shown = format!("{cmd:?}");
        assert!(shown.contains("TOKEN"));
        assert!(!shown.contains("hunter2"));
    }

    #[test]
    fn pass_env_skips_unset_names() {
        let cmd = Command::new("tool").pass_env(&["ADYTUM_EXEC_SURELY_UNSET_VARIABLE"]);
        assert!(cmd.env_vars().is_empty());
    }
}
