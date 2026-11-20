# Contributing to Adytum

## Checks

Rust is pinned in `rust-toolchain.toml`. CI runs these, and a pull request passes them first:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy -p adytum --no-default-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
cargo deny --locked check
cargo shear
```

The live tests need Lima and the internet, and make real VMs, so they run only with `ADYTUM_LIVE=1`:

```bash
ADYTUM_LIVE=1 cargo test -p adytum-cli --test live -- --nocapture --test-threads=1
ADYTUM_LIVE=1 cargo test -p adytum-cli --test firewall -- --nocapture --test-threads=1
ADYTUM_LIVE=1 cargo test -p adytum-cli --test nest -- --nocapture --test-threads=1
```

## Rules

- Conventional Commits, with a body that says why.
- No `unsafe`. No `unwrap`, `expect`, `panic!` or `todo!` outside tests. An exception is `#[expect(lint, reason = "...")]`.
- A new dependency has a comment in `Cargo.toml` that says why. Hashing comes from `sha2`.
- Commands are argument lists, never shell strings.
- Nothing opens a listener beyond localhost.
- No real names, hostnames, accounts or home paths in the repository. Machines in docs are `desk` and `tower`.
- Docs say what is true now, in plain words, with sentence-case headings and no em dashes. Where the text needs a word for machine intelligence, it is "SI".

By sending a pull request you agree that your change is under the [MIT license](LICENSE).
