/// No sudo and no password, so a program in the cell cannot lift the
/// guest's firewall. Lima's admin user, which has sudo, is only the way in.
pub(crate) const EXEC_USER: &str = "cell";

/// Lima's user-mode network sends this address to the host's 127.0.0.1.
const HOST_ADDRESS: &str = "192.168.5.2";

pub(crate) fn as_user(user: &str, argv: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = ["sudo", "-n", "-u", user, "-H", "--"].iter().map(|a| (*a).to_owned()).collect();
    out.extend(argv);
    out
}

pub(crate) fn as_root(argv: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = ["sudo", "-n", "--"].iter().map(|a| (*a).to_owned()).collect();
    out.extend(argv);
    out
}

/// Runs as root at every boot, before `limactl start` returns: it makes the
/// exec user and loads the firewall. docs/design.md says why each rule is
/// there.
pub(crate) fn provision_script() -> String {
    SCRIPT.replace("@USER@", EXEC_USER).replace("@HOST@", HOST_ADDRESS)
}

const SCRIPT: &str = r#"#!/bin/sh
# Written by adytum. Runs as root at every boot.
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
user=@USER@
host=@HOST@

# The user every command runs as: no password, no sudo.
if ! id -u "$user" >/dev/null 2>&1; then
  useradd --create-home --shell /bin/bash --user-group "$user"
fi
passwd -l "$user" >/dev/null 2>&1 || true

# The firewall. Without nft there is no wall, so the cell does not start.
command -v nft >/dev/null 2>&1 || { echo "adytum: nft is missing, so the cell has no firewall" >&2; exit 1; }
install -d -m 0755 /etc/adytum

gateways=$(ip -4 route show default | awk '{ for (i = 1; i < NF; i++) if ($i == "via") print $(i + 1) }')
hosts=$(printf '%s\n' "$host" $gateways | sort -u | paste -sd, -)
resolvers=$(printf '%s\n%s\n' "$host" 192.168.5.3; awk '$1 == "nameserver" && $2 ~ /^[0-9]+[.][0-9]+[.][0-9]+[.][0-9]+$/ { print $2 }' /run/systemd/resolve/resolv.conf /etc/resolv.conf 2>/dev/null)
resolvers=$(printf '%s\n' $resolvers | grep -v '^127[.]' | sort -u | paste -sd, -)
# The subnets the cell is on: its named network and its peers, container
# bridges. The host's own address and gateway were dropped above, so a subnet
# that holds them stays closed to them.
own=$(ip -4 -o route show scope link | awk '$1 ~ /^[0-9.]+\/[0-9]+$/ { print $1 }' | sort -u | paste -sd, -)

{
  echo "table inet adytum"
  echo "delete table inet adytum"
  echo "table inet adytum {"
  echo "  chain guard {"
  echo "    ip daddr { $hosts } tcp flags & (syn | ack) != syn accept"
  echo "    ip daddr { $resolvers } udp dport 53 accept"
  echo "    ip daddr $host udp sport 68 udp dport 67 accept"
  echo "    ip daddr { $hosts } drop"
  [ -n "$own" ] && echo "    ip daddr { $own } accept"
  echo "    ip daddr { 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16, 100.64.0.0/10 } drop"
  echo "  }"
  echo "  chain output {"
  echo "    type filter hook output priority 0; policy accept;"
  echo "    oifname \"lo\" accept"
  echo "    jump guard"
  echo "  }"
  echo "  chain forward {"
  echo "    type filter hook forward priority 0; policy accept;"
  echo "    ct state established,related accept"
  echo "    jump guard"
  echo "  }"
  echo "}"
} > /etc/adytum/firewall.nft.new
mv /etc/adytum/firewall.nft.new /etc/adytum/firewall.nft
nft -f /etc/adytum/firewall.nft

cat > /etc/systemd/system/adytum-firewall.service <<'UNIT'
[Unit]
Description=Adytum cell firewall
DefaultDependencies=no
Before=network-pre.target
Wants=network-pre.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -f /etc/adytum/firewall.nft

[Install]
WantedBy=sysinit.target
UNIT
systemctl daemon-reload
systemctl enable adytum-firewall.service >/dev/null 2>&1
"#;

/// Lima starts a cell even when its boot script fails, so the script's
/// last step leaves this file and the driver looks for it after a start.
pub(crate) const READY: &str = "/run/adytum/ready";

