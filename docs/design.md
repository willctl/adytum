# Adytum design

Adytum keeps cells on one machine apart from the host and from each other. This page lists what it promises, why each promise holds, and what is not built. The [README](../README.md) says how to use it.

## Shape

One library crate, `adytum`, and one binary crate, `adytum-cli`. In the library:

- `Cells` pairs a `Driver` with the ledger. It is the only way to create, clone, start, stop, remove and reconcile a cell.
- A `Driver` is one kind of cell. `lima::Lima` is the default; `openshell::OpenShell` is behind the `openshell` feature.
- `policy::check_create` holds every rule as a pure function over a spec and the recorded cells.
- `exec` is the only place a process starts.
- `doctor` reads the machine into a `Machine` and judges it with a pure `checks`.
- `Error` is the one error type. `code()` is stable for scripts, and `fix()` is a sentence for a person.

## Names are Adytum's

Adytum touches only cells it named. A `CellName` is `adytum-` and lowercase letters, digits and dashes, and parsing any other text is the refusal `name_not_ours`. Every driver call takes a `CellName`, so a VM such as `pane` cannot reach a driver by mistake. Lima runs with a `LIMA_HOME` of Adytum's own, so `limactl list` never shows the VMs in Lima's default folder at all.

## Walls

A wall is an opaque label, `wall_` and a word. Adytum compares walls and never reads one. Two cells behind different walls never share a named network, a mount source, or a clone source, and the rules refuse a spec that would. A named network becomes a Lima `user-v2` switch whose name is a hash of the wall and the network name, so the same network name behind two walls is two switches, and no argument can put a cell on another wall's switch.

## The host is closed

Lima's user-mode network maps the guest address 192.168.5.2 (`host.lima.internal`, also the default gateway) to the host's 127.0.0.1. Left alone, a program in a cell reaches every loopback service on the host. The template carries a script that Lima runs as root at every boot, before `limactl start` returns, so before any command of Adytum's runs. It loads an nftables table and a systemd unit that loads it again before the network at later boots. The rules, in order:

1. Accept a TCP segment to the host that is not a new connection. Lima's ssh and published ports are connections the host opens, and the rule reads the SYN flag rather than conntrack, so a connection older than the rules keeps working.
2. Accept DNS over UDP to the resolvers, which Lima answers itself, and DHCP.
3. Drop everything else to 192.168.5.2 and to the default gateway.
4. Accept the subnets the cell is on, such as a named network's peers. The host's address was dropped in step 3, so this cannot reopen it.
5. Drop the private ranges 10/8, 172.16/12, 192.168/16, 169.254/16 and 100.64/10, which hold the host's LAN address.

The chain hangs on both the output and the forward hook, so a container inside a cell cannot route around it. Without `nft` in the image the script fails and the cell does not start. The internet stays open, because a cell installs software after it is made.

## The guest guard

Lima logs in as an admin user with passwordless sudo. Every command runs as `cell` instead: `sudo -n -u cell -H -- <command>`. The `cell` user has no password and no sudo rule, so a program in the cell cannot flush the rules. `ExecOpts { root: true, .. }` runs a command as root, for provisioning; only the caller's code sets it. A copy into a cell is written by `cell` too, so the files belong to it and land only where `cell` could write anyway. The caller can name another guest user as the copy's owner, such as `root` for provisioning; that user then writes it, so it lands only where that user may write. Either way nothing lands writable by group or others: tar unpacks a folder's files under sudo's umask of at least 022, so they keep their modes less those bits, and a single file is made under the same umask. A copy out goes through `limactl copy` as the admin user, so it gets only files that are readable by others.

## No mounts in a sealed cell

Lima mounts the host's home folder unless the template lists mounts. A sealed cell's template always says `mounts: []`, and the rules refuse a sealed spec with a mount before anything is written. The template also turns off what Lima would copy from the host: the user's name, the ssh agent, the ssh keys and the proxy variables. Ports are published on loopback only.

## No shell

A `Command` is a program and an argument list, never a shell string. The child starts with an empty environment plus the variables the caller names, in a process group of its own, and the whole group is stopped when the deadline passes. In a cell, `limactl shell` hands each argument to the guest as one argument.

## The ledger

`<ADYTUM_HOME>/adytum.sqlite` records each create, start, stop and remove before the driver runs and settles it after. A crash at any point leaves an open operation. `Cells::reconcile` finishes or undoes every open operation, then makes the ledger agree with what the driver lists. A cell the driver has and the ledger never recorded is reported and never touched, so Adytum deletes only what it wrote down. Writes are `BEGIN IMMEDIATE` transactions with `synchronous = FULL`, so a record said to be written survives a power cut. `PRAGMA user_version` is a hash of the schema text, and a file with another hash is refused and left as it is.

## Lima quirks the driver works around

- `limactl start` on an unknown name makes a default VM with the home folder mounted, so `start` and `stop` list first and refuse a missing cell.
- Under load, Lima's host agent can miss its 5-second start-up window. The driver force-stops the instance and starts it once more; a second miss is a tool failure.
- Lima keeps unix sockets in each instance's folder, and a socket path on macOS holds 104 bytes, so a long `ADYTUM_HOME` is refused and `adytum doctor` warns about it.
- Lima does not refuse two networks with one gateway, so the driver picks a gateway no other network holds.

## How it is checked

The rules each have a refusal test. The drivers are tested against a fake runner that checks what `limactl` and `openshell` are asked. The ledger is tested by cutting each operation where a crash could cut it and reconciling after. With `ADYTUM_LIVE=1`, `crates/adytum-cli/tests/live.rs` runs real VMs from create to remove, including a `kill -9` during a create, and `crates/adytum-cli/tests/firewall.rs` proves the host is closed: listeners on the host's loopback and LAN address are unreachable, `sudo` fails, the rules hold after a restart, and DNS still works.

## Not built

- Lima 2.3's `networkOpts.hostLoopback: false` and `--sync`, which could replace the in-guest firewall. They need an upgrade from Lima 2.1.2.
- An egress proxy on the host, so a cell has no network card and reaches only what the host lets through.
- A microsandbox (libkrun) driver and an Apple container driver.
- OCI containers under runsc or kata.
