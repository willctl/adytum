//! Lima's defaults mount the home folder, forward every port a guest
//! listens on, copy the host user and pass the host's proxy variables on.
//! The template turns each of those off.

use crate::Error;
use crate::{Arch, CellSpec, DriverKind};

use crate::lima::guest::provision_script;

use crate::lima::network::lima_network_name;

/// Lima's admin user, in place of the host user's name.
const GUEST_USER: &str = "adytum";

const IGNORE_GUEST_IPS: [&str; 3] = ["127.0.0.1", "0.0.0.0", "::"];

fn quoted(text: &str) -> String {
    // A JSON string is also a YAML double-quoted string.
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::Aarch64 => "aarch64",
        Arch::X86_64 => "x86_64",
    }
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported { what: what.to_owned() }
}

/// Re-checks the rules that guard the host, so a driver called without
/// [`Cells`](crate::Cells) still cannot publish a port or mount a folder.
pub(crate) fn render(spec: &CellSpec) -> Result<String, Error> {
    crate::policy::check_spec(spec)?;
    if spec.driver != DriverKind::Lima {
        return Err(unsupported("a cell for another driver"));
    }
    if spec.clone_from.is_some() {
        return Err(unsupported("cloning a cell"));
    }

    let mut out = String::new();
    let mut line = |text: &str| {
        out.push_str(text);
        out.push('\n');
    };
    line("# Written by adytum from a cell spec. Edit the spec, not this file.");
    line("minimumLimaVersion: 2.0.0");
    line("vmType: vz");
    line(&format!("arch: {}", arch_name(spec.image.arch)));
    line("images:");
    line(&format!("- location: {}", quoted(&spec.image.location)));
    line(&format!("  arch: {}", arch_name(spec.image.arch)));
    line(&format!("  digest: {}", quoted(&spec.image.digest)));
    line(&format!("cpus: {}", spec.resources.cpus));
    line(&format!("memory: {}MiB", spec.resources.memory_mib));
    line(&format!("disk: {}GiB", spec.resources.disk_gib));

    // A sealed cell always gets an empty list: left out, Lima mounts the
    // home folder.
    let mounts: &[_] = if spec.sealed { &[] } else { &spec.mounts };
    if mounts.is_empty() {
        line("mounts: []");
    } else {
        line("mounts:");
        for m in mounts {
            let source = m.source.to_string_lossy();
            if !m.source.is_absolute() || !m.target.starts_with('/') || source.contains("..") || m.target.contains("..") {
                return Err(unsupported("a mount whose folder or target is not an absolute path without .."));
            }
            line(&format!("- location: {}", quoted(&source)));
            line(&format!("  mountPoint: {}", quoted(&m.target)));
            line(&format!("  writable: {}", m.writable));
        }
    }

    line("containerd:");
    line("  system: false");
    line("  user: false");
    line("ssh:");
    line("  forwardAgent: false");
    line("  loadDotSSHPubKeys: false");
    line("propagateProxyEnv: false");
    // No line at all without a network: that is Lima's own default.
    if let Some(name) = network_for(spec) {
        line("networks:");
        line(&format!("- lima: {name}"));
    }
    line("user:");
    line(&format!("  name: {GUEST_USER}"));
    line("  comment: \"\"");

    line("provision:");
    line("- mode: system");
    line("  script: |");
    for script_line in provision_script().lines() {
        if script_line.is_empty() {
            line("");
        } else {
            line(&format!("    {script_line}"));
        }
    }

    // Lima takes the first rule that matches: the spec's ports, then an
    // ignore for every other port, so Lima's own forward-all never matches.
    line("portForwards:");
    for p in &spec.ports {
        line(&format!("- guestPort: {}", p.guest));
        line(&format!("  hostPort: {}", p.host));
        line(&format!("  hostIP: {}", quoted(&p.bind.to_string())));
        line("  proto: tcp");
    }
    for guest_ip in IGNORE_GUEST_IPS {
        line(&format!("- guestIP: {}", quoted(guest_ip)));
        line("  proto: any");
        line("  guestPortRange: [1, 65535]");
        line("  ignore: true");
    }
    Ok(out)
}

