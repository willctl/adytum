use std::net::IpAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::ids::CellId;
use crate::model::names::{CellName, NetworkName, Wall};
use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Lima,
    #[serde(rename = "openshell")]
    OpenShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    Aarch64,
    X86_64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawImagePin", deny_unknown_fields)]
pub struct ImagePin {
    /// An https URL or an absolute local file path.
    pub location: String,
    /// `sha256:` and 64 lowercase hex characters.
    pub digest: String,
    pub arch: Arch,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImagePin {
    location: String,
    digest: String,
    arch: Arch,
}

impl ImagePin {
    pub fn new(location: String, digest: String, arch: Arch) -> Result<Self, Error> {
        let url = location.strip_prefix("https://").is_some_and(|rest| !rest.is_empty());
        let path = location.starts_with('/') && location.len() > 1;
        if !(url || path) || location.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(Error::Invalid { what: "image location", rule: "an https URL or an absolute file path, without spaces" });
        }
        let hex = digest.strip_prefix("sha256:").unwrap_or("");
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(Error::Invalid { what: "image digest", rule: "sha256: and 64 lowercase hex characters" });
        }
        Ok(Self { location, digest, arch })
    }
}

impl TryFrom<RawImagePin> for ImagePin {
    type Error = Error;

    fn try_from(raw: RawImagePin) -> Result<Self, Error> {
        Self::new(raw.location, raw.digest, raw.arch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub guest: u16,
    pub host: u16,
    pub bind: IpAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub source: PathBuf,
    pub target: String,
    pub writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    pub cpus: u8,
    pub memory_mib: u32,
    pub disk_gib: u32,
}

impl Default for Resources {
    fn default() -> Self {
        Self { cpus: 2, memory_mib: 2048, disk_gib: 10 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellSpec {
    pub name: CellName,
    pub driver: DriverKind,
    pub image: ImagePin,
    pub wall: Wall,
    pub resources: Resources,
    pub ports: Vec<Port>,
    pub mounts: Vec<Mount>,
    /// A sealed cell has nothing from the host.
    pub sealed: bool,
    pub network: Option<NetworkName>,
    pub clone_from: Option<CellName>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellState {
    Creating,
    Stopped,
    Running,
    Removing,
    /// Not in a state Adytum can use.
    Broken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cell {
    pub id: CellId,
    pub spec: CellSpec,
    pub state: CellState,
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../../tests/golden/cell-spec.json");

    #[test]
    fn golden_spec_round_trips_byte_for_byte() {
        let spec: CellSpec = serde_json::from_str(GOLDEN).unwrap();
        let mut out = serde_json::to_string_pretty(&spec).unwrap();
        out.push('\n');
        assert_eq!(out, GOLDEN);
    }

    #[test]
    fn unknown_fields_are_refused() {
        let with_extra = GOLDEN.replacen("{\n", "{\n  \"extra\": 1,\n", 1);
        assert!(serde_json::from_str::<CellSpec>(&with_extra).is_err());
    }

    #[test]
    fn a_cell_name_outside_ours_is_refused_in_json() {
        let bad = GOLDEN.replacen("adytum-demo", "pane", 1);
        assert!(serde_json::from_str::<CellSpec>(&bad).is_err());
    }

    #[test]
    fn image_pin_checks_location_and_digest() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let ok = |l: &str, d: &str| ImagePin::new(l.into(), d.into(), Arch::X86_64);
        assert!(ok("https://example.test/a.img", &digest).is_ok());
        assert!(ok("/var/images/a.img", &digest).is_ok());
        assert!(ok("http://example.test/a.img", &digest).is_err());
        assert!(ok("a.img", &digest).is_err());
        assert!(ok("https://example.test/a b", &digest).is_err());
        assert!(ok("https://example.test/a.img", "sha256:ab").is_err());
        assert!(ok("https://example.test/a.img", &digest.to_uppercase()).is_err());
        assert!(ok("https://example.test/a.img", &format!("md5:{}", "ab".repeat(32))).is_err());
    }

    #[test]
    fn resources_default() {
        assert_eq!(Resources::default(), Resources { cpus: 2, memory_mib: 2048, disk_gib: 10 });
    }

    #[test]
    fn states_and_drivers_are_snake_case() {
        assert_eq!(serde_json::to_string(&CellState::Removing).unwrap(), "\"removing\"");
        assert_eq!(serde_json::to_string(&DriverKind::Lima).unwrap(), "\"lima\"");
        assert_eq!(serde_json::to_string(&Arch::X86_64).unwrap(), "\"x86_64\"");
    }
}
