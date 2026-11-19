use serde_json::Value;

use crate::OpenShellError;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// What `sandbox list -o json` printed: `{ "sandboxes": [...], "next_page_token": ... }`.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxList {
    /// The sandboxes on this page, or `None` if the key was missing.
    pub sandboxes: Option<Vec<Value>>,
    /// Whether another page follows.
    pub more: bool,
}

pub fn parse_list(text: &str) -> Result<SandboxList, OpenShellError> {
    let v: Value = serde_json::from_str(text).map_err(|e| OpenShellError::BadJson(e.to_string()))?;
    Ok(SandboxList { sandboxes: v.get("sandboxes").and_then(Value::as_array).cloned(), more: truthy(v.get("next_page_token")) })
}

impl SandboxList {
    /// True only when the listing is complete and empty, which is the one
    /// answer that shows a sandbox is gone.
    pub fn confirms_none(&self) -> bool {
        self.sandboxes.as_ref().is_some_and(Vec::is_empty) && !self.more
    }

    /// Each entry's name and phase.
    pub fn entries(&self) -> Vec<(Option<String>, Option<Value>)> {
        self.sandboxes.iter().flatten().map(|s| (name_of(s), phase_of(s))).collect()
    }
}

fn name_of(v: &Value) -> Option<String> {
    [v.get("name"), v.get("sandbox").and_then(|s| s.get("name")), v.get("metadata").and_then(|s| s.get("name"))].into_iter().flatten().find_map(|n| n.as_str().map(str::to_owned))
}

/// A sandbox's phase, wherever this OpenShell version puts it: at the top,
/// under `sandbox`, or under `status`.
pub fn phase_of(v: &Value) -> Option<Value> {
    [v.get("phase"), v.get("sandbox").and_then(|x| x.get("phase")), v.get("status").and_then(|x| x.get("phase"))].into_iter().flatten().find(|p| !p.is_null()).cloned()
}

/// True when OpenShell's stderr says a policy denied the operation. The
/// word must stand alone, so `policy_denied_by_x` does not count.
pub fn policy_denied(stderr: &str) -> bool {
    const WORD: &str = "policy_denied";
    stderr.match_indices(WORD).any(|(i, _)| {
        let before = stderr[..i].chars().next_back();
        let after = stderr[i + WORD.len()..].chars().next();
        let inside = |c: char| c.is_ascii_alphanumeric() || c == '_';
        !before.is_some_and(inside) && !after.is_some_and(inside)
    })
}