/// The clone's CPUs, memory, mounts and port rules replace the source's, so
/// it never keeps a host port or folder only the source named. Disk stays:
/// Lima can grow a disk but not shrink one.
pub(crate) fn clone_edits(spec: &CellSpec) -> Result<Vec<String>, Error> {
    crate::policy::check_spec(spec)?;
    if spec.driver != DriverKind::Lima {
        return Err(unsupported("a cell for another driver"));
    }
    let mut mounts = Vec::new();
    for m in if spec.sealed { &[][..] } else { &spec.mounts[..] } {
        let source = m.source.to_string_lossy();
        if !m.source.is_absolute() || !m.target.starts_with('/') || source.contains("..") || m.target.contains("..") {
            return Err(unsupported("a mount whose folder or target is not an absolute path without .."));
        }
        mounts.push(serde_json::json!({ "location": source, "mountPoint": m.target, "writable": m.writable }));
    }
    let mut forwards = Vec::new();
    for p in &spec.ports {
        forwards.push(serde_json::json!({ "guestPort": p.guest, "hostPort": p.host, "hostIP": p.bind.to_string(), "proto": "tcp" }));
    }
    for guest_ip in IGNORE_GUEST_IPS {
        forwards.push(serde_json::json!({ "guestIP": guest_ip, "proto": "any", "guestPortRange": [1, 65535], "ignore": true }));
    }
    let mut edits = vec![
        format!(".cpus = {}", spec.resources.cpus),
        format!(".memory = \"{}MiB\"", spec.resources.memory_mib),
        format!(".mounts = {}", serde_json::Value::Array(mounts)),
        format!(".portForwards = {}", serde_json::Value::Array(forwards)),
    ];
    if let Some(name) = network_for(spec) {
        edits.push(format!(".networks = {}", serde_json::json!([{ "lima": name }])));
    }
    Ok(edits)
}

