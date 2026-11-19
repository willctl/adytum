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
- A cell that holds an open eidolon is not cloned.

## Running a command

`adytum-exec` is the only place a child process starts. A command is a program and a list of arguments. The child gets an empty environment plus the variables the caller names (and `PATH` and `HOME`, named the same way), runs in its own process group, and is stopped with the whole group when the deadline passes or the caller cancels. Output streams as it comes. A duplex mode hands the caller the child's stdin and stdout as one byte stream, which is how a caller moves bytes into a cell and reaches a program's socket in the guest without a forwarded port.

## Drivers

A driver implements one trait: `create`, `start`, `exec`, `stream`, `copy_in`, `copy_out`, `stop`, `remove`, `list`, `clone`. `adytum-lima` writes a Lima template from the spec (image by location and digest, no host mounts for a sealed cell, port forwards on 127.0.0.1 only) and runs `limactl` with `--tty=false`. Its golden files pin what `limactl` is asked and what it answers, per pinned Lima version. `adytum-oci` does the same for podman and docker. The `adytum` command makes Lima cells only.

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
