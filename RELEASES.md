# Adytum release notes

## 0.2.0 (rewrite)

- One library crate, `adytum`, with the features `lima` and `container` (both default), `openshell`, `doctor` and `testing`, and the `adytum` command in `adytum-cli`.
- `Cells` is the one API for a cell's life, and records every step in the ledger. It gets each cell's driver from `Drivers`; `SystemDrivers` is the one the command uses.
- A spec has a `kind` (`Kind::Lima`, `Kind::Container` or `Kind::OpenShell`, each with its options) and a `parent`, the cell it runs in. A Lima cell made with `containers` on holds container cells, and one made with `nested_virtualization` on can run VMs of its own.
- Container cells run through `nerdctl` in a Lima cell or `docker` on this machine, pinned by digest, under gVisor unless an open one asks for runc, as an unprivileged user with no capabilities, and with no network when sealed. A container on this machine must be sealed. A container carries its `CellId` as a label, and is removed only when it carries the recorded one.
- A Lima cell can have a desktop (`desktop`); `lima::View` and `adytum view` show it through the cell's stream, with a password for one view. `docs/recipes/firefox.md` drives Firefox in such a cell with an MCP server over `adytum exec`.
- `adytum exec` forwards stdin that is not a terminal through a stream, shows output as it comes, takes `--root`, and exits with the command's own code. `Duplex::take_stderr` gives a running child's stderr.
- The Lima driver stops a cell whose boot script did not finish, and refuses copying out a FIFO, socket or device. `Runner::run` keeps at most 64 MiB of each output stream, and a copy out stops past 8 GiB.
- `Driver::create`, `clone_cell` and `remove` take the ledger's `CellId`; `Listed` has an `id`. `Report::unlisted` names hosts whose cells could not be listed, instead of failing the reconcile.
- Creating or starting a cell starts the cells it runs in; stopping or removing one stops or removes the cells inside it first. New refusals: `parent_missing`, `parent_wall`, `parent_cannot_host`, `parent_sealed`, `child_too_big`, `nested_port`, `nested_mount`, `too_deep`, `clone_host`, `clone_source_missing`, `clone_has_cells`, `runc_sealed` and `open_container_here`.
- `Driver::stream` takes `ExecOpts`, so a stream can run as root.
- A file copied into a cell belongs to the cell's user unless the copy names another guest user as its owner (`Driver::copy_in`'s `owner`, `adytum cp --owner`). Nothing copied in is writable by group or others.
- `Refusal` and `RefusalCode` read back from JSON as well as write to it.
- The `testing` feature's `FakeDriver` records each call as a `DriverCall`, can keep each cell's files in a folder (`FakeDriver::in_folders`), takes the test's answers for `exec` and `stream`, and is a `Drivers` for every kind and host.
- `adytum doctor` says whether this Mac can run VMs inside cells.
- One `Error` type; the command's `--json` error codes and exit codes are as before.
- The ledger's schema is new, so a ledger from before is refused, with a fix that says how to start over.
