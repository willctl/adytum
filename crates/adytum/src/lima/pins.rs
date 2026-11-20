use crate::ImagePin;

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
    fn the_default_image_is_pinned() {
        assert!(image(DEFAULT_IMAGE).is_some());
        assert!(image("nonesuch").is_none());
        assert!(image_names().contains(&DEFAULT_IMAGE.to_owned()));
    }
}
