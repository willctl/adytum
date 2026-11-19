# Adytum design

Adytum makes, runs and removes cells on one machine. It acts only for the person on the machine it runs on and opens no network listener beyond 127.0.0.1. It depends on none of XenoNet's crates. Who may use which wall is decided outside Adytum; [XenoNet](xenonet.md) is the planned place.

[Eidolon](https://github.com/willctl/eidolon) keeps an entity's state and opens it in an Adytum cell. It links Adytum's library crates.

```
Eidolon  an entity's state, sealed, opened in a cell, moved
   |     links Adytum's library crates
Adytum   cells on one machine (VMs and containers)
```

## Vocabulary

- A **cell** is a VM or a container with one purpose, made and removed by Adytum.
- A **wall** is an opaque label every cell carries. Adytum never learns what a label means.
- A cell can be recorded as **holding** an open eidolon (`adytum hold`), which stops it from being cloned. Adytum knows only the eidolon's id.

## Crates

| Crate | Job |
|---|---|
| `adytum-model` | Typed ids (`cell_`, `img_`, `wall_`), names, the cell spec, image pins, refusals. Serde, no IO |
| `adytum-policy` | Every isolation rule as a pure function over a spec and what already exists |
| `adytum-driver` | The trait a cell driver implements and its errors; the one place text becomes a cell name |
| `adytum-exec` | A program and an argument list, never a shell string: streamed output, a deadline, a cancel, the process group stopped, only the named variables, and a duplex byte stream to a child's stdin and stdout |
| `adytum-lima` | Lima cells: create, start, run, copy, stop, remove, from a pinned image, by running `limactl` through `adytum-exec` |
| `adytum-oci` | Podman and docker cells, the same trait as Lima |
| `adytum-ledger` | A record before each create and remove, settled after, compared with what exists at every start. It keeps its own `adytum.sqlite` under `ADYTUM_HOME` |
| `adytum-doctor` | What is wrong, each problem with its fix |
| `adytum-cli` | The `adytum` binary |
| `adytum-testkit` | A fake `limactl` and `podman` that replay golden files |

## The rules (`adytum-policy`)

Each rule's first test is its refusal.

- A port is published on 127.0.0.1 only. A spec's automation port is never published at all.
- A sealed cell has no mounts from the host.
- Adytum touches only cells it named: every driver refuses a name outside `adytum-*`.
- Walls: two cells with different walls never share a network, a mount source, a clone source or a secret.
- A Lima cell cannot reach the host. Lima's user-mode network maps the guest address 192.168.5.2 (`host.lima.internal`, also the default gateway) to the host's 127.0.0.1, so a cell would otherwise reach every loopback service on the host. A firewall in the guest drops new connections to that address and to every private range (which holds the host's own LAN address), except DNS and the cell's own other subnets.
- A command in a Lima cell runs as an unprivileged user with no sudo, so a program in the cell cannot lift that firewall.
- A cell that holds an open eidolon is not cloned.

## Running a command

`adytum-exec` is the only place a child process starts. A command is a program and a list of arguments. The child gets an empty environment plus the variables the caller names (and `PATH` and `HOME`, named the same way), runs in its own process group, and is stopped with the whole group when the deadline passes or the caller cancels. Output streams as it comes. A duplex mode hands the caller the child's stdin and stdout as one byte stream, which is how a caller moves bytes into a cell and reaches a program's socket in the guest without a forwarded port.

## Drivers

A driver implements one trait: `create`, `start`, `exec`, `stream`, `copy_in`, `copy_out`, `stop`, `remove`, `list`, `clone`. `adytum-lima` writes a Lima template from the spec (image by location and digest, no host mounts for a sealed cell, port forwards on 127.0.0.1 only, a provisioning script for the guest's firewall and unprivileged user) and runs `limactl` with `--tty=false`. Its golden files pin what `limactl` is asked and what it answers, per pinned Lima version. `adytum-oci` does the same for podman and docker. The `adytum` command makes Lima cells only.

### The wall inside a Lima cell

Lima logs in to a guest as an admin user (`adytum`, passwordless sudo) and, left alone, leaves the guest open to the host. The template therefore carries a provisioning script that Lima runs as root at every boot, before `limactl start` returns and so before any command of Adytum's runs:

- It makes the account `cell`: no password, no sudo rule, no extra groups. `exec` and `stream` run `sudo -n -u cell -H -- <command>` from the admin account, so the program never has the admin account's rights. `ExecOpts { root: true, .. }` on `Driver::exec` runs a command as root for provisioning a cell (installing packages, writing system files); `Lima::exec_root` is the same call as a method. The Lima driver runs it as `sudo -n -- <command>`, the OCI drivers as `--user root` in the container (a plain exec runs as the image's own user), and OpenShell refuses it as unsupported. `stream` never runs as root. Only the caller's code sets the flag; a program in the cell cannot.
- It loads an nftables table `inet adytum` and a systemd unit that loads it again before the network at later boots. A `guard` chain hangs on both the output and the forward hook, so a container inside the cell cannot go round it. In order it: accepts a TCP segment to the host that is not a new connection (the replies to the connections the host opens in, such as Lima's ssh and published ports; this looks at the SYN flag, not at conntrack, so a connection older than the rules still works); accepts UDP port 53 to the resolvers (Lima's own, at 192.168.5.2) and DHCP; drops everything else to 192.168.5.2 and to the default gateway; accepts the subnets the cell itself is on (the peers on a named network, a container bridge), which never include the host's address, as that was dropped before; drops the private ranges 10/8, 172.16/12, 192.168/16, 169.254/16 and 100.64/10, which holds the host's LAN address. Without `nft` in the image the cell does not start.
- Outbound internet is open by default, because a cell installs software from the network after it is made (Eidolon's browser cell fetches Firefox from Mozilla's repository). `Lima::create_with_egress(spec, &Egress::Only(list))` adds a final drop after an allow-list of IPv4 addresses; `Egress::Only(vec![])` shuts the internet, DNS apart. The list is placed after the host drops, so it cannot reopen the host. It is an argument of the driver, not a field of the spec, and a clone keeps its source's choice.
- Not covered: IPv6 (Lima's user-mode network gives a guest none), and the host-side address of a named network (it is a host interface, not its loopback).
- The admin user is still how Lima reaches the guest. The program's account cannot become it: there is no sudo, and the key is the host's.
- `copy_in` and `copy_out` still go through the admin account (`limactl copy`), so a file copied in is owned by it; put it somewhere the `cell` user can read.

`ADYTUM_LIVE=1 cargo test -p adytum-cli --test firewall` proves it against a real VM: a listener on the host's 127.0.0.1 and one on 0.0.0.0 are unreachable from a cell at 192.168.5.2, `host.lima.internal`, the default gateway and the host's LAN address; `sudo -n true` fails; the exec user cannot flush the rules; DNS resolves; the rules hold after a restart; with an allow-list, a fetch off the list fails.

### Networks on Lima

A spec's network becomes a Lima `user-v2` network, a virtual switch inside Lima's host agent that needs no root and no `socket_vmnet`. The cell gets a second network card on it; two cells on one switch reach each other, and nothing on the host does. The Lima network is named from the cell's wall and the spec's network name together, so one network name behind two walls is two switches, and a cell can only join a network of its own wall. The driver makes the network (with a gateway in `10.200-250.x.1/24` that no other network holds) before the first cell that needs it, and deletes it when the last cell using it is removed. Multicast and mDNS across the switch are not promised. A cell with no network gets no `networks` entry, which is Lima's default. A clone joins the network its own spec names and drops the source's.

## The ledger

The ledger writes a record before a cell is created or removed and settles it after. At every start it compares its records with what the driver reports, finishes or removes the difference, and says so on standard error. A cell it never recorded is listed and never touched. It keeps its own SQLite file so that no other program's schema is involved.

## The alignment check

- **Which goal?** Secure by construction: a wall is a machine boundary, so a session can't act as an identity that isn't in its cell. Each tool is one command with a doctor.
- **Intent to outcome.** A task for an identity runs in a cell behind that identity's wall.
- **One-of-anything?** None. Any number of cells and walls per machine.
- **Who, under what authority?** Adytum acts only for the person on their own machine. A wall is a label, and who may use it is decided by whatever drives Adytum.
- **Push or poll?** The ledger reconciles once at start, and drivers answer per call. Nothing polls.
- **Vendor knowledge in the core?** None: Adytum knows Lima and OCI through drivers.
- **Existing protocols.** Lima and OCI runtimes for cells.
- **How we know.** Refusal tests for every rule, golden files for the drivers, kill tests for the ledger, and a live test (`ADYTUM_LIVE=1 cargo test -p adytum-cli --test live`) that proves its own teardown.

## Related

- [The guide](guide.md), for using Adytum.
- [XenoNet](xenonet.md), the planned use of a cell as a sandbox.
- [The README](../README.md) and [release notes](../RELEASES.md).