/// What a cell runs as root at every boot: the firewall, then for a cell
/// that holds containers gVisor's runsc and containerd shim, installed once
/// from the pinned archive and checked against its sha256 before anything
/// is unpacked, then the ready file. A failed step ends the script before
/// the ready file.
pub(crate) fn boot_script(gvisor: Option<&crate::lima::GvisorPin>) -> String {
    let mut script = provision_script();
    if let Some(pin) = gvisor {
        let hex = pin.digest.strip_prefix("sha256:").unwrap_or(&pin.digest);
        script.push_str(&GVISOR.replace("@VERSION@", &pin.version).replace("@URL@", &pin.location).replace("@SHA256@", hex));
    }
    script.push_str("\n# The cell is ready only if every step above held.\ninstall -d -m 0755 /run/adytum\ntouch ");
    script.push_str(READY);
    script.push('\n');
    script
}

const GVISOR: &str = r#"
# gVisor, so a container gets a kernel of its own.
marker=/usr/local/lib/adytum/gvisor-@VERSION@
if [ ! -e "$marker" ]; then
  dir=$(mktemp -d)
  curl -fsSL --retry 3 -o "$dir/gvisor.tar.zstd" "@URL@"
  echo "@SHA256@  $dir/gvisor.tar.zstd" | sha256sum -c --quiet -
  tar -x --zstd --no-same-owner -f "$dir/gvisor.tar.zstd" -C "$dir" runsc containerd-shim-runsc-v1 gvisor-bin
  install -m 0755 "$dir/runsc" "$dir/containerd-shim-runsc-v1" /usr/local/bin/
  # runsc starts its sentry and helpers from here.
  rm -rf /usr/local/bin/gvisor-bin
  mv "$dir/gvisor-bin" /usr/local/bin/gvisor-bin
  chmod -R go-w /usr/local/bin/gvisor-bin
  rm -r "$dir"
  install -d -m 0755 /usr/local/lib/adytum
  touch "$marker"
