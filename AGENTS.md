# Adytum: notes for agents

Adytum keeps sealed cells (VMs through Lima, containers through podman or docker). It depends on none of XenoNet. Eidolon (its own repository) and, later, XenoNet use it.

- Crates: `adytum-model` (ids, specs), `-policy` (pure isolation rules), `-exec` (argument lists, never shell strings), `-driver` (the driver trait), `-lima`, `-oci`, `-ledger` (a SQLite record settled after each step, compared at every start), `-doctor`, `-testkit`, `-cli` (the `adytum` command, `--json` on every command).
- Design: `docs/design.md`. Read it before changing a rule.
- Checks: see CONTRIBUTING.md. Live loop: `ADYTUM_LIVE=1 cargo test -p adytum-cli --test live`. Cells made by tests are named `adytum-test-*`; never start, stop or delete other VMs.
- A refusal gets its test first. No secret in an argument, temp file or log. Docs state the current state only.
- Exit codes: 0 done, 1 refused or failed, 2 misuse.
