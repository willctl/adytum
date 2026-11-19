use crate::{Distro, OpenShellError};

/// Where `openshell` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Profile {
    /// The `openshell` on this machine's PATH.
    Native,
    /// `openshell` inside a WSL distribution (Windows only).
    Wsl(Distro),
}

/// The machine the commands are built for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Host {
    windows: bool,
    system_root: Option<String>,
}

impl Host {
    pub fn new(windows: bool, system_root: Option<String>) -> Host {
        Host { windows, system_root }
    }

    /// The machine this process runs on.
    pub fn current() -> Host {
        Host { windows: cfg!(windows), system_root: std::env::var("SystemRoot").ok().filter(|r| !r.is_empty()) }
    }

    pub fn is_windows(&self) -> bool {
        self.windows
    }
}

/// A program and its arguments, ready to hand to a runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub file: String,
    pub args: Vec<String>,
}

/// `wsl.exe` where Windows keeps it.
pub fn wsl_program(system_root: Option<&str>) -> String {
    std::path::Path::new(system_root.unwrap_or("C:\\Windows")).join("System32").join("wsl.exe").to_string_lossy().into_owned()
}

/// The distributions `wsl --list --quiet` printed (UTF-16 with NULs, a BOM
/// and CRLF lines), as names. Docker Desktop's own distributions and names
/// that are not plain words are left out.
pub fn parse_wsl_list(stdout: &str) -> Vec<Distro> {
    stdout
        .replace('\0', "")
        .split('\n')
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}'))
        .filter(|d| !d.starts_with("docker-desktop"))
        .filter_map(|d| Distro::new(d).ok())
        .collect()
}

impl Profile {
    /// How a command runs for this profile: native `openshell`, or
    /// `wsl.exe --distribution <d> --exec openshell ...`. Every argument stays
    /// one argument.
    pub fn invocation(&self, host: &Host, args: &[String]) -> Result<Invocation, OpenShellError> {
        match self {
            Profile::Wsl(distro) => {
                if !host.windows {
                    return Err(OpenShellError::WslNeedsWindows);
                }
                let mut a = vec!["--distribution".to_owned(), distro.as_str().to_owned(), "--exec".to_owned(), "openshell".to_owned()];
                a.extend(args.iter().cloned());
                Ok(Invocation { file: wsl_program(host.system_root.as_deref()), args: a })
            }
            Profile::Native => {
                if host.windows {
                    return Err(OpenShellError::NativeOnWindows);
                }
                Ok(Invocation { file: "openshell".to_owned(), args: args.to_vec() })
            }
        }
    }

    /// The command that turns a host path into the path a WSL distribution
    /// sees, or `None` when the profile is native (paths are the same).
    pub fn path_translation(&self, host: &Host, file: &str) -> Result<Option<Invocation>, OpenShellError> {
        let Profile::Wsl(distro) = self else { return Ok(None) };
        if !host.windows {
            return Err(OpenShellError::WslNeedsWindows);
        }
        let args = vec!["--distribution".to_owned(), distro.as_str().to_owned(), "--exec".to_owned(), "wslpath".to_owned(), "-a".to_owned(), file.to_owned()];
        Ok(Some(Invocation { file: wsl_program(host.system_root.as_deref()), args }))
    }
}

/// Reads what `wslpath -a` printed: one absolute Linux path.
pub fn parse_wslpath(stdout: &str) -> Result<String, OpenShellError> {
    let path = stdout.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if !path.starts_with('/') || path.contains(['\r', '\n', '\0', ':']) {
        return Err(OpenShellError::PathNotTranslated);
    }
    Ok(path.to_owned())
}
