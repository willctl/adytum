use adytum_driver::DriverError;
use adytum_model::Egress;

/// The account every command in a cell runs as. It has no sudo and no
/// password, so a program in the cell cannot lift the guest's firewall.
/// Lima's own admin user (`adytum`, with passwordless sudo) is only the way
/// in: `limactl shell` logs in as it and drops to this user at once.
pub const EXEC_USER: &str = "cell";

/// The address Lima's user-mode network gives the host. Anything sent to it
/// reaches the host's 127.0.0.1.
pub const HOST_ADDRESS: &str = "192.168.5.2";

/// The argument list that runs `argv` as the exec user, from the admin
/// account `limactl shell` logs in as. `env` has already been put in front
/// of the program by the caller if there are variables.
pub fn as_exec_user(argv: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = ["sudo", "-n", "-u", EXEC_USER, "-H", "--"].iter().map(|a| (*a).to_owned()).collect();
    out.extend(argv);
    out
}

/// The argument list that runs `argv` as root, for provisioning a cell.
pub fn as_root(argv: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = ["sudo", "-n", "--"].iter().map(|a| (*a).to_owned()).collect();
    out.extend(argv);
    out
}

/// The script Lima runs as root at every boot, before `limactl start`
/// returns and so before any command of Adytum's runs. It makes the exec
/// user and loads the firewall (and keeps it loaded at later boots with a
/// systemd unit that runs before the network comes up).
///
/// Why each rule:
///
/// - Lima's user-mode network maps the guest address 192.168.5.2 (also
///   `host.lima.internal` and the default gateway) to the host's 127.0.0.1,
///   so without a rule a cell reaches every loopback service on the host. It
///   also carries the host's own address, so every private range is closed
///   too, apart from the subnets the cell is on (a named network's peers,
///   container bridges), which the cell needs. The host's address and
///   gateway are dropped before those subnets are let through.
/// - Rules look at TCP's SYN flag, not at conntrack, so the connections the
///   host opens into the guest (Lima's ssh, published ports) keep their
///   replies, even one opened before the rules load.
/// - DNS (UDP) to Lima's resolver stays open; it is answered by Lima itself.
/// - The chain is hooked to both output and forward, so a container in a
///   cell cannot go around it.
pub fn provision_script(egress: &Egress) -> String {
    let (mode, allowed) = match egress {
        Egress::Open => ("open", String::new()),
        Egress::Only(nets) => ("only", nets.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")),
    };
    SCRIPT.replace("@USER@", EXEC_USER).replace("@HOST@", HOST_ADDRESS).replace("@MODE@", mode).replace("@ALLOWED@", &allowed)
}

const SCRIPT: &str = r#"#!/bin/sh
# Written by adytum. Runs as root at every boot.
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
user=@USER@
host=@HOST@
mode=@MODE@
allowed="@ALLOWED@"

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
  if [ "$mode" = only ]; then
    [ -n "$allowed" ] && echo "    ip daddr { $allowed } accept"
    echo "    drop"
  fi
  echo "  }"
  echo "  chain output {"
  echo "    type filter hook output priority 0; policy accept;"
  echo "    oifname \"lo\" accept"
  echo "    jump guard"
  echo "  }"
  echo "  chain forward {"
  echo "    type filter hook forward priority 0; policy accept;"
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

/// A path inside a cell: absolute, no `..`, no control characters.
pub fn check_guest_path(path: &str) -> Result<(), DriverError> {
    let bad = |rule: &'static str| DriverError::BadPath { path: path.to_owned(), rule };
    if !path.starts_with('/') {
        return Err(bad("a guest path must be absolute"));
    }
    if path.chars().any(char::is_control) {
        return Err(bad("it has a control character"));
    }
    if path.contains(':') {
        return Err(bad("limactl reads a colon as the end of an instance name"));
    }
    if path.split('/').any(|part| part == "..") {
        return Err(bad("it climbs out with .."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> String {
        provision_script(&Egress::Open)
    }

    /// Where `needle` first appears, failing with the script in view.
    fn at(script: &str, needle: &str) -> usize {
        script.find(needle).unwrap_or_else(|| panic!("{needle:?} is not in the script:\n{script}"))
    }

    #[test]
    fn the_script_is_valid_shell() {
        for egress in [Egress::Open, Egress::only(&["1.1.1.1", "10.0.0.0/8"]).unwrap(), Egress::Only(vec![])] {
            let mut sh = std::process::Command::new("sh").arg("-n").stdin(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
            std::io::Write::write_all(&mut sh.stdin.take().unwrap(), provision_script(&egress).as_bytes()).unwrap();
            let out = sh.wait_with_output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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
        assert!(!open().contains("ct state"));
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
    fn open_egress_adds_no_final_drop_and_a_list_ends_in_one() {
        let open = open();
        assert!(open.contains("mode=open\nallowed=\"\"\n"), "{open}");
        let only = provision_script(&Egress::only(&["1.1.1.1", "198.51.100.0/24"]).unwrap());
        assert!(only.contains("mode=only\nallowed=\"1.1.1.1/32,198.51.100.0/24\"\n"), "{only}");
        // The allow-list is read after the host's address is dropped, so it cannot reopen it.
        assert!(at(&only, "ip daddr { $hosts } drop") < at(&only, "ip daddr { $allowed } accept"));
        assert!(only.contains("if [ \"$mode\" = only ]"));
        // An empty list shuts the internet but for DNS.
        assert!(provision_script(&Egress::Only(vec![])).contains("mode=only\nallowed=\"\"\n"));
    }

    #[test]
    fn commands_run_as_the_exec_user_or_as_root_never_as_the_admin_user() {
        let argv = vec!["id".to_owned(), "-un".to_owned()];
        assert_eq!(as_exec_user(argv.clone()), ["sudo", "-n", "-u", "cell", "-H", "--", "id", "-un"]);
        assert_eq!(as_root(argv), ["sudo", "-n", "--", "id", "-un"]);
        // A command that starts with a dash is not read as an option of sudo.
        assert_eq!(as_exec_user(vec!["-u".to_owned()])[6], "-u");
    }

    #[test]
    fn guest_paths_are_absolute_and_stay_put() {
        for ok in ["/", "/tmp/a b", "/tmp/..hidden", "/a/b.c"] {
            assert!(check_guest_path(ok).is_ok(), "{ok}");
        }
        for bad in ["tmp/a", "", "/tmp/../etc", "/..", "/a/..", "/a\nb", "/a:b", "-r"] {
            assert!(check_guest_path(bad).is_err(), "{bad:?}");
        }
    }
}
