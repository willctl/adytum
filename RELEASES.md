# Adytum release notes

## 0.2.0 (rewrite)

- One library crate, `adytum`, with the features `lima` (default), `openshell`, `doctor` and `testing`, and the `adytum` command in `adytum-cli`.
- `Cells` is the one API for a cell's life, and records every step in the ledger.
- A file copied into a cell belongs to the cell's user, `cell`, unless the copy names another guest user as its owner (`Driver::copy_in`'s `owner`, `adytum cp --owner`). Nothing copied in is writable by group or others.
- `Refusal` and `RefusalCode` read back from JSON as well as write to it.
- The `testing` feature's `FakeDriver` records each call as a `DriverCall`, can keep each cell's files in a folder (`FakeDriver::in_folders`), and takes the test's answers for `exec` and `stream`.
- One `Error` type; the command's `--json` error codes and exit codes are as before.
- Gone: the container driver, egress allow-lists and secrets in a spec. A ledger that holds a spec with `secrets` no longer reads.
