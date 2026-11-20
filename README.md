# Adytum

Adytum makes sealed cells on one machine. A cell is a Lima VM, or a container inside a Lima VM, behind a wall: a label that keeps it apart from cells with other labels. Programs in a cell cannot reach the host, run as an unprivileged user, and get files only through a copy you start. Adytum is a Rust library and a command, `adytum`.

## Install

You need [Lima](https://lima-vm.io) 2.1.2 and Rust.

```text
cargo install --path crates/adytum-cli
adytum doctor
```

`adytum doctor` says what is missing, each problem with its fix, and whether this Mac can run VMs inside cells. Adytum keeps its ledger and its own Lima folder under `~/.adytum`, or under `ADYTUM_HOME`, so it never sees the VMs in Lima's default folder.

## The command

```text
adytum create adytum-web --sealed --wall wall_work --port 18080:80
adytum start adytum-web
adytum exec adytum-web -- uname -a
adytum cp ./site adytum-web:/home/cell/site
adytum cp adytum-web:/home/cell/out.log ./out.log
adytum cp --owner root ./unit adytum-web:/etc/systemd/system/web.service
adytum clone adytum-web adytum-web2      # the source must be stopped
adytum ls
adytum stop adytum-web && adytum rm adytum-web
```

A cell can run inside another. A Lima cell made with `--containers` runs containerd, and holds container cells:

```text
adytum create adytum-box --sealed --containers
adytum create adytum-job --kind container --parent adytum-box \
  --image docker.io/library/alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
adytum start adytum-job                  # starts adytum-box first if it is stopped
adytum exec adytum-job -- id -u          # 1000, with no capabilities
adytum ls                                # adytum-job is indented under adytum-box
adytum rm adytum-box                     # removes adytum-job first
```

A container cell keeps its parent's wall and is sealed when its parent is. Its image is pinned by digest. It runs as user 1000 with every capability dropped and no way to gain one, and a sealed one has no network. It publishes no ports and mounts nothing. Without `--parent` a container cell runs through `docker` on this machine.

`--nested-virtualization` on a Lima cell lets programs in it run VMs of their own, on a Mac with an M3 or later chip and macOS 15 or later.

`adytum exec` forwards its stdin to the command when stdin is not a terminal, and shows the command's output as it comes, so `tar -c -f - ./t | adytum exec adytum-web -- tar -x -f - -C /home/cell` unpacks in the cell without a file on the way.

Cell names are `adytum-` and lowercase letters, digits and dashes. Every command takes `--json` and then prints one JSON object; an error is `{"error": {"code", "message", "fix"}}`. The exit code is 0 when it worked, 1 when it was refused or failed, and 2 when the command line was wrong; `adytum exec` exits with the command's own code.

## The library

`Cells` is the one way to change a cell's life, so the ledger always knows what exists. It gets each cell's driver from `Drivers`: `SystemDrivers` runs Lima on this machine and containers in Lima cells. The features are `lima` and `container` (both on by default), `openshell` for a driver for OpenShell sandboxes, `doctor` for the setup checks, and `testing` for a fake driver and a fake runner.

```rust,no_run
use std::sync::Arc;

use adytum::lima::{self, lima_home_in};
use adytum::{CellSpec, Cells, ContainerOptions, ExecOpts, Kind, LimaOptions, Resources, SystemDrivers};

#[tokio::main]
async fn main() -> adytum::Result<()> {
    let home = adytum::home().expect("HOME is set");
    let cells = Cells::open(&home, Arc::new(SystemDrivers::new(lima_home_in(&home))))?;
    cells.reconcile().await?;
    let image = lima::image(lima::DEFAULT_IMAGE).expect("the default image is pinned");
    let vm = CellSpec {
        name: "adytum-demo".parse()?,
        kind: Kind::Lima(LimaOptions { containers: true, ..LimaOptions::new(image) }),
        parent: None,
        wall: "wall_demo".parse()?,
        resources: Resources::default(),
        ports: vec![],
        mounts: vec![],
        sealed: true,
        network: None,
        clone_from: None,
    };
    cells.create(&vm).await?;
    let image = "docker.io/library/alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f".parse()?;
    let job = CellSpec { name: "adytum-demo-job".parse()?, kind: Kind::Container(ContainerOptions { image }), parent: Some(vm.name.clone()), ..vm.clone() };
    cells.create(&job).await?;
    cells.start(&job.name).await?;
    let out = cells.driver_of(&job.name)?.exec(&job.name, &["id".into(), "-u".into()], ExecOpts::default()).await?;
    print!("{}", out.stdout_lossy());
    cells.remove(&vm.name).await
}
```

## Limits

- Lima cells use the `vz` VM type, so the host is a Mac. A Lima cell cannot run inside another cell yet, and there is no UTM, Windows Sandbox or Hyper-V driver; [the design](docs/design.md#kinds-to-come) says what each would take.
- A VM is a strong wall, not a perfect one: a flaw in the hypervisor reaches the host. A container is a weaker wall than a VM: a flaw in the guest's kernel gives root in the parent VM, and root there can lift the parent's firewall and reach the host's loopback.
- A wall is a label. Adytum keeps walls apart and never learns what one means, so who may use a wall is decided by whatever drives Adytum.
- Adytum acts for the person who runs it. Anyone who can run commands as you can run `adytum` as you.
- A cell has no IPv6 and reaches the internet, unless it is a sealed container, which has no network. The firewall closes the host and private networks, not the internet.
- A VM needs about 20 GiB free where `ADYTUM_HOME` lives.

[The design](docs/design.md) says what Adytum promises and why. To work on Adytum, see [CONTRIBUTING.md](CONTRIBUTING.md).
