use crate::openshell::names::check_guest_path;
use crate::openshell::{ImageRef, Label, ProviderName, SandboxName};
use crate::Error;

fn strs(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

#[derive(Debug, Clone)]
pub struct Create {
    pub name: SandboxName,
    pub image: ImageRef,
    /// A label the caller can later select the sandbox by.
    pub label: Option<Label>,
    /// An existing provider, which supplies credentials by name.
    pub provider: Option<ProviderName>,
    /// A policy file as the `openshell` process sees it (translated for WSL).
    pub policy: Option<String>,
    /// The command the sandbox runs, after `--`. Empty runs the image's own.
    pub command: Vec<String>,
}

pub fn version() -> Vec<String> {
    strs(&["--version"])
}

pub fn list() -> Vec<String> {
    strs(&["sandbox", "list", "-o", "json"])
}

pub fn list_labelled(label: &Label) -> Vec<String> {
    vec!["sandbox".into(), "list".into(), "--selector".into(), label.pair(), "-o".into(), "json".into()]
}

pub fn create(c: &Create) -> Result<Vec<String>, Error> {
    let mut a = vec!["sandbox".to_owned(), "create".into(), "--name".into(), c.name.as_str().to_owned(), "--from".into(), c.image.as_str().to_owned()];
    if let Some(label) = &c.label {
        a.extend(["--label".to_owned(), label.pair()]);
    }
    a.push("--detach".into());
    if let Some(p) = &c.provider {
        a.extend(["--provider".to_owned(), p.as_str().to_owned()]);
    }
    if let Some(policy) = &c.policy {
        check_guest_path(policy)?;
        a.extend(["--policy".to_owned(), policy.clone()]);
    }
    if !c.command.is_empty() {
        a.push("--".into());
        a.extend(c.command.iter().cloned());
    }
    Ok(a)
}

/// `timeout_secs` is
/// OpenShell's own limit on the command.
pub fn exec(name: &SandboxName, timeout_secs: Option<u32>, argv: &[String]) -> Vec<String> {
    let mut a = vec!["sandbox".to_owned(), "exec".into(), name.as_str().to_owned(), "--no-login-shell".into()];
    if let Some(t) = timeout_secs {
        a.extend(["--timeout".to_owned(), t.to_string()]);
    }
    a.push("--".into());
    a.extend(argv.iter().cloned());
    a
}

/// `no_git_ignore` sends files a
/// `.gitignore` would hide too (for a folder that was already filtered).
pub fn upload(name: &SandboxName, local: &str, dest: &str, no_git_ignore: bool) -> Result<Vec<String>, Error> {
    check_guest_path(local)?;
    check_guest_path(dest)?;
    let mut a = strs(&["sandbox", "upload"]);
    if no_git_ignore {
        a.push("--no-git-ignore".into());
    }
    a.extend([name.as_str().to_owned(), local.to_owned(), dest.to_owned()]);
    Ok(a)
}

pub fn get(name: &SandboxName) -> Vec<String> {
    vec!["sandbox".into(), "get".into(), name.as_str().to_owned(), "--output".into(), "json".into()]
}

pub fn policy_full(name: &SandboxName) -> Vec<String> {
    vec!["policy".into(), "get".into(), name.as_str().to_owned(), "--full".into()]
}

pub fn logs(name: &SandboxName, since: &str) -> Vec<String> {
    vec!["logs".into(), name.as_str().to_owned(), "--since".into(), since.to_owned(), "--source".into(), "sandbox".into()]
}

pub fn delete(name: &SandboxName) -> Vec<String> {
    vec!["sandbox".into(), "delete".into(), name.as_str().to_owned()]
}
