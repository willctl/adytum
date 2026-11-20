//! A named network is a Lima `user-v2` switch inside the host agent, so it
//! needs no root. Its name is a hash of the wall and the network name, so
//! one network name behind two walls is two switches.

use crate::{NetworkName, Wall};
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

const PREFIX: &str = "adytum-";
const HASH_CHARS: usize = 20;

fn digest(wall: &Wall, network: &NetworkName) -> [u8; 32] {
    let mut sha = Sha256::new();
    sha.update(wall.as_str().as_bytes());
    // Neither name can hold a NUL, so the pair is unambiguous.
    sha.update([0]);
    sha.update(network.as_str().as_bytes());
    sha.finalize().into()
}

/// Short, because Lima keeps a unix socket under a folder of this name.
pub(crate) fn lima_network_name(wall: &Wall, network: &NetworkName) -> String {
    let mut name = String::from(PREFIX);
    for byte in digest(wall, network).iter().take(HASH_CHARS / 2) {
        let _ = write!(name, "{byte:02x}");
    }
    name
}

pub(crate) fn is_ours(name: &str) -> bool {
    name.strip_prefix(PREFIX).is_some_and(|h| h.len() == HASH_CHARS && h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
}

/// `10.<200-250>.<0-255>.1`: the subnet exists only inside the switch, and
/// a range home and office networks rarely use keeps it from shadowing a
/// real one in the guest. `shift` moves on past a gateway already held.
pub(crate) fn candidate_gateway(wall: &Wall, network: &NetworkName, shift: u8) -> String {
    let d = digest(wall, network);
    let second = 200 + d[20] % 51;
    let third = d[21].wrapping_add(shift);
    format!("10.{second}.{third}.1")
}

/// `(name, gateway)` for each line of `limactl network list`, the gateway
/// without its `/24`.
pub(crate) fn parse_network_list(text: &str) -> Vec<(String, String)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let name = cols.next()?;
            let _mode = cols.next()?;
            let gateway = cols.next()?.split('/').next()?;
            Some((name.to_owned(), gateway.to_owned()))
        })
        .collect()
}

pub(crate) fn instance_networks(lima_yaml: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_networks = false;
    for line in lima_yaml.lines() {
        if line.starts_with("networks:") {
            in_networks = true;
            continue;
        }
        if !in_networks {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('-') {
            in_networks = false;
            continue;
        }
        let item = line.trim().trim_start_matches('-').trim();
        if let Some(name) = item.strip_prefix("lima:") {
            let name = name.trim().trim_matches(|c| c == '"' || c == '\'');
            if is_ours(name) {
                found.push(name.to_owned());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(s: &str) -> Wall {
        s.parse().unwrap()
    }
    fn net(s: &str) -> NetworkName {
        s.parse().unwrap()
    }

    #[test]
    fn the_same_network_name_behind_two_walls_is_two_lima_networks() {
        let a = lima_network_name(&wall("wall_a"), &net("net-1"));
        let b = lima_network_name(&wall("wall_b"), &net("net-1"));
        assert_ne!(a, b);
        assert_eq!(a, lima_network_name(&wall("wall_a"), &net("net-1")));
        assert!(is_ours(&a) && is_ours(&b));
    }

    #[test]
    fn a_dash_in_the_wall_or_the_network_cannot_make_two_pairs_collide() {
        let one = lima_network_name(&wall("wall_a-b"), &net("c"));
        let two = lima_network_name(&wall("wall_a"), &net("b-c"));
        assert_ne!(one, two);
    }

    #[test]
    fn a_lima_network_name_is_short_enough_for_a_socket_path() {
        assert_eq!(lima_network_name(&wall("wall_a"), &net("net-1")).len(), PREFIX.len() + HASH_CHARS);
    }

    #[test]
    fn only_names_adytum_makes_are_ours() {
        for other in ["user-v2", "shared", "bridged", "host", "adytum-", "adytum-xyz", "adytum-0123456789ABCDEF0123", "adytum-0123456789abcdef012"] {
            assert!(!is_ours(other), "{other}");
        }
        assert!(is_ours("adytum-0123456789abcdef0123"));
    }

    #[test]
    fn the_gateway_is_a_private_address_and_moves_on_when_asked() {
        let (w, n) = (wall("wall_a"), net("net-1"));
        let first = candidate_gateway(&w, &n, 0);
        assert!(first.starts_with("10.2") && first.ends_with(".1"), "{first}");
        assert_ne!(first, candidate_gateway(&w, &n, 1));
    }

    #[test]
    fn the_network_list_is_read_by_column() {
        let text = "NAME       MODE       GATEWAY             INTERFACE\nadytum-a   user-v2    10.201.4.1/24       -\nbridged    bridged    -                   en0\n";
        assert_eq!(parse_network_list(text), [("adytum-a".to_owned(), "10.201.4.1".to_owned()), ("bridged".to_owned(), "-".to_owned())]);
    }

    #[test]
    fn an_instance_file_names_the_adytum_networks_it_joins() {
        let yaml = "cpus: 2\nnetworks:\n- lima: adytum-0123456789abcdef0123\n- lima: user-v2\nportForwards:\n- lima: adytum-aaaaaaaaaaaaaaaaaaaa\n";
        assert_eq!(instance_networks(yaml), ["adytum-0123456789abcdef0123"]);
        assert_eq!(instance_networks("cpus: 2\nnetworks: []\n").len(), 0);
    }
}
