use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    PortNotLocalhost,
    /// The automation port is published.
    AutomationPortPublished,
    SealedCellMount,
    NameNotOurs,
    NameTaken,
    WallNetwork,
    WallMount,
    WallClone,
    CloneOpenEidolon,
    /// An OpenShell policy file changed after it was previewed.
    PolicyChanged,
}

impl RefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PortNotLocalhost => "port_not_localhost",
            Self::AutomationPortPublished => "automation_port_published",
            Self::SealedCellMount => "sealed_cell_mount",
            Self::NameNotOurs => "name_not_ours",
            Self::NameTaken => "name_taken",
            Self::WallNetwork => "wall_network",
            Self::WallMount => "wall_mount",
            Self::WallClone => "wall_clone",
            Self::CloneOpenEidolon => "clone_open_eidolon",
            Self::PolicyChanged => "policy_changed",
        }
    }
}

/// A rule said no: a code to match on, what was refused and why, and what
/// to change. Generic over the code so other tools keep their own rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal<C = RefusalCode> {
    pub code: C,
    pub message: String,
    pub fix: String,
}

impl<C> Refusal<C> {
    pub fn new(code: C, message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self { code, message: message.into(), fix: fix.into() }
    }
}

impl<C> fmt::Display for Refusal<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.message, self.fix)
    }
}

impl<C: fmt::Debug> std::error::Error for Refusal<C> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_travel_as_their_stable_names() {
        let pairs = [
            (RefusalCode::PortNotLocalhost, "port_not_localhost"),
            (RefusalCode::AutomationPortPublished, "automation_port_published"),
            (RefusalCode::SealedCellMount, "sealed_cell_mount"),
            (RefusalCode::NameNotOurs, "name_not_ours"),
            (RefusalCode::NameTaken, "name_taken"),
            (RefusalCode::WallNetwork, "wall_network"),
            (RefusalCode::WallMount, "wall_mount"),
            (RefusalCode::WallClone, "wall_clone"),
            (RefusalCode::CloneOpenEidolon, "clone_open_eidolon"),
            (RefusalCode::PolicyChanged, "policy_changed"),
        ];
        for (code, text) in pairs {
            assert_eq!(code.as_str(), text);
            assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{text}\""));
            assert_eq!(serde_json::from_str::<RefusalCode>(&format!("\"{text}\"")).unwrap(), code);
        }
        assert!(serde_json::from_str::<RefusalCode>("\"NameTaken\"").is_err());
    }

    #[test]
    fn a_refusal_reads_back_what_it_wrote_with_any_code() {
        let r = Refusal::new(RefusalCode::WallClone, "No.", "Pick a cell behind the same wall.");
        let text = serde_json::to_string(&r).unwrap();
        assert_eq!(text, r#"{"code":"wall_clone","message":"No.","fix":"Pick a cell behind the same wall."}"#);
        assert_eq!(serde_json::from_str::<Refusal>(&text).unwrap(), r);

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Theirs {
            SecondSeat,
        }
        let theirs = Refusal::new(Theirs::SecondSeat, "Open elsewhere.", "Close it there.");
        let back: Refusal<Theirs> = serde_json::from_str(&serde_json::to_string(&theirs).unwrap()).unwrap();
        assert_eq!(back, theirs);
        assert!(serde_json::from_str::<Refusal<Theirs>>(r#"{"code":"wall_clone","message":"m","fix":"f"}"#).is_err(), "a code the type lacks");
    }

    #[test]
    fn a_refusal_reads_as_its_message_then_its_fix() {
        let r = Refusal::new(RefusalCode::NameTaken, "That name is taken.", "Pick another.");
        assert_eq!(r.to_string(), "That name is taken. Pick another.");
    }
}
