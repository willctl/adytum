use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{invalid, ModelError};

/// An IPv4 address with an optional prefix length, such as `203.0.113.7` or
/// `198.51.100.0/24`. A bare address means `/32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Ipv4Net {
    addr: Ipv4Addr,
    prefix: u8,
}

impl Ipv4Net {
    /// The address part.
    pub fn addr(&self) -> Ipv4Addr {
        self.addr
    }

    /// The prefix length, 0 to 32.
    pub fn prefix(&self) -> u8 {
        self.prefix
    }
}

impl FromStr for Ipv4Net {
    type Err = ModelError;

    fn from_str(text: &str) -> Result<Self, ModelError> {
        let rule = "a dotted IPv4 address, with /0 to /32 after it if wanted";
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, prefix.parse::<u8>().ok().filter(|p| *p <= 32).ok_or_else(|| invalid("network address", rule))?),
            None => (text, 32),
        };
        // Parsed by the standard library, so nothing but digits and dots
        // survives into a rule.
        let addr = addr.parse::<Ipv4Addr>().map_err(|_| invalid("network address", rule))?;
        Ok(Self { addr, prefix })
    }
}

impl fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl TryFrom<String> for Ipv4Net {
    type Error = ModelError;

    fn try_from(text: String) -> Result<Self, ModelError> {
        text.parse()
    }
}

impl From<Ipv4Net> for String {
    fn from(net: Ipv4Net) -> String {
        net.to_string()
    }
}

/// Where a cell's traffic may go on the internet. Whatever the choice, a
/// cell never reaches the host or anything on the host's own addresses: that
/// wall is not an option.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Egress {
    /// Any address outside the host. What provisioning (package installs)
    /// needs, so it is the default.
    #[default]
    Open,
    /// Only these addresses, plus the cell's DNS. An empty list shuts the
    /// internet.
    Only(Vec<Ipv4Net>),
}

impl Egress {
    /// Only the given addresses, each checked.
    pub fn only<S: AsRef<str>>(addresses: &[S]) -> Result<Self, ModelError> {
        addresses.iter().map(|a| a.as_ref().parse()).collect::<Result<Vec<Ipv4Net>, _>>().map(Self::Only)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_parse_with_or_without_a_prefix_and_nothing_else() {
        assert_eq!("203.0.113.7".parse::<Ipv4Net>().unwrap().to_string(), "203.0.113.7/32");
        assert_eq!("198.51.100.0/24".parse::<Ipv4Net>().unwrap().prefix(), 24);
        for bad in ["", "example.com", "1.2.3", "1.2.3.4/33", "1.2.3.4/", "1.2.3.4/x", "1.2.3.4; drop", "1.2.3.4 5.6.7.8", "::1", "1.2.3.4/24/8", "-1.2.3.4"] {
            assert!(bad.parse::<Ipv4Net>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn egress_is_open_unless_a_list_is_given_and_round_trips() {
        assert_eq!(Egress::default(), Egress::Open);
        let only = Egress::only(&["1.1.1.1", "10.0.0.0/8"]).unwrap();
        let text = serde_json::to_string(&only).unwrap();
        assert_eq!(text, r#"{"only":["1.1.1.1/32","10.0.0.0/8"]}"#);
        assert_eq!(serde_json::from_str::<Egress>(&text).unwrap(), only);
        assert_eq!(serde_json::to_string(&Egress::Open).unwrap(), r#""open""#);
        assert!(Egress::only(&["nope"]).is_err());
        assert!(serde_json::from_str::<Egress>(r#"{"only":["1.1.1.1; rm"]}"#).is_err());
    }
}
