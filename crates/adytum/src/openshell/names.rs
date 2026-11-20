use crate::Error;

fn plain(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"_.-".contains(&c)
}

/// The longest name OpenShell 0.1.4 accepts for a new sandbox.
pub(crate) const MAX_NEW_NAME: usize = 19;

/// A sandbox's name: letters, digits, `.`, `-` and `_`, starting with a
/// letter or digit, so it can never read as a flag.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SandboxName(String);

impl SandboxName {
    pub fn new(text: &str) -> Result<SandboxName, Error> {
        let b = text.as_bytes();
        if b.is_empty() || b.len() > 253 || !b[0].is_ascii_alphanumeric() || !b.iter().all(|c| plain(*c)) {
            return Err(Error::Invalid { what: "sandbox name", rule: "letters, digits, dots, dashes and underscores, starting with a letter or digit" });
        }
        Ok(SandboxName(text.to_owned()))
    }

    /// A name for a sandbox about to be made: also within OpenShell's limit.
    pub fn for_create(text: &str) -> Result<SandboxName, Error> {
        let name = SandboxName::new(text)?;
        if text.len() > MAX_NEW_NAME {
            return Err(Error::Invalid { what: "name for a new sandbox", rule: "at most 19 characters, the most OpenShell 0.1.4 takes" });
        }
        Ok(name)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro(String);

impl Distro {
    pub fn new(text: &str) -> Result<Distro, Error> {
        if text.is_empty() || text.len() > 80 || !text.bytes().all(plain) {
            return Err(Error::Invalid { what: "WSL distribution", rule: "a plain word of at most 80 characters" });
        }
        Ok(Distro(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef(String);

impl ImageRef {
    pub fn new(text: &str) -> Result<ImageRef, Error> {
        if text.is_empty() || text.starts_with('-') || !text.bytes().all(|c| c.is_ascii_alphanumeric() || b"_./:@-".contains(&c)) {
            return Err(Error::Invalid { what: "container image", rule: "letters, digits and _ . / : @ -, not starting with a dash" });
        }
        Ok(ImageRef(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A provider holds credentials, so its name is all a command line carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderName(String);

impl ProviderName {
    pub fn new(text: &str) -> Result<ProviderName, Error> {
        let b = text.as_bytes();
        if b.is_empty() || !b[0].is_ascii_alphanumeric() || !b.iter().all(|c| plain(*c)) {
            return Err(Error::Invalid { what: "OpenShell provider name", rule: "letters, digits, dots, dashes and underscores, starting with a letter or digit" });
        }
        Ok(ProviderName(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    key: String,
    value: String,
}

impl Label {
    pub fn new(key: &str, value: &str) -> Result<Label, Error> {
        let ok = |t: &str| !t.is_empty() && t.bytes().all(plain);
        if !ok(key) || !ok(value) {
            return Err(Error::Invalid { what: "label", rule: "letters, digits, dots, dashes and underscores on each side of the =" });
        }
        Ok(Label { key: key.to_owned(), value: value.to_owned() })
    }

    /// `key=value`, which `--label` and `--selector` both read.
    pub fn pair(&self) -> String {
        format!("{}={}", self.key, self.value)
    }
}

pub(crate) fn check_guest_path(path: &str) -> Result<(), Error> {
    if !path.starts_with('/') || path.chars().any(char::is_control) || path.split('/').any(|p| p == "..") {
        return Err(Error::BadPath { path: path.to_owned(), rule: "it must be absolute, without .. or control characters" });
    }
    Ok(())
}
