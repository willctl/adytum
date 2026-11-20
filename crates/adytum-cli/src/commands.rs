use std::io::Write;
use std::path::{Path, PathBuf};

use adytum::{CellName, CellSpec, CellState, DriverKind, GuestUser, Port, Resources, Wall};
use adytum::{Cells, ExecOpts, Report};
use serde_json::{json, Value};

use crate::args::{Command, CreateArgs, Usage};
use crate::{Context, Done, Failure};

const DEFAULT_WALL: &str = "wall_default";

struct Session {
    cells: Cells,
    report: Report,
}

impl Session {
    async fn open(ctx: &Context, err: &mut dyn Write) -> Result<Session, Failure> {
        let cells = Cells::open(&ctx.home, ctx.driver.clone())?;
        let report = cells.reconcile().await?;
        for (what, names) in [("removed the half-made or vanished cell", &report.removed), ("finished removing", &report.finished)] {
            for name in names {
                let _ = writeln!(err, "adytum: reconciled: {what} {name}.");
            }
        }
        Ok(Session { cells, report })
    }

    fn reconciled(&self) -> Value {
        json!({ "removed": self.report.removed, "finished": self.report.finished, "unrecorded": self.report.unrecorded })
    }

    fn done(&self, mut json: Value, text: String) -> Done {
        if let Value::Object(map) = &mut json {
            map.insert("reconciled".to_owned(), self.reconciled());
        }
        Done::new(json, text)
    }

    fn recorded(&self, name: &CellName) -> Result<CellSpec, Failure> {
        Ok(self.cells.get(name)?.spec)
    }
}

pub(crate) async fn execute(command: Command, ctx: &Context, out: &mut dyn Write, err: &mut dyn Write, json: bool) -> Result<Done, Failure> {
    // Names and options are checked before the ledger is opened, so a
    // refused line leaves no trace on disk.
    match command {
        Command::Help => Ok(Done::new(json!({}), String::new())),
        Command::Version => Ok(Done::new(json!({ "version": env!("CARGO_PKG_VERSION") }), format!("adytum {}", env!("CARGO_PKG_VERSION")))),
        Command::Doctor => doctor().await,
        Command::Create(args) => {
            let spec = spec_from(&args, args.name.parse::<CellName>()?)?;
            create(ctx, err, spec).await
        }
        Command::Start(name) => {
            let name = name.parse::<CellName>()?;
            let session = Session::open(ctx, err).await?;
            session.cells.start(&name).await?;
            Ok(session.done(json!({ "name": name, "state": CellState::Running }), format!("started {name}")))
        }
        Command::Stop(name) => {
            let name = name.parse::<CellName>()?;
            let session = Session::open(ctx, err).await?;
            session.cells.stop(&name).await?;
            Ok(session.done(json!({ "name": name, "state": CellState::Stopped }), format!("stopped {name}")))
        }
        Command::Rm(name) => {
            let name = name.parse::<CellName>()?;
            let session = Session::open(ctx, err).await?;
            session.cells.remove(&name).await?;
            Ok(session.done(json!({ "name": name, "removed": true }), format!("removed {name}")))
        }
        Command::Clone { from, to, wall, ports } => {
            let (from, to) = (from.parse::<CellName>()?, to.parse::<CellName>()?);
            let wall = wall.map(|w| w.parse::<Wall>().map_err(|e| usage(format!("{e}."), "Write a wall like wall_work."))).transpose()?;
            clone(ctx, err, &from, to, wall, &ports).await
        }
        Command::Exec { name, argv } => exec(ctx, &name, &argv, out, err, json).await,
        Command::Cp { src, dst, owner } => copy(ctx, err, &src, &dst, owner.as_deref()).await,
        Command::Ls => list(ctx, err).await,
    }
}

async fn doctor() -> Result<Done, Failure> {
    let checks = adytum::doctor::checks(&adytum::doctor::gather().await);
    let failed = adytum::doctor::has_failure(&checks);
    Ok(Done { json: json!({ "ok": !failed, "checks": checks }), text: adytum::doctor::render_human(&checks).trim_end().to_owned(), exit: i32::from(failed) })
}

fn usage(message: impl Into<String>, fix: impl Into<String>) -> Failure {
    Failure::from(Usage { message: message.into(), fix: fix.into() })
}

fn spec_from(args: &CreateArgs, name: CellName) -> Result<CellSpec, Failure> {
    let image_name = args.image.as_deref().unwrap_or(adytum::lima::DEFAULT_IMAGE);
    let image = adytum::lima::image(image_name)
        .ok_or_else(|| usage(format!("There is no image named {image_name:?}."), format!("Use one of: {}.", adytum::lima::image_names().join(", "))))?;
    let wall: Wall = args.wall.as_deref().unwrap_or(DEFAULT_WALL).parse().map_err(|e| usage(format!("{e}."), "Write a wall like wall_work."))?;
    let defaults = Resources::default();
    let resources = Resources { cpus: args.cpus.unwrap_or(defaults.cpus), memory_mib: args.memory_mib.unwrap_or(defaults.memory_mib), disk_gib: defaults.disk_gib };
    if resources.cpus == 0 || resources.memory_mib == 0 {
        return Err(usage("A cell needs at least one CPU and some memory.", "Give --cpus and --memory a number above 0."));
    }
    Ok(CellSpec {
        name,
        driver: DriverKind::Lima,
        image,
        wall,
        resources,
        ports: loopback_ports(&args.ports),
        mounts: Vec::new(),
        sealed: args.sealed,
        network: None,
        clone_from: None,
    })
}

