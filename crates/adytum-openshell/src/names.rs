use crate::OpenShellError;

fn plain(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"_.-".contains(&c)
}

/// The longest name OpenShell 0.1.4 accepts for a new sandbox.
pub const MAX_NEW_NAME: usize = 19;

/// A sandbox's name: letters, digits, `.`, `-` and `_`, starting with a
/// letter or digit, so it can never read as a flag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SandboxName(String);

impl SandboxName {
    /// A name that already exists (from a record or a listing).
    pub fn new(text: &str) -> Result<SandboxName, OpenShellError> {
        let b = text.as_bytes();
        if b.is_empty() || b.len() > 253 || !b[0].is_ascii_alphanumeric() || !b.iter().all(|c| plain(*c)) {
            return Err(OpenShellError::BadSandboxName(text.to_owned()));
        }
        Ok(SandboxName(text.to_owned()))
    }

    /// A name for a sandbox about to be made: also within OpenShell's limit.
    pub fn for_create(text: &str) -> Result<SandboxName, OpenShellError> {
        let name = SandboxName::new(text)?;
        if text.len() > MAX_NEW_NAME {
            return Err(OpenShellError::SandboxNameTooLong(text.to_owned()));
        }
        Ok(name)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An installed WSL distribution: a plain word of at most 80 characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro(String);

impl Distro {
    pub fn new(text: &str) -> Result<Distro, OpenShellError> {
        if text.is_empty() || text.len() > 80 || !text.bytes().all(plain) {
            return Err(OpenShellError::BadDistro);
        }
        Ok(Distro(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A container image reference such as `ghcr.io/org/image:tag@sha256:...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef(String);

impl ImageRef {
    pub fn new(text: &str) -> Result<ImageRef, OpenShellError> {
        if text.is_empty() || text.starts_with('-') || !text.bytes().all(|c| c.is_ascii_alphanumeric() || b"_./:@-".contains(&c)) {
            return Err(OpenShellError::BadImage);
        }
        Ok(ImageRef(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The name of an OpenShell provider (which holds credentials, so the name
/// is all a command line carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderName(String);

impl ProviderName {
    pub fn new(text: &str) -> Result<ProviderName, OpenShellError> {
        let b = text.as_bytes();
        if b.is_empty() || !b[0].is_ascii_alphanumeric() || !b.iter().all(|c| plain(*c)) {
            return Err(OpenShellError::BadProvider);
        }
        Ok(ProviderName(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A `key=value` label the caller puts on a sandbox, and later selects by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    key: String,
    value: String,
}

impl Label {
    pub fn new(key: &str, value: &str) -> Result<Label, OpenShellError> {
        let ok = |t: &str| !t.is_empty() && t.bytes().all(plain);
        if !ok(key) || !ok(value) {
            return Err(OpenShellError::BadLabel);
        }
        Ok(Label { key: key.to_owned(), value: value.to_owned() })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    /// `key=value`, which `--label` and `--selector` both read.
    pub fn pair(&self) -> String {
        format!("{}={}", self.key, self.value)
    }
}

/// A path inside a sandbox or a WSL distribution: absolute, no `..`, no
/// control characters.
pub fn check_guest_path(path: &str) -> Result<(), OpenShellError> {
    if !path.starts_with('/') || path.chars().any(char::is_control) || path.split('/').any(|p| p == "..") {
        return Err(OpenShellError::BadGuestPath(path.to_owned()));
    }
    Ok(())
}
