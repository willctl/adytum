use std::ffi::OsString;
use std::fmt;

use crate::{CellName, Refusal};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why Adytum did not do what it was asked. [`Error::code`] is stable for
/// scripts; the message and [`Error::fix`] are sentences for a person.
#[derive(Debug)]
pub enum Error {
    Refused(Refusal),
    Invalid {
        what: &'static str,
        rule: &'static str,
    },
    BadPath {
        path: String,
        rule: &'static str,
    },
    Unsupported {
        what: String,
    },
    NotFound {
        name: String,
    },
    NotRecorded(CellName),
    Tool {
        what: String,
        status: Option<i32>,
        stderr: String,
    },
    ProgramNotFound {
        program: OsString,
    },
    Spawn {
        program: OsString,
        source: std::io::Error,
    },
    Child {
        what: &'static str,
        source: std::io::Error,
    },
    Io {
        what: &'static str,
        source: std::io::Error,
    },
    /// The ledger was written by another schema, and is left as it is.
    SetUpAgain(String),
    Corrupt(String),
    Ledger(String),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused(r) => r.code.as_str(),
            Self::Invalid { .. } => "invalid",
            Self::BadPath { .. } => "bad_path",
            Self::Unsupported { .. } => "unsupported",
            Self::NotFound { .. } => "not_found",
            Self::NotRecorded(_) => "not_recorded",
            Self::Tool { .. } => "tool_failed",
            Self::ProgramNotFound { .. } | Self::Spawn { .. } | Self::Child { .. } => "exec_failed",
            Self::Io { .. } => "io",
            Self::SetUpAgain(_) | Self::Corrupt(_) => "ledger_unreadable",
            Self::Ledger(_) => "ledger_failed",
        }
    }

    pub fn fix(&self) -> &str {
        match self {
            Self::Refused(r) => &r.fix,
            Self::Invalid { .. } => "Change the value to follow that rule.",
            Self::BadPath { .. } => "Give an absolute path without .. in it.",
            Self::Unsupported { .. } => "Leave it out, or use a driver that does it.",
            Self::NotFound { .. } | Self::NotRecorded(_) => "Run `adytum ls` to see the cells.",
            Self::Tool { .. } => "Check what the tool said, and run `adytum doctor` if it is about the setup.",
            Self::ProgramNotFound { .. } => "Install it, or give its full path.",
            Self::Spawn { .. } => "Check that it is executable.",
            Self::Child { .. } | Self::Ledger(_) => "Check the disk and the folder's permissions, then try again.",
            Self::Io { .. } => "Check that the path exists and can be read and written.",
            Self::SetUpAgain(_) | Self::Corrupt(_) => "Move adytum.sqlite out of Adytum's folder, remove the cells it made with `limactl delete`, and run the command again.",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(r) => f.write_str(&r.message),
            Self::Invalid { what, rule } => write!(f, "Not a valid {what}: {rule}."),
            Self::BadPath { path, rule } => write!(f, "The path {path:?} cannot be used: {rule}."),
            Self::Unsupported { what } => write!(f, "Adytum does not support {what} here."),
            Self::NotFound { name } => write!(f, "There is no cell named {name}."),
            Self::NotRecorded(name) => write!(f, "The ledger has no record of a cell named {name}, so Adytum leaves it alone."),
            Self::Tool { what, status, stderr } => {
                match status {
                    Some(s) => write!(f, "{what} exited with status {s}.")?,
                    None => write!(f, "{what} was stopped.")?,
                }
                if !stderr.is_empty() {
                    write!(f, " It said: {stderr}")?;
                }
                Ok(())
            }
            Self::ProgramNotFound { program } => write!(f, "The program {} was not found on PATH.", program.display()),
            Self::Spawn { program, source } => write!(f, "The program {} could not be started: {source}.", program.display()),
            Self::Child { what, source } | Self::Io { what, source } => write!(f, "Could not {what}: {source}."),
            Self::SetUpAgain(why) => f.write_str(why),
            Self::Corrupt(what) => write!(f, "The ledger holds a value it cannot read ({what})."),
            Self::Ledger(why) => write!(f, "The ledger failed: {why}."),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused(r) => Some(r),
            Self::Spawn { source, .. } | Self::Child { source, .. } | Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Self {
        Self::Refused(r)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Ledger(e.to_string())
    }
}
