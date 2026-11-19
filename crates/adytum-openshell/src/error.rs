use std::fmt;

/// Why OpenShell input was refused before any command ran. The messages are
/// plain sentences a person can act on.
#[derive(Debug)]
pub enum OpenShellError {
    /// A WSL profile on a machine that is not Windows.
    WslNeedsWindows,
    /// A distribution name that is not a plain word.
    BadDistro,
    /// Native execution was asked for on Windows.
    NativeOnWindows,
    /// A sandbox name that cannot be one argument OpenShell reads as a name.
    BadSandboxName(String),
    /// A sandbox name longer than OpenShell accepts for a new sandbox.
    SandboxNameTooLong(String),
    /// A container image reference with characters outside the allowed set.
    BadImage,
    /// A provider name OpenShell would not know.
    BadProvider,
    /// A label key or value that is not a plain word.
    BadLabel,
    /// A path inside the sandbox or WSL that is not absolute and plain.
    BadGuestPath(String),
    /// A policy file that is not a regular file under 256 KiB.
    PolicyTooBig,
    /// The policy file differs from the one that was previewed.
    PolicyChanged,
    /// The policy file could not be read.
    PolicyIo {
        /// The step that failed: "stat" or "open".
        op: &'static str,
        /// The cause.
        source: std::io::Error,
    },
    /// `wslpath` printed something that is not a Linux path.
    PathNotTranslated,
    /// OpenShell printed JSON that could not be read.
    BadJson(String),
}

impl fmt::Display for OpenShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WslNeedsWindows => f.write_str("WSL profiles are available on Windows only."),
            Self::BadDistro => f.write_str("Use the installed WSL distribution name."),
            Self::NativeOnWindows => f.write_str("OpenShell needs a WSL distribution on Windows. Native Windows execution is unavailable."),
            Self::BadSandboxName(n) => write!(f, "The sandbox name {n:?} cannot be used. Use letters, digits, dots, dashes and underscores, starting with a letter or digit."),
            Self::SandboxNameTooLong(n) => write!(f, "The sandbox name {n:?} is longer than the {} characters OpenShell accepts. Use a shorter name.", crate::MAX_NEW_NAME),
            Self::BadImage => f.write_str("Use a container image reference."),
            Self::BadProvider => f.write_str("Use an existing OpenShell provider name."),
            Self::BadLabel => f.write_str("Use letters, digits, dots, dashes and underscores in a label."),
            Self::BadGuestPath(p) => write!(f, "The path {p:?} cannot be used. Give an absolute path without .. or control characters."),
            Self::PolicyTooBig => f.write_str("Choose a policy file under 256 KiB."),
            Self::PolicyChanged => f.write_str("The policy changed. Preview again."),
            Self::PolicyIo { op, source } => write!(f, "Could not {op} the policy file: {source}. Check that it exists and can be read."),
            Self::PathNotTranslated => f.write_str("WSL could not translate this project path."),
            Self::BadJson(e) => write!(f, "OpenShell printed JSON that could not be read: {e}. Check that the openshell version is supported."),
        }
    }
}

impl std::error::Error for OpenShellError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PolicyIo { source, .. } => Some(source),
            _ => None,
        }
    }
}