/// Made from the cell's own wall, so a cell only ever joins its wall's network.
pub(crate) fn network_for(spec: &CellSpec) -> Option<String> {
    spec.network.as_ref().map(|n| lima_network_name(&spec.wall, n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImagePin, Mount, Port, Resources, Wall};
    use std::path::PathBuf;

    fn spec(sealed: bool) -> CellSpec {
        CellSpec {
            name: "adytum-test-tpl".parse().unwrap(),
            driver: DriverKind::Lima,
            image: ImagePin::new(
                "https://cloud-images.ubuntu.com/releases/noble/release-20260518/ubuntu-24.04-server-cloudimg-arm64.img".into(),
                "sha256:6a61b967ba4a27dd1966f835a67643073ed55c2860ce3dc1cb0517282e6b8bec".into(),
                Arch::Aarch64,
            )
            .unwrap(),
            wall: "wall_a".parse::<Wall>().unwrap(),
            resources: Resources::default(),
            ports: vec![],
            automation_port: None,
            mounts: vec![],
            sealed,
            network: None,
            clone_from: None,
        }
    }

    fn mount() -> Mount {
        Mount { source: PathBuf::from("/srv/share"), target: "/mnt/share".into(), writable: false }
    }

    #[test]
    fn a_sealed_template_has_empty_mounts_and_the_ignore_rules_last() {
        let text = render(&spec(true)).unwrap();
        assert!(text.contains("\nmounts: []\n"), "{text}");
        assert!(text.ends_with("- guestIP: \"::\"\n  proto: any\n  guestPortRange: [1, 65535]\n  ignore: true\n"), "{text}");
        // 127.0.0.1 has its own rule: Lima's 0.0.0.0 rule alone does not match it.
        let rules: Vec<_> = text.lines().filter(|l| l.starts_with("- guestIP")).collect();
        assert_eq!(rules, ["- guestIP: \"127.0.0.1\"", "- guestIP: \"0.0.0.0\"", "- guestIP: \"::\""]);
    }

    #[test]
    fn a_sealed_spec_that_smuggles_a_mount_is_refused_and_writes_nothing() {
        let mut s = spec(true);
        s.mounts.push(mount());
        let Err(Error::Refused(r)) = render(&s) else { panic!("not refused") };
        assert_eq!(r.code, crate::RefusalCode::SealedCellMount);
    }

    #[test]
    fn an_unsealed_cell_gets_only_its_own_mounts() {
        let mut s = spec(false);
        s.mounts.push(mount());
        let text = render(&s).unwrap();
        assert!(text.contains("mounts:\n- location: \"/srv/share\"\n  mountPoint: \"/mnt/share\"\n  writable: false\n"), "{text}");
        assert!(!text.contains("mounts: []"));
    }

    #[test]
    fn published_ports_come_before_the_ignore_rules_and_bind_to_localhost() {
        let mut s = spec(true);
        s.ports.push(Port { guest: 8080, host: 18080, bind: "127.0.0.1".parse().unwrap() });
        let text = render(&s).unwrap();
        let port_at = text.find("guestPort: 8080").unwrap();
        let first_ignore = text.find("ignore: true").unwrap();
        assert!(port_at < first_ignore);
        assert!(text.contains("  hostPort: 18080\n  hostIP: \"127.0.0.1\"\n  proto: tcp\n"));
    }

    #[test]
    fn a_port_on_another_address_is_refused() {
        let mut s = spec(true);
        s.ports.push(Port { guest: 80, host: 8080, bind: "0.0.0.0".parse().unwrap() });
        let Err(Error::Refused(r)) = render(&s) else { panic!("not refused") };
        assert_eq!(r.code, crate::RefusalCode::PortNotLocalhost);
    }

    #[test]
    fn the_automation_port_is_never_forwarded() {
        let mut s = spec(true);
        s.automation_port = Some(9222);
        let text = render(&s).unwrap();
        assert!(!text.contains("9222"));
        s.ports.push(Port { guest: 9222, host: 9222, bind: "127.0.0.1".parse().unwrap() });
        let Err(Error::Refused(r)) = render(&s) else { panic!("not refused") };
        assert_eq!(r.code, crate::RefusalCode::AutomationPortPublished);
    }

    #[test]
    fn the_host_user_is_not_copied_into_the_guest() {
        let text = render(&spec(true)).unwrap();
        assert!(text.contains("user:\n  name: adytum\n  comment: \"\"\n"));
        assert!(text.contains("propagateProxyEnv: false"));
        assert!(text.contains("forwardAgent: false"));
        assert!(text.contains("containerd:\n  system: false\n  user: false\n"));
    }

    #[test]
    fn every_template_provisions_the_exec_user_and_the_firewall_as_root_before_the_ignore_rules() {
        for sealed in [true, false] {
            let text = render(&spec(sealed)).unwrap();
            let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle:?} is missing:\n{text}"));
            assert!(text.contains("\nprovision:\n- mode: system\n  script: |\n    #!/bin/sh\n"), "{text}");
            assert!(at("provision:") < at("portForwards:"));
            assert!(text.contains("      echo \"    ip daddr { $hosts } drop\"\n"));
            // The block scalar keeps every script line at one indentation or deeper.
            let script = &text[at("  script: |\n")..at("portForwards:")];
            assert!(script.lines().skip(1).all(|l| l.is_empty() || l.starts_with("    ")), "{script}");
        }
    }

    #[test]
    fn the_host_drop_comes_before_any_accept_of_other_addresses() {
        let text = render(&spec(true)).unwrap();
        let host_drop = text.find("ip daddr { $hosts } drop").unwrap();
        assert!(host_drop < text.find("ip daddr { $own } accept").unwrap());
    }

    #[test]
    fn what_lima_cannot_honour_is_refused_not_dropped() {
        let mut s = spec(true);
        s.clone_from = Some("adytum-other".parse().unwrap());
        assert!(matches!(render(&s), Err(Error::Unsupported { .. })));
    }

    #[test]
    fn clone_edits_replace_the_sources_ports_mounts_and_size() {
        let mut s = spec(false);
        s.ports.push(Port { guest: 8080, host: 18080, bind: "127.0.0.1".parse().unwrap() });
        s.mounts.push(mount());
        let edits = clone_edits(&s).unwrap();
        assert_eq!(edits[0], ".cpus = 2");
        assert_eq!(edits[1], ".memory = \"2048MiB\"");
        assert_eq!(edits[2], r#".mounts = [{"location":"/srv/share","mountPoint":"/mnt/share","writable":false}]"#);
        assert!(edits[3].starts_with(r#".portForwards = [{"guestPort":8080,"hostPort":18080,"hostIP":"127.0.0.1","proto":"tcp"},"#), "{}", edits[3]);
        assert!(edits[3].ends_with(r#""guestPortRange":[1,65535],"ignore":true}]"#), "{}", edits[3]);
        // A sealed spec that lists a mount is refused; one without has none.
        let mut sealed = spec(true);
        sealed.mounts.push(mount());
        assert!(matches!(clone_edits(&sealed), Err(Error::Refused(_))));
        assert_eq!(clone_edits(&spec(true)).unwrap()[2], ".mounts = []");
    }

    #[test]
    fn clone_edits_refuse_what_render_refuses() {
        let mut s = spec(true);
        s.ports.push(Port { guest: 80, host: 8080, bind: "0.0.0.0".parse().unwrap() });
        let Err(Error::Refused(r)) = clone_edits(&s) else { panic!("not refused") };
        assert_eq!(r.code, crate::RefusalCode::PortNotLocalhost);
    }

    fn on_network(wall: &str, network: &str) -> CellSpec {
        let mut s = spec(true);
        s.wall = wall.parse().unwrap();
        s.network = Some(network.parse().unwrap());
        s
    }

    #[test]
    fn a_cell_with_no_network_gets_no_networks_line() {
        let text = render(&spec(true)).unwrap();
        assert!(!text.contains("networks"), "{text}");
        assert_eq!(network_for(&spec(true)), None);
        assert!(clone_edits(&spec(true)).unwrap().iter().all(|e| !e.contains("networks")));
    }

    #[test]
    fn a_cell_joins_the_lima_network_made_from_its_own_wall_and_network() {
        let s = on_network("wall_a", "net-1");
        let name = network_for(&s).unwrap();
        assert_eq!(name, lima_network_name(&"wall_a".parse().unwrap(), &"net-1".parse().unwrap()));
        let text = render(&s).unwrap();
        assert!(text.contains(&format!("\nnetworks:\n- lima: {name}\n")), "{text}");
        assert_eq!(text.matches("lima:").count(), 1, "{text}");
    }

    #[test]
    fn the_same_network_name_behind_two_walls_never_gives_one_lima_network() {
        let (a, b) = (on_network("wall_a", "net-1"), on_network("wall_b", "net-1"));
        let (na, nb) = (network_for(&a).unwrap(), network_for(&b).unwrap());
        assert_ne!(na, nb);
        assert!(!render(&a).unwrap().contains(&nb));
        assert!(!render(&b).unwrap().contains(&na));
    }

    #[test]
    fn a_networked_sealed_cell_still_has_no_mounts_and_ignores_every_port() {
        let mut s = on_network("wall_a", "net-1");
        let text = render(&s).unwrap();
        assert!(text.contains("\nmounts: []\n"), "{text}");
        assert!(text.ends_with("  ignore: true\n"), "{text}");
        assert_eq!(text.matches("ignore: true").count(), 3);
        s.mounts.push(mount());
        let Err(Error::Refused(r)) = render(&s) else { panic!("not refused") };
        assert_eq!(r.code, crate::RefusalCode::SealedCellMount);
    }

    #[test]
    fn a_network_does_not_open_a_port_to_the_host() {
        let mut s = on_network("wall_a", "net-1");
        s.ports.push(Port { guest: 80, host: 8080, bind: "0.0.0.0".parse().unwrap() });
        let Err(Error::Refused(r)) = render(&s) else { panic!("not refused") };
        assert_eq!(r.code, crate::RefusalCode::PortNotLocalhost);
        let text = render(&on_network("wall_a", "net-1")).unwrap();
        assert!(!text.contains("hostPort"), "{text}");
    }

    #[test]
    fn a_clone_is_put_on_its_own_walls_network() {
        let edits = clone_edits(&on_network("wall_a", "net-1")).unwrap();
        let name = network_for(&on_network("wall_a", "net-1")).unwrap();
        assert_eq!(edits.last().unwrap(), &format!(r#".networks = [{{"lima":"{name}"}}]"#));
    }
}
