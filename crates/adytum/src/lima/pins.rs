use crate::{Arch, ImagePin};

const PINS: &str = include_str!("../../pins.json");

pub const DEFAULT_IMAGE: &str = "ubuntu-24.04";

fn all() -> Vec<(String, ImagePin)> {
    // pins.json is compiled in and checked by a test.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(PINS) else { return Vec::new() };
    let Some(images) = value.get("images").and_then(serde_json::Value::as_object) else { return Vec::new() };
    images.iter().filter_map(|(name, pin)| serde_json::from_value::<ImagePin>(pin.clone()).ok().map(|p| (name.clone(), p))).collect()
}

/// A location and the digest Lima checks the download against.
pub fn image(name: &str) -> Option<ImagePin> {
    all().into_iter().find(|(n, _)| n == name).map(|(_, p)| p)
}

pub fn image_names() -> Vec<String> {
    all().into_iter().map(|(n, _)| n).collect()
}

/// The gVisor release a Lima cell that holds containers installs: its
/// archive, and the sha256 the guest checks it against before unpacking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GvisorPin {
    pub version: String,
    pub location: String,
    pub digest: String,
}

pub fn gvisor(arch: Arch) -> Option<GvisorPin> {
    let value = serde_json::from_str::<serde_json::Value>(PINS).ok()?;
    let gvisor = value.get("gvisor")?;
    let key = match arch {
        Arch::Aarch64 => "aarch64",
        Arch::X86_64 => "x86_64",
    };
    let text = |v: &serde_json::Value, k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::to_owned);
    let pin = gvisor.get(key)?;
    let (version, location, digest) = (text(gvisor, "version")?, text(pin, "location")?, text(pin, "digest")?);
    // The same checks an image pin gets.
    ImagePin::new(location.clone(), digest.clone(), arch).ok()?;
    let version_ok = !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.');
    version_ok.then_some(GvisorPin { version, location, digest })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pin_in_the_file_parses_with_a_well_formed_digest() {
        let value: serde_json::Value = serde_json::from_str(PINS).unwrap();
        let images = value["images"].as_object().unwrap();
        assert!(!images.is_empty());
        for (name, pin) in images {
            let parsed: ImagePin = serde_json::from_value(pin.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let hex = parsed.digest.strip_prefix("sha256:").unwrap();
            assert_eq!(hex.len(), 64, "{name}");
            assert!(hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{name}");
            assert!(parsed.location.starts_with("https://"), "{name}: a pin is fetched over https");
        }
        assert_eq!(all().len(), images.len(), "a pin failed to load");
    }

    #[test]
    fn gvisor_is_pinned_for_the_default_images_arch_by_version_and_sha256() {
        let pin = gvisor(Arch::Aarch64).unwrap();
        assert!(pin.location.starts_with("https://storage.googleapis.com/gvisor/releases/release/"), "{pin:?}");
        assert!(pin.location.contains(&format!("/{}/aarch64/", pin.version)), "{pin:?}");
        assert_eq!(pin.digest.len(), "sha256:".len() + 64);
        assert_eq!(gvisor(image(DEFAULT_IMAGE).unwrap().arch).as_ref(), Some(&pin));
        assert!(gvisor(Arch::X86_64).is_none());
    }

    #[test]
    fn the_default_image_is_pinned() {
        assert!(image(DEFAULT_IMAGE).is_some());
        assert!(image("nonesuch").is_none());
        assert!(image_names().contains(&DEFAULT_IMAGE.to_owned()));
    }
}