fi
/usr/local/bin/runsc --version >/dev/null
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> String {
        provision_script()
    }

    /// Where `needle` first appears, failing with the script in view.
    fn at(script: &str, needle: &str) -> usize {
        script.find(needle).unwrap_or_else(|| panic!("{needle:?} is not in the script:\n{script}"))
    }

    #[test]
    fn the_scripts_are_valid_shell() {
        let pin = crate::lima::gvisor(crate::Arch::Aarch64).unwrap();
        for script in [boot_script(None), boot_script(Some(&pin))] {
            let mut sh = std::process::Command::new("sh").arg("-n").stdin(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
            std::io::Write::write_all(&mut sh.stdin.take().unwrap(), script.as_bytes()).unwrap();
            let out = sh.wait_with_output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    }

    #[test]
    fn gvisor_is_checked_against_its_pin_before_it_is_unpacked_and_installed_once() {
        let pin = crate::lima::gvisor(crate::Arch::Aarch64).unwrap();
        let script = boot_script(Some(&pin));
        let hex = pin.digest.strip_prefix("sha256:").unwrap();
        assert!(script.contains(&format!("echo \"{hex}  $dir/gvisor.tar.zstd\" | sha256sum -c --quiet -")), "{script}");
        assert!(script.contains(&pin.location) && script.contains(&format!("gvisor-{}", pin.version)));
        assert!(at(&script, "sha256sum -c") < at(&script, "tar -x"), "nothing is unpacked before the check");
        assert!(at(&script, "if [ ! -e \"$marker\" ]") < at(&script, "curl"));
        assert!(at(&script, "nft -f /etc/adytum/firewall.nft") < at(&script, "curl"), "the firewall comes first");
        assert!(!boot_script(None).contains("gvisor"));
    }

    #[test]
    fn the_ready_file_is_the_last_step_of_a_script_that_stops_at_the_first_failure() {
        let pin = crate::lima::gvisor(crate::Arch::Aarch64).unwrap();
        for script in [boot_script(None), boot_script(Some(&pin))] {
            assert!(script.starts_with("#!/bin/sh\n# Written by adytum. Runs as root at every boot.\nset -eu\n"), "{script}");
            assert!(script.trim_end().ends_with(&format!("touch {READY}")), "{script}");
            assert_eq!(script.matches("#!/bin/sh").count(), 1);
        }
    }

    #[test]
    fn the_shell_escapes_reach_the_guest_as_written() {
        // A `\n` that became a real newline in the source would still run, and hide the mistake.
        let script = open();
        assert!(script.contains(r"printf '%s\n' "), "{script}");
        assert!(script.contains(r#"echo "    oifname \"lo\" accept""#), "{script}");
    }

    #[test]
    fn the_hosts_address_is_dropped_after_the_replies_and_dns_it_must_keep() {
        let script = open();
        assert!(script.contains("host=192.168.5.2\n"));
        let replies = at(&script, "ip daddr { $hosts } tcp flags & (syn | ack) != syn accept");
        let dns = at(&script, "ip daddr { $resolvers } udp dport 53 accept");
        let drop = at(&script, "ip daddr { $hosts } drop");
        assert!(replies < drop && dns < drop, "an accept after the drop never matches");
        // The gateway is the host too, whatever the address.
        assert!(script.contains("ip -4 route show default"));
        // Lima's own resolver is the host's address: UDP only, never TCP.
        assert!(!script.contains("tcp dport 53"));
    }

    #[test]
    fn replies_to_the_hosts_connections_are_told_apart_by_the_syn_flag_not_by_conntrack() {
        // A connection the host opened (Lima's ssh) may be older than the rules.
        let script = open();
        let output = &script[at(&script, "chain output {")..at(&script, "chain forward {")];
        assert!(!output.contains("ct state"), "{output}");
    }

    #[test]
    fn a_container_gets_its_replies_and_its_new_connections_meet_the_guard() {
        // A container's connection out is new when it is forwarded, so the
        // guard sees it; only what comes back is let through by state.
        let script = open();
        let forward = &script[at(&script, "chain forward {")..];
        assert!(at(forward, "ct state established,related accept") < at(forward, "jump guard"), "{forward}");
        assert!(!forward.contains("ct state new"), "{forward}");
    }

    #[test]
    fn the_rest_of_the_hosts_network_is_closed_except_the_subnets_the_cell_is_on() {
        let script = open();
        let own = at(&script, "ip daddr { $own } accept");
        let private = at(&script, "ip daddr { 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16, 100.64.0.0/10 } drop");
        assert!(own < private);
        assert!(at(&script, "ip daddr { $hosts } drop") < own, "the host's address must not be rescued by the own-subnet rule");
    }

    #[test]
    fn the_rules_hook_output_and_forward_so_a_container_cannot_go_around() {
        let script = open();
        assert!(script.contains("type filter hook output priority 0; policy accept;"));
        assert!(script.contains("type filter hook forward priority 0; policy accept;"));
        assert_eq!(script.matches("jump guard").count(), 2);
    }

    #[test]
    fn the_rules_are_loaded_now_and_at_every_boot_before_the_network() {
        let script = open();
        assert!(script.contains("nft -f /etc/adytum/firewall.nft\n"));
        assert!(script.contains("Before=network-pre.target"));
        assert!(script.contains("systemctl enable adytum-firewall.service"));
        // No nft, no wall: the cell must not come up without one.
        assert!(script.contains("nft is missing") && script.contains("exit 1"));
        // The table is replaced whole, so a second boot leaves no duplicates.
        assert!(at(&script, "delete table inet adytum") < at(&script, "echo \"table inet adytum {\""));
    }

    #[test]
    fn the_exec_user_has_no_password_and_no_sudo_rule() {
        let script = open();
        assert!(script.contains("user=cell\n"));
        assert!(script.contains("useradd --create-home --shell /bin/bash --user-group \"$user\""));
        assert!(script.contains("passwd -l \"$user\""));
        for power in ["sudoers", "usermod", "adduser", "gpasswd", "NOPASSWD"] {
            assert!(!script.contains(power), "{power}");
        }
    }

    #[test]
    fn commands_run_as_the_exec_user_or_as_root_never_as_the_admin_user() {
        let argv = vec!["id".to_owned(), "-un".to_owned()];
        assert_eq!(as_user(EXEC_USER, argv.clone()), ["sudo", "-n", "-u", "cell", "-H", "--", "id", "-un"]);
        assert_eq!(as_root(argv), ["sudo", "-n", "--", "id", "-un"]);
        // A command that starts with a dash is not read as an option of sudo.
        assert_eq!(as_user(EXEC_USER, vec!["-u".to_owned()])[6], "-u");
    }
}
