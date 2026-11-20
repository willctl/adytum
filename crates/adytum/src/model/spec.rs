use std::net::IpAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::ids::CellId;
use crate::model::names::{CellName, NetworkName, OciImage, Wall};
use crate::Error;

/// What a cell is, with what that kind of cell needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Lima(LimaOptions),
    Container(ContainerOptions),
    /// The sandbox's image and policy are given to the driver.
    #[serde(rename = "openshell")]
    OpenShell,
}

/// A kind without its options: what picks a driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindName {
    Lima,
    Container,
    #[serde(rename = "openshell")]
    OpenShell,
}

impl Kind {
    pub fn name(&self) -> KindName {
        match self {
            Kind::Lima(_) => KindName::Lima,
            Kind::Container(_) => KindName::Container,
            Kind::OpenShell => KindName::OpenShell,
        }
    }

    /// Whether a cell of this kind can run a cell of `child`'s kind inside it.
    pub fn hosts(&self, child: KindName) -> bool {
        matches!((self, child), (Kind::Lima(options), KindName::Container) if options.containers)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimaOptions {
    pub image: ImagePin,
    /// Runs containerd and nerdctl as root in the guest, so the cell can
    /// hold container cells.
    #[serde(default)]
    pub containers: bool,
    /// Lets the guest run VMs of its own. Lima does it with vz on an M3 or
    /// later chip.
    #[serde(default)]
    pub nested_virtualization: bool,
    /// A virtual display for the cell's user, and a screen share on the
    /// guest's loopback that `adytum view` opens one session at a time.
    #[serde(default)]
    pub desktop: bool,
}

impl LimaOptions {
    pub fn new(image: ImagePin) -> LimaOptions {
        LimaOptions { image, containers: false, nested_virtualization: false, desktop: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerOptions {
    pub image: OciImage,
    #[serde(default)]
    pub runtime: Runtime,
}

impl ContainerOptions {
    pub fn new(image: OciImage) -> ContainerOptions {
        ContainerOptions { image, runtime: Runtime::default() }
    }
}

/// What runs a container. gVisor gives it a kernel of its own in user
/// space, so a flaw in the parent's kernel is not one step away; runc runs
/// it on the parent's kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    #[default]
    Gvisor,
    Runc,
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
    pub kind: Kind,
    /// The cell this one runs in; `None` is this machine.
    pub parent: Option<CellName>,
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
    fn a_container_spec_reads_back_what_it_wrote() {
        let mut spec: CellSpec = serde_json::from_str(GOLDEN).unwrap();
        spec.kind = Kind::Container(ContainerOptions::new(format!("docker.io/library/alpine:3.22@sha256:{}", "ab".repeat(32)).parse().unwrap()));
        spec.parent = Some("adytum-host".parse().unwrap());
        let text = serde_json::to_string(&spec).unwrap();
        assert!(text.contains(r#""kind":{"container":{"image":"docker.io/library/alpine:3.22@sha256:abab"#) && text.contains(r#""runtime":"gvisor""#), "{text}");
        assert_eq!(serde_json::from_str::<CellSpec>(&text).unwrap(), spec);
        spec.kind = Kind::OpenShell;
        assert!(serde_json::to_string(&spec).unwrap().contains(r#""kind":"openshell""#));
    }

    #[test]
    fn only_a_lima_cell_that_runs_containers_hosts_one() {
        let pin = ImagePin::new("https://example.test/a.img".into(), format!("sha256:{}", "0".repeat(64)), Arch::Aarch64).unwrap();
        let mut lima = LimaOptions::new(pin);
        assert!(!Kind::Lima(lima.clone()).hosts(KindName::Container));
        lima.containers = true;
        assert!(Kind::Lima(lima.clone()).hosts(KindName::Container));
        for kind in [KindName::Lima, KindName::OpenShell] {
            assert!(!Kind::Lima(lima.clone()).hosts(kind), "{kind:?}");
        }
        let image: OciImage = format!("a@sha256:{}", "0".repeat(64)).parse().unwrap();
        assert!(!Kind::Container(ContainerOptions::new(image)).hosts(KindName::Container));
        assert!(!Kind::OpenShell.hosts(KindName::Container));
    }

    #[test]
    fn states_and_kinds_are_snake_case() {
        assert_eq!(serde_json::to_string(&CellState::Removing).unwrap(), "\"removing\"");
        assert_eq!(serde_json::to_string(&KindName::OpenShell).unwrap(), "\"openshell\"");
        assert_eq!(serde_json::to_string(&Arch::X86_64).unwrap(), "\"x86_64\"");
    }
}
