# Adytum design

Adytum keeps cells on one machine apart from the host and from each other. This page lists what it promises, why each promise holds, and what is not built. The [README](../README.md) says how to use it.

## Shape

One library crate, `adytum`, and one binary crate, `adytum-cli`. In the library:

- A `CellSpec` says what a cell is (`Kind`, with that kind's options), where it runs (`parent`: a cell, or `None` for this machine), its wall and what it may have from the host.
- `Cells` pairs the ledger with `Drivers`, which gives the driver of each kind on each host. It is the only way to create, clone, start, stop, remove and reconcile a cell. `Cells::driver_of` gives a cell's driver for commands and copies.
- A `Driver` runs one kind of cell: `lima::Lima` (feature `lima`, on by default), `container::Oci` (feature `container`, on by default) and `openshell::OpenShell` (feature `openshell`). `SystemDrivers` is the `Drivers` the command uses.
- `policy::check_create` holds every rule as a pure function over a spec and the recorded cells.
- `exec` is the only place a process starts.
- `doctor` reads the machine into a `Machine` and judges it with a pure `checks`.
- `Error` is the one error type. `code()` is stable for scripts, and `fix()` is a sentence for a person.

## Kinds and hosts

A kind is what a cell is: `Kind::Lima(LimaOptions)` is a VM, `Kind::Container(ContainerOptions)` is an OCI container, `Kind::OpenShell` is an OpenShell sandbox. `KindName` is a kind without its options, which is what picks a driver.

A host is where a driver runs its programs. `Host::Machine` is this machine. `Host::Cell` is inside a cell: `Host::runner` gives an `exec::Runner` whose commands run there as root through that cell's own driver, `exec` for a command and `stream` for a byte stream, with nothing of this machine's environment. A driver takes a `Runner` either way, so the same driver serves both hosts, and a cell's driver is built from the cells it runs in, outward to this machine.

A kind says which kinds it can run inside it (`Kind::hosts`). Today a Lima cell made with `containers` on runs container cells, and nothing else runs cells. `SystemDrivers` gives Lima on this machine, `nerdctl` in a Lima cell, and `docker` on this machine for a container with no parent.

## Names are Adytum's

Adytum touches only cells it named. A `CellName` is `adytum-` and lowercase letters, digits and dashes, and parsing any other text is the refusal `name_not_ours`. Every driver call takes a `CellName`, so a VM such as `pane` cannot reach a driver by mistake, and a driver's listing leaves out every name that does not parse. Lima runs with a `LIMA_HOME` of Adytum's own, so `limactl list` never shows the VMs in Lima's default folder at all.

## Walls

A wall is an opaque label, `wall_` and a word. Adytum compares walls and never reads one. Two cells behind different walls never share a named network, a mount source, or a clone source, and the rules refuse a spec that would. A named network becomes a Lima `user-v2` switch whose name is a hash of the wall and the network name, so the same network name behind two walls is two switches, and no argument can put a cell on another wall's switch. A container carries its wall as the label `adytum.wall`.

## Cells in cells

The rules for a cell with a parent, each with its own refusal code:

- The parent is recorded (`parent_missing`).
- The child is behind the parent's wall (`parent_wall`), so a cell never holds another wall's cells.
- The parent's kind can run the child's kind (`parent_cannot_host`).
- A sealed parent holds only sealed cells (`parent_sealed`).
- A nested cell publishes no ports (`nested_port`) and mounts nothing from this machine (`nested_mount`): its ports would open inside the parent, and its mounts would come from the parent, where no rule here can see them.
- At most three cells stand between this machine and a cell (`too_deep`).
- A clone is made where its source runs (`clone_host`).

`Cells` keeps the tree in order. Creating or starting a cell starts the cells it runs in first, outermost first. Stopping a cell stops the running cells inside it first, deepest first. Removing a cell removes the cells inside it first, deepest first, each with its own intent and settlement in the ledger. A cell whose host is not running cannot be asked, so it is not removed through a driver; it goes with the cell it runs in, because removing a VM removes its containers.

## The host is closed

Lima's user-mode network maps the guest address 192.168.5.2 (`host.lima.internal`, also the default gateway) to the host's 127.0.0.1. Left alone, a program in a cell reaches every loopback service on the host. The template carries a script that Lima runs as root at every boot, before `limactl start` returns, so before any command of Adytum's runs. It loads an nftables table and a systemd unit that loads it again before the network at later boots. The rules, in order:

1. Accept a TCP segment to the host that is not a new connection. Lima's ssh and published ports are connections the host opens, and the rule reads the SYN flag rather than conntrack, so a connection older than the rules keeps working.
2. Accept DNS over UDP to the resolvers, which Lima answers itself, and DHCP.
3. Drop everything else to 192.168.5.2 and to the default gateway.
4. Accept the subnets the cell is on, such as a named network's peers. The host's address was dropped in step 3, so this cannot reopen it.
5. Drop the private ranges 10/8, 172.16/12, 192.168/16, 169.254/16 and 100.64/10, which hold the host's LAN address.

The chain hangs on both the output and the forward hook, so a container inside a cell meets it too. The forward hook first accepts packets of connections conntrack already knows, so the replies to a container's connections reach it; every new connection a container opens is forwarded new, and meets the rules above. Without `nft` in the image the script fails and the cell does not start. The internet stays open, because a cell installs software after it is made.

## Containers

A Lima cell made with `containers` on runs rootful containerd and nerdctl from the archive Lima 2.1.2 pins by digest; the rootless one Lima starts by default stays off. The container driver speaks docker's command line, so `nerdctl` in a Lima cell and `docker` on this machine both work. Its image is pinned by digest (`OciImage`), because a tag can move.

Every container runs `sleep infinity` under an init, as user 1000:1000, with `--cap-drop ALL`, `--security-opt no-new-privileges` and a pids limit. A sealed one gets `--network none`: only a loopback interface, so it reaches neither this machine nor its parent's network. An open one is on nerdctl's bridge inside its parent, behind the parent's firewall. `ExecOpts { root: true, .. }` runs as uid 0, still with no capabilities.

Copies never use `nerdctl cp`, which nerdctl documents as meant only for trusted containers: it runs as root in the parent and follows what the container's file system says. A copy in runs `mkdir`, `tar` or `dd` inside the container as the copy's owner, fed through `exec`, so it lands only where that user may write, under the container's umask. A copy out runs `tar` or `cat` inside as user 1000, so it gets only what that user may read, and this machine's tar unpacks it, refusing entries that climb out of the destination. An image needs `sleep`, and for copies `tar`, `dd`, `cat` and `test`, which busybox and the common base images have.

A container is a weaker wall than a VM. A flaw in the guest kernel or in runc gives a program root in the parent VM, and root there can lift the parent's firewall. The VM's own wall, the hypervisor, still stands between it and the host's memory and files.

## The guest guard

Lima logs in as an admin user with passwordless sudo. Every command runs as `cell` instead: `sudo -n -u cell -H -- <command>`. The `cell` user has no password and no sudo rule, so a program in the cell cannot flush the rules. `ExecOpts { root: true, .. }` runs a command as root, for provisioning and for the drivers of the cells inside; only the caller's code sets it. A copy into a cell is written by `cell` too, so the files belong to it and land only where `cell` could write anyway. The caller can name another guest user as the copy's owner, such as `root` for provisioning; that user then writes it, so it lands only where that user may write. Either way nothing lands writable by group or others: tar unpacks a folder's files under sudo's umask of at least 022, so they keep their modes less those bits, and a single file is made under the same umask. A copy out goes through `limactl copy` as the admin user, so it gets only files that are readable by others.

## No mounts in a sealed cell

Lima mounts the host's home folder unless the template lists mounts. A sealed cell's template always says `mounts: []`, and the rules refuse a sealed spec with a mount before anything is written. The template also turns off what Lima would copy from the host: the user's name, the ssh agent, the ssh keys and the proxy variables. Ports are published on loopback only. A container driver takes no mounts at all.

## No shell

A `Command` is a program and an argument list, never a shell string. The child starts with an empty environment plus the variables the caller names, in a process group of its own, and the whole group is stopped when the deadline passes. In a cell, `limactl shell` hands each argument to the guest as one argument, and `nerdctl exec` hands them on to the container the same way.

## The ledger

`<ADYTUM_HOME>/adytum.sqlite` records each create, start, stop and remove before the driver runs and settles it after. A crash at any point leaves an open operation. Each cell's row names its parent's row, and goes when that row goes (`ON DELETE CASCADE`), so the records of the cells inside a removed cell go in the same transaction.

`Cells::reconcile` walks the hosts from this machine down. On each host it finishes or undoes the open operations of the cells there, then makes the ledger agree with what the host's drivers list, then goes on into each running cell that holds recorded cells. The cells inside a cell that is not running are left as they are recorded, open operations included, until their host runs again or goes. An open operation whose cell went with its parent is settled. A cell a host has and the ledger never recorded is reported and never touched, so Adytum deletes only what it wrote down.

Writes are `BEGIN IMMEDIATE` transactions with `synchronous = FULL`, so a record said to be written survives a power cut. `PRAGMA user_version` is a hash of the schema text, and a file with another hash is refused and left as it is; the refusal's fix says how to start over.

## Lima quirks the driver works around

- `limactl start` on an unknown name makes a default VM with the home folder mounted, so `start` and `stop` list first and refuse a missing cell.
- Under load, Lima's host agent can miss its 5-second start-up window. The driver force-stops the instance and starts it once more; a second miss is a tool failure.
- Lima keeps unix sockets in each instance's folder, and a socket path on macOS holds 104 bytes, so a long `ADYTUM_HOME` is refused and `adytum doctor` warns about it.
- Lima does not refuse two networks with one gateway, so the driver picks a gateway no other network holds.
- nerdctl ends a failed command's stderr with a line of its own, and the container driver takes it off.

## How it is checked

The rules each have a refusal test. The drivers are tested against a fake runner that checks what `limactl`, `nerdctl` and `openshell` are asked. The ledger and the tree are tested with `testing::FakeDriver`, which is also a `Drivers` whose drivers inside a cell fail while that cell is not running, by cutting each operation where a crash could cut it and reconciling after. With `ADYTUM_LIVE=1`:

- `crates/adytum-cli/tests/live.rs` runs real VMs from create to remove, including a `kill -9` during a create.
- `crates/adytum-cli/tests/firewall.rs` proves the host is closed: listeners on the host's loopback and LAN address are unreachable, `sudo` fails, the rules hold after a restart, and DNS still works.
- `crates/adytum-cli/tests/nest.rs` runs containers in a Lima cell: a sealed one has no network and reaches neither the host's loopback nor `host.lima.internal`, an open one reaches the internet and not the host, copies keep their bytes and owners, removing the VM removes its containers first, and a create killed during the image pull leaves no container after a reconcile.

## Kinds to come

What other kinds would need from the design, so the design keeps room for them. None is built.

- A Lima cell inside a Lima cell. The parent needs `nested_virtualization` (vz on an M3 or later, written as `nestedVirtualization: true`), and inside it `limactl` and QEMU, pinned by version and digest, with QEMU's `-cpu host`. `Kind::hosts` would allow it, and the host fits as it is. What is missing is in the Lima driver: it writes its template, reads `lima.yaml` and looks at its folder through this machine's file system. Those would go through the host's runner too, as `tee`, `cat` and `test` in the parent.
- UTM, for Windows and macOS guests on a Mac. `utmctl` offers list, status, start, suspend, stop, exec, file push and pull, ip-address, clone and delete through the guest agent, so a driver on `Host::Machine` fits: create is a clone of a prepared template VM, which becomes the kind's option in place of an image pin, and start, stop, remove and list map one to one. Three things do not map. The guest agent runs commands as the system account, so an unprivileged `exec` needs a way in the guest to drop to a user, or the driver refuses one. `utmctl exec --input` forwards stdin but is not a live byte stream, so `stream` is refused and a UTM cell cannot be a host until a guest program carries one. `utmctl file push` and `pull` move one file through stdin and stdout, so a folder goes as one archive and is unpacked by `exec`, which needs an archive tool in the guest. The firewall of a Lima cell does not apply; a sealed UTM cell would come from a template with no network, and an open one would need the guest's own firewall rules, provisioned through `exec`.
- Windows Sandbox (`wsb`) and Hyper-V on a Windows host, the tower. Both are drivers on `Host::Machine` with `exec::SystemRunner`, which already starts Windows programs. `wsb` starts, execs in, lists and stops sandboxes, but runs one sandbox at a time and moves files only through shared folders, which a sealed cell may not have; a copy would share a scratch folder only for its length, and that rule would need care. Hyper-V does it through PowerShell: `New-VM` from a pinned disk, `Copy-VMFile` in, and `Invoke-Command -VMName` for commands and copies out, which needs a guest credential that must not travel as an argument. Hyper-V's `ExposeVirtualizationExtensions` would be its `nested_virtualization`. Neither gives a Windows guest a firewall Adytum writes today.

## Not built

- Lima 2.3's `networkOpts.hostLoopback: false` and `--sync`, which could replace the in-guest firewall. They need an upgrade from Lima 2.1.2.
- An egress proxy on the host, so a cell has no network card and reaches only what the host lets through.
- Rootless containerd in a Lima cell, which would keep a container escape from root in the VM.
- Named networks for containers, and container images built rather than pulled.
- A microsandbox (libkrun) driver and an Apple container driver.
- OCI containers under runsc or kata.
