use crate::openshell::Distro;
use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Profile {
    Native,
    Wsl(Distro),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Host {
    windows: bool,
    system_root: Option<String>,
}

impl Host {
    pub fn new(windows: bool, system_root: Option<String>) -> Host {
        Host { windows, system_root }
    }

    pub fn current() -> Host {
        Host { windows: cfg!(windows), system_root: std::env::var("SystemRoot").ok().filter(|r| !r.is_empty()) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub file: String,
    pub args: Vec<String>,
}

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
    pub fn invocation(&self, host: &Host, args: &[String]) -> Result<Invocation, Error> {
        match self {
            Profile::Wsl(distro) => {
                if !host.windows {
                    return Err(Error::Unsupported { what: "a WSL profile on a machine that is not Windows".to_owned() });
                }
                let mut a = vec!["--distribution".to_owned(), distro.as_str().to_owned(), "--exec".to_owned(), "openshell".to_owned()];
                a.extend(args.iter().cloned());
                Ok(Invocation { file: wsl_program(host.system_root.as_deref()), args: a })
            }
            Profile::Native => {
                if host.windows {
                    return Err(Error::Unsupported { what: "OpenShell without WSL on Windows".to_owned() });
                }
                Ok(Invocation { file: "openshell".to_owned(), args: args.to_vec() })
            }
        }
    }

    /// `None` when the profile is native, where paths are the same.
    pub fn path_translation(&self, host: &Host, file: &str) -> Result<Option<Invocation>, Error> {
        let Profile::Wsl(distro) = self else { return Ok(None) };
        if !host.windows {
            return Err(Error::Unsupported { what: "a WSL profile on a machine that is not Windows".to_owned() });
        }
        let args = vec!["--distribution".to_owned(), distro.as_str().to_owned(), "--exec".to_owned(), "wslpath".to_owned(), "-a".to_owned(), file.to_owned()];
        Ok(Some(Invocation { file: wsl_program(host.system_root.as_deref()), args }))
    }
}

pub fn parse_wslpath(stdout: &str) -> Result<String, Error> {
    let path = stdout.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if !path.starts_with('/') || path.contains(['\r', '\n', '\0', ':']) {
        return Err(Error::Invalid { what: "WSL path", rule: "wslpath printed something that is not one absolute Linux path" });
    }
    Ok(path.to_owned())
}