fn loopback_ports(ports: &[(u16, u16)]) -> Vec<Port> {
    let loopback = std::net::IpAddr::from([127, 0, 0, 1]);
    ports.iter().map(|&(host, guest)| Port { guest, host, bind: loopback }).collect()
}

async fn create(ctx: &Context, err: &mut dyn Write, spec: CellSpec) -> Result<Done, Failure> {
    let session = Session::open(ctx, err).await?;
    let id = session.cells.create(&spec).await?;
    let sealed = if spec.sealed { ", sealed" } else { "" };
    Ok(session.done(
        json!({ "name": spec.name, "id": id.to_string(), "state": CellState::Stopped, "wall": spec.wall, "sealed": spec.sealed }),
        format!("created {} ({}{sealed}); start it with `adytum start {}`", spec.name, spec.wall, spec.name),
    ))
}

/// A published host port is the source's alone, so the copy gets only the
/// ports asked for.
async fn clone(ctx: &Context, err: &mut dyn Write, from: &CellName, to: CellName, wall: Option<Wall>, ports: &[(u16, u16)]) -> Result<Done, Failure> {
    let session = Session::open(ctx, err).await?;
    let source = session.recorded(from)?;
    let spec = CellSpec { name: to, wall: wall.unwrap_or_else(|| source.wall.clone()), ports: loopback_ports(ports), clone_from: Some(from.clone()), ..source };
    session.cells.create(&spec).await?;
    Ok(session.done(
        json!({ "name": spec.name, "state": CellState::Stopped, "wall": spec.wall, "sealed": spec.sealed, "cloned_from": from }),
        format!("cloned {from} to {} ({}); start it with `adytum start {}`", spec.name, spec.wall, spec.name),
    ))
}

async fn exec(ctx: &Context, name: &str, argv: &[String], out: &mut dyn Write, err: &mut dyn Write, json: bool) -> Result<Done, Failure> {
    let name = name.parse::<CellName>()?;
    let session = Session::open(ctx, err).await?;
    session.recorded(&name)?;
    let output = session.cells.driver().exec(&name, argv, ExecOpts::default()).await?;
    let exit = i32::from(!output.success());
    if json {
        let status = output.status;
        let mut done = session.done(json!({ "name": name, "status": status, "stdout": output.stdout_lossy(), "stderr": output.stderr_lossy() }), String::new());
        done.exit = exit;
        return Ok(done);
    }
    let _ = out.write_all(&output.stdout);
    let _ = err.write_all(&output.stderr);
    if exit != 0 {
        let how = output.status.map_or_else(|| "was stopped".to_owned(), |s| format!("exited with status {s}"));
        let _ = writeln!(err, "adytum: the command {how}.");
    }
    Ok(Done { json: json!({}), text: String::new(), exit })
}

/// `<name>:<path>` is a path inside a cell; anything else is on the host.
fn guest_side(side: &str) -> Option<(&str, &str)> {
    let (name, path) = side.split_once(':')?;
    (!name.is_empty() && !name.contains('/')).then_some((name, path))
}

fn absolute(path: &str) -> Result<PathBuf, Failure> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir().map_err(|source| Failure::from(adytum::Error::Io { what: "read the current folder", source }))?;
    Ok(cwd.join(path))
}

async fn copy(ctx: &Context, err: &mut dyn Write, src: &str, dst: &str, owner: Option<&str>) -> Result<Done, Failure> {
    let (name, to_cell) = match (guest_side(src), guest_side(dst)) {
        (None, Some((name, _))) => (name.parse::<CellName>()?, true),
        (Some((name, _)), None) => (name.parse::<CellName>()?, false),
        (Some(_), Some(_)) => return Err(usage("Both sides are paths inside cells.", "Copy through the host in two steps.")),
        (None, None) => return Err(usage("Neither side is a path inside a cell.", "Write one side as <name>:<absolute path>; start a host path with ./ if it holds a colon.")),
    };
    if owner.is_some() && !to_cell {
        return Err(usage("--owner is for a copy into a cell.", "Leave --owner out when copying out of a cell."));
    }
    let owner = owner.map(str::parse::<GuestUser>).transpose()?;
    let session = Session::open(ctx, err).await?;
    session.recorded(&name)?;
    if to_cell {
        let path = guest_side(dst).map_or("", |(_, p)| p);
        session.cells.driver().copy_in(&name, &absolute(src)?, path, owner.as_ref()).await?;
    } else {
        let path = guest_side(src).map_or("", |(_, p)| p);
        session.cells.driver().copy_out(&name, path, &absolute(dst)?).await?;
    }
    Ok(session.done(json!({ "from": src, "to": dst }), format!("copied {src} to {dst}")))
}

async fn list(ctx: &Context, err: &mut dyn Write) -> Result<Done, Failure> {
    let session = Session::open(ctx, err).await?;
    let mut rows = Vec::new();
    let mut lines = Vec::new();
    for cell in session.cells.list()? {
        let name = &cell.spec.name;
        rows.push(json!({ "name": name, "state": cell.state, "wall": cell.spec.wall }));
        let state = serde_json::to_value(cell.state).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        lines.push(format!("{name}  {state}  {}", cell.spec.wall));
    }
    for name in &session.report.unrecorded {
        lines.push(format!("{name}  (not recorded by adytum; left alone)"));
    }
    let text = if lines.is_empty() { "no cells".to_owned() } else { lines.join("\n") };
    Ok(session.done(json!({ "cells": rows }), text))
}
