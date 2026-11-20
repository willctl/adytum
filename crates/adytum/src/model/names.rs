use std::fmt;
use std::str::FromStr;

use crate::{Error, Refusal, RefusalCode};

fn is_lower_alnum(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

macro_rules! string_newtype {
    ($(#[$doc:meta])* $name:ident, $refuse:expr, $check:expr) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(text: &str) -> Result<Self, Error> {
                let check: fn(&str) -> Result<(), &'static str> = $check;
                let refuse: fn(&str, &'static str) -> Error = $refuse;
                check(text).map_err(|rule| refuse(text, rule))?;
                Ok(Self(text.to_owned()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = Error;

            fn try_from(text: String) -> Result<Self, Error> {
                text.parse()
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({:?})"), self.0)
            }
        }
    };
}

string_newtype!(
    /// An opaque label, `wall_` and 1 to 64 of a-z, 0-9, `_`, `-`. Adytum
    /// compares walls and never reads one.
    Wall,
    |_, rule| Error::Invalid { what: "wall", rule },
    |t| {
        let rest = t.strip_prefix("wall_").ok_or("it starts with wall_")?;
        if rest.is_empty() || rest.len() > 64 {
            return Err("1 to 64 characters after wall_");
        }
        if !rest.chars().all(|c| is_lower_alnum(c) || c == '_' || c == '-') {
            return Err("only a-z, 0-9, _ and - after wall_");
        }
        Ok(())
    }
);

string_newtype!(
    /// `adytum-` and 1 to 47 of a-z,
    /// 0-9, `-`, starting with a letter or digit and not ending in a dash.
    /// Anything else is not a cell Adytum made.
    CellName,
    |text, rule| Error::Refused(Refusal::new(
        RefusalCode::NameNotOurs,
        format!("{text:?} is not a cell Adytum made: {rule}."),
        "Adytum only touches cells named adytum-<lowercase letters, digits, dashes>."
    )),
    |t| {
        let rest = t.strip_prefix("adytum-").ok_or("it starts with adytum-")?;
        if rest.is_empty() || rest.len() > 47 {
            return Err("1 to 47 characters after adytum-");
        }
        if !rest.chars().all(|c| is_lower_alnum(c) || c == '-') {
            return Err("only a-z, 0-9 and - after adytum-");
        }
        if !rest.starts_with(is_lower_alnum) {
            return Err("a letter or digit right after adytum-");
        }
        if rest.ends_with('-') {
            return Err("no dash at the end");
        }
        Ok(())
    }
);

string_newtype!(
    /// A network's name: 1 to 48 of a-z, 0-9, `-`.
    NetworkName,
    |_, rule| Error::Invalid { what: "network name", rule },
    |t| {
        if t.is_empty() || t.len() > 48 {
            return Err("1 to 48 characters");
        }
        if !t.chars().all(|c| is_lower_alnum(c) || c == '-') {
            return Err("only a-z, 0-9 and -");
        }
        Ok(())
    }
);

string_newtype!(
    /// A user in a cell's guest, such as `root`: 1 to 32 of a-z, 0-9, `_`,
    /// `-`, starting with a letter or `_`, as `useradd` takes them.
    GuestUser,
    |_, rule| Error::Invalid { what: "guest user", rule },
    |t| {
        if t.is_empty() || t.len() > 32 {
            return Err("1 to 32 characters");
        }
        if !t.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
            return Err("a letter or _ first");
        }
        if !t.chars().all(|c| is_lower_alnum(c) || c == '_' || c == '-') {
            return Err("only a-z, 0-9, _ and -");
        }
        Ok(())
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_accepts_labels_and_refuses_the_rest() {
        assert!("wall_a".parse::<Wall>().is_ok());
        assert!("wall_Team-1_x".parse::<Wall>().is_err());
        assert!("wall_".parse::<Wall>().is_err());
        assert!("a".parse::<Wall>().is_err());
        assert!(format!("wall_{}", "a".repeat(64)).parse::<Wall>().is_ok());
        assert!(format!("wall_{}", "a".repeat(65)).parse::<Wall>().is_err());
        assert!("wall_a b".parse::<Wall>().is_err());
    }

    #[test]
    fn cell_name_matches_adytum_pattern() {
        for ok in ["adytum-a", "adytum-test-1", "adytum-0x", &format!("adytum-{}", "a".repeat(47))] {
            assert!(ok.parse::<CellName>().is_ok(), "{ok:?}");
        }
        for bad in ["pane", "xenonet-vm1", "default", "adytum-", "adytum--a", "adytum-a-", "adytum-A", "adytum-a_b", &format!("adytum-{}", "a".repeat(48))] {
            assert!(bad.parse::<CellName>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn network_name_pattern() {
        assert!("net-1".parse::<NetworkName>().is_ok());
        assert!("".parse::<NetworkName>().is_err());
        assert!("Net".parse::<NetworkName>().is_err());
        assert!("a".repeat(49).parse::<NetworkName>().is_err());
    }

    #[test]
    fn guest_user_is_a_plain_user_name() {
        for ok in ["root", "cell", "eidolon", "_apt", "www-data", "u1", &"a".repeat(32)] {
            assert!(ok.parse::<GuestUser>().is_ok(), "{ok:?}");
        }
        for bad in ["", "Root", "1user", "-u", "a b", "a:b", "a/b", "a.b", "user$", "../root", &"a".repeat(33)] {
            assert!(bad.parse::<GuestUser>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn json_is_the_plain_string_and_is_validated() {
        let n: CellName = serde_json::from_str("\"adytum-a\"").unwrap();
        assert_eq!(serde_json::to_string(&n).unwrap(), "\"adytum-a\"");
        assert!(serde_json::from_str::<CellName>("\"pane\"").is_err());
    }
}
