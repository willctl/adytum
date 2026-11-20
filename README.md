# Adytum

Adytum makes sealed cells on one machine. A cell is a Lima VM behind a wall: a label that keeps it apart from cells with other labels. Programs in a cell cannot reach the host, run without sudo, and get files only through a copy you start. Adytum is a Rust library and a command, `adytum`.

## Install

You need [Lima](https://lima-vm.io) 2.1.2 and Rust.

```text
cargo install --path crates/adytum-cli
adytum doctor
```

`adytum doctor` says what is missing, each problem with its fix. Adytum keeps its ledger and its own Lima folder under `~/.adytum`, or under `ADYTUM_HOME`, so it never sees the VMs in Lima's default folder.

## The command

```text
adytum create adytum-web --sealed --wall wall_work --port 18080:80
adytum start adytum-web
adytum exec adytum-web -- uname -a
adytum cp ./site adytum-web:/home/cell/site
adytum cp adytum-web:/home/cell/out.log ./out.log
adytum cp --owner root ./unit adytum-web:/etc/systemd/system/web.service
adytum clone adytum-web adytum-web2      # the source must be stopped
adytum hold adytum-web eid_1             # a held cell is not cloned
adytum ls
adytum stop adytum-web && adytum rm adytum-web
```

Cell names are `adytum-` and lowercase letters, digits and dashes. Every command takes `--json` and then prints one JSON object; an error is `{"error": {"code", "message", "fix"}}`. The exit code is 0 when it worked, 1 when it was refused or failed, and 2 when the command line was wrong.

## The library

`Cells` is the one way to change a cell's life, so the ledger always knows what exists. The `lima` feature is on by default; `openshell` adds a driver for OpenShell sandboxes, `doctor` the setup checks, and `testing` a fake driver, whose cells can keep their files in folders, and a fake runner.

```rust,no_run
use std::sync::Arc;

use adytum::lima::{self, Lima};
use adytum::{CellSpec, Cells, DriverKind, ExecOpts, Resources};

#[tokio::main]
async fn main() -> adytum::Result<()> {
    let home = adytum::home().expect("HOME is set");
    let cells = Cells::open(&home, Arc::new(Lima::system(lima::lima_home_in(&home))?))?;
    cells.reconcile().await?;
    let spec = CellSpec {
        name: "adytum-demo".parse()?,
        driver: DriverKind::Lima,
        image: lima::image(lima::DEFAULT_IMAGE).expect("the default image is pinned"),
        wall: "wall_demo".parse()?,
        resources: Resources::default(),
        ports: vec![],
        automation_port: None,
        mounts: vec![],
        sealed: true,
        network: None,
        clone_from: None,
    };
    cells.create(&spec).await?;
    cells.start(&spec.name).await?;
    let out = cells.driver().exec(&spec.name, &["uname".into(), "-a".into()], ExecOpts::default()).await?;
    print!("{}", out.stdout_lossy());
    cells.remove(&spec.name).await
}
```

## Limits

- Cells are Lima VMs with the `vz` VM type, so the host is a Mac. There is no Windows, WSL or container driver.
- A VM is a strong wall, not a perfect one: a flaw in the hypervisor reaches the host.
- A wall is a label. Adytum keeps walls apart and never learns what one means, so who may use a wall is decided by whatever drives Adytum.
- Adytum acts for the person who runs it. Anyone who can run commands as you can run `adytum` as you.
- A cell has no IPv6 and reaches the internet. The firewall closes the host and private networks, not the internet.
- A VM needs about 20 GiB free where `ADYTUM_HOME` lives.

[The design](docs/design.md) says what Adytum promises and why. To work on Adytum, see [CONTRIBUTING.md](CONTRIBUTING.md).
