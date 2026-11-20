use std::io::Write;
use std::path::{Path, PathBuf};

use adytum::{Cell, CellName, CellSpec, CellState, ContainerOptions, GuestUser, Kind, LimaOptions, OciImage, Port, Resources, Runtime, Wall};
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
        let cells = Cells::open(&ctx.home, ctx.drivers.clone())?;
        let report = cells.reconcile().await?;
        for (what, names) in [("removed the half-made or vanished cell", &report.removed), ("finished removing", &report.finished)] {
            for name in names {
                let _ = writeln!(err, "adytum: reconciled: {what} {name}.");
            }
        }
        for u in &report.unlisted {
            let host = u.host.as_ref().map_or_else(|| "this machine".to_owned(), ToString::to_string);
            let _ = writeln!(err, "adytum: could not list the {:?} cells on {host}, so they are left as recorded: {}", u.kind, u.error);
        }
        Ok(Session { cells, report })
    }

    fn reconciled(&self) -> Value {
        let unlisted: Vec<Value> = self.report.unlisted.iter().map(|u| json!({ "host": u.host, "kind": u.kind, "error": u.error })).collect();
        json!({ "removed": self.report.removed, "finished": self.report.finished, "unrecorded": self.report.unrecorded, "unlisted": unlisted })
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
        Command::Create(args) => create(ctx, err, &args).await,
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

fn spec_from(args: &CreateArgs, name: CellName, parent: Option<&Cell>) -> Result<CellSpec, Failure> {
    let lima_only = |flag: &str| usage(format!("--{flag} is for a Lima cell."), format!("Leave --{flag} out, or make a Lima cell."));
    let kind = match args.kind.as_deref().unwrap_or("lima") {
        "lima" => {
            if args.runtime.is_some() {
                return Err(usage("--runtime is for a container cell.", "Leave --runtime out, or make a container cell."));
            }
            let image_name = args.image.as_deref().unwrap_or(adytum::lima::DEFAULT_IMAGE);
            let image = adytum::lima::image(image_name)
                .ok_or_else(|| usage(format!("There is no image named {image_name:?}."), format!("Use one of: {}.", adytum::lima::image_names().join(", "))))?;
            Kind::Lima(LimaOptions { image, containers: args.containers, nested_virtualization: args.nested_virtualization })
        }
        "container" => {
            if args.containers {
                return Err(lima_only("containers"));
            }
            if args.nested_virtualization {
                return Err(lima_only("nested-virtualization"));
            }
            let text = args.image.as_deref().ok_or_else(|| usage("A container cell needs an image.", "Give --image <reference>@sha256:<digest>."))?;
            let image: OciImage =
                text.parse().map_err(|e| usage(format!("{e}."), "Give --image <reference>@sha256:<digest>, such as docker.io/library/alpine:3.22@sha256:<64 hex>."))?;
            let runtime = match args.runtime.as_deref() {
                None | Some("gvisor") => Runtime::Gvisor,
                Some("runc") => Runtime::Runc,
                Some(other) => return Err(usage(format!("There is no runtime named {other:?}."), "Use --runtime gvisor or --runtime runc.")),
            };
            Kind::Container(ContainerOptions { image, runtime })
        }
        other => return Err(usage(format!("There is no kind of cell named {other:?}."), "Use --kind lima or --kind container.")),
    };
    // A cell in a cell keeps its parent's wall, and is sealed if the parent is.
    let wall = match (&args.wall, parent) {
        (Some(wall), _) => wall.parse::<Wall>().map_err(|e| usage(format!("{e}."), "Write a wall like wall_work."))?,
        (None, Some(parent)) => parent.spec.wall.clone(),
        (None, None) => DEFAULT_WALL.parse::<Wall>().map_err(|e| usage(format!("{e}."), "Write a wall like wall_work."))?,
    };
    // A cell in a cell may use what its parent has, unless told less.
    let defaults = parent.map_or_else(Resources::default, |p| p.spec.resources);
    let resources = Resources { cpus: args.cpus.unwrap_or(defaults.cpus), memory_mib: args.memory_mib.unwrap_or(defaults.memory_mib), disk_gib: defaults.disk_gib };
    if resources.cpus == 0 || resources.memory_mib == 0 {
        return Err(usage("A cell needs at least one CPU and some memory.", "Give --cpus and --memory a number above 0."));
    }
    Ok(CellSpec {
        name,
        kind,
        parent: parent.map(|p| p.spec.name.clone()),
        wall,
        resources,
        ports: loopback_ports(&args.ports),
        mounts: Vec::new(),
        sealed: args.sealed || parent.is_some_and(|p| p.spec.sealed),
        network: None,
        clone_from: None,
    })
}

fn loopback_ports(ports: &[(u16, u16)]) -> Vec<Port> {
    let loopback = std::net::IpAddr::from([127, 0, 0, 1]);
    ports.iter().map(|&(host, guest)| Port { guest, host, bind: loopback }).collect()
}

fn kind_text(kind: &Kind) -> Value {
    serde_json::to_value(kind.name()).unwrap_or(Value::Null)
}

async fn create(ctx: &Context, err: &mut dyn Write, args: &CreateArgs) -> Result<Done, Failure> {
    let name = args.name.parse::<CellName>()?;
    let parent = args.parent.as_deref().map(str::parse::<CellName>).transpose()?;
    // Checked before the ledger is opened, so a wrong line leaves no trace.
    if parent.is_none() {
        spec_from(args, name.clone(), None)?;
    }
    let session = Session::open(ctx, err).await?;
    let parent = match &parent {
        Some(p) => Some(session.cells.get(p)?),
        None => None,
    };
    let spec = spec_from(args, name, parent.as_ref())?;
    let id = session.cells.create(&spec).await?;
    let sealed = if spec.sealed { ", sealed" } else { "" };
    let inside = spec.parent.as_ref().map(|p| format!(" in {p}")).unwrap_or_default();
    Ok(session.done(
        json!({ "name": spec.name, "id": id.to_string(), "state": CellState::Stopped, "wall": spec.wall, "sealed": spec.sealed, "kind": kind_text(&spec.kind), "parent": spec.parent }),
        format!("created {}{inside} ({}{sealed}); start it with `adytum start {}`", spec.name, spec.wall, spec.name),
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

/// Input that is not a terminal is forwarded to the command through a
/// stream, and its output is written as it comes. The exit code is the
/// command's own, or 1 when it did not exit by itself.
async fn exec(ctx: &Context, name: &str, argv: &[String], out: &mut dyn Write, err: &mut dyn Write, json: bool) -> Result<Done, Failure> {
    let name = name.parse::<CellName>()?;
    let session = Session::open(ctx, err).await?;
    let driver = session.cells.driver_of(&name)?;
    let input = ctx.input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    let output = match input {
        Some(input) => forward(&*driver, &name, argv, input, if json { None } else { Some((&mut *out, &mut *err)) }).await?,
        None => driver.exec(&name, argv, ExecOpts::default()).await?,
    };
    let exit = match (output.success(), output.status) {
        (true, _) => 0,
        (false, Some(code)) if code != 0 => code,
        _ => 1,
    };
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

/// Runs `argv` on a stream: `input` goes to its stdin, and its stdout and
/// stderr go to `shown` as they come, or into the returned output when
/// `shown` is `None`.
async fn forward(
    driver: &dyn adytum::Driver,
    name: &CellName,
    argv: &[String],
    mut input: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    shown: Option<(&mut dyn Write, &mut dyn Write)>,
) -> Result<adytum::exec::Output, Failure> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut duplex = driver.stream(name, argv, ExecOpts::default()).await?;
    let (mut out, mut err) = shown.map_or((None, None), |(o, e)| (Some(o), Some(e)));
    let mut errors = if err.is_some() { duplex.take_stderr() } else { None };
    let (mut from, mut to) = tokio::io::split(duplex);
    // A command that stops reading early closes the pipe; that is its business.
    let feed = async {
        let _ = tokio::io::copy(&mut input, &mut to).await;
        let _ = to.shutdown().await;
    };
    let mut kept = Vec::new();
    let drain = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = from.read(&mut buf).await.map_err(|source| adytum::Error::Io { what: "read the command's output", source })?;
            if n == 0 {
                return Ok::<(), adytum::Error>(());
            }
            match out.as_mut() {
                Some(out) => {
                    let _ = out.write_all(&buf[..n]);
                    let _ = out.flush();
                }
                None => kept.extend_from_slice(&buf[..n]),
            }
        }
    };
    let show_errors = async {
        let (Some(chunks), Some(err)) = (errors.as_mut(), err.as_mut()) else { return };
        while let Some(chunk) = chunks.recv().await {
            let _ = err.write_all(&chunk);
            let _ = err.flush();
        }
    };
    let ((), drained, ()) = tokio::join!(feed, drain, show_errors);
    let mut output = from.unsplit(to).finish().await?;
    drained?;
    output.stdout = kept;
    Ok(output)
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
    let driver = session.cells.driver_of(&name)?;
    if to_cell {
        let path = guest_side(dst).map_or("", |(_, p)| p);
        driver.copy_in(&name, &absolute(src)?, path, owner.as_ref()).await?;
    } else {
        let path = guest_side(src).map_or("", |(_, p)| p);
        driver.copy_out(&name, path, &absolute(dst)?).await?;
    }
    Ok(session.done(json!({ "from": src, "to": dst }), format!("copied {src} to {dst}")))
}

async fn list(ctx: &Context, err: &mut dyn Write) -> Result<Done, Failure> {
    let session = Session::open(ctx, err).await?;
    let cells = session.cells.list()?;
    let rows: Vec<Value> =
        cells.iter().map(|c| json!({ "name": c.spec.name, "state": c.state, "wall": c.spec.wall, "kind": kind_text(&c.spec.kind), "parent": c.spec.parent })).collect();
    let mut lines = Vec::new();
    tree(&cells, None, 0, &mut lines);
    for name in &session.report.unrecorded {
        lines.push(format!("{name}  (not recorded by adytum; left alone)"));
    }
    let text = if lines.is_empty() { "no cells".to_owned() } else { lines.join("\n") };
    Ok(session.done(json!({ "cells": rows }), text))
}

/// Each cell, then the cells inside it two spaces further in.
fn tree(cells: &[Cell], parent: Option<&CellName>, depth: usize, lines: &mut Vec<String>) {
    for cell in cells.iter().filter(|c| c.spec.parent.as_ref() == parent) {
        let text = |v: Value| v.as_str().map(str::to_owned).unwrap_or_default();
        let (state, kind) = (text(serde_json::to_value(cell.state).unwrap_or(Value::Null)), text(kind_text(&cell.spec.kind)));
        lines.push(format!("{}{}  {state}  {}  {kind}", "  ".repeat(depth), cell.spec.name, cell.spec.wall));
        if depth < 8 {
            tree(cells, Some(&cell.spec.name), depth + 1, lines);
        }
    }
}
