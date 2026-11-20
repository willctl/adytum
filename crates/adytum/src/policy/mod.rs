use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

use crate::model::tree::{ancestors, find, MAX_DEPTH};
use crate::{Cell, CellSpec, Refusal, RefusalCode};

/// The rules a spec must keep on its own, whatever else exists: the ones
/// that guard the host. A driver checks these again before it writes
/// anything.
pub fn check_spec(spec: &CellSpec) -> Result<(), Refusal> {
    ports_on_localhost(spec)?;
    nested_publishes_nothing(spec)?;
    sealed_has_no_mounts(spec)?;
    nested_mounts_nothing(spec)?;
    sealed_containers_run_under_gvisor(spec)?;
    containers_here_are_sealed(spec)
}

/// A container on this machine with a network reaches the machine: docker's
/// bridge, and Docker Desktop's host.docker.internal, lead to its loopback
/// and its LAN, and no firewall of Adytum's stands in between.
fn containers_here_are_sealed(spec: &CellSpec) -> Result<(), Refusal> {
    if spec.parent.is_none() && spec.kind.name() == crate::KindName::Container && !spec.sealed {
        return Err(Refusal::new(
            RefusalCode::OpenContainerHere,
            format!("{} would be a container on this machine with a network, which reaches this machine.", spec.name),
            "Make it sealed, or run it in a Lima cell made with containers on, behind that cell's firewall.",
        ));
    }
    Ok(())
}

/// Under runc a flaw in the parent's kernel gives root in the parent, and
/// root there can lift the firewall; a sealed cell promises more than that.
fn sealed_containers_run_under_gvisor(spec: &CellSpec) -> Result<(), Refusal> {
    match &spec.kind {
        crate::Kind::Container(options) if spec.sealed && options.runtime == crate::Runtime::Runc => Err(Refusal::new(
            RefusalCode::RuncSealed,
            format!("{} is sealed, and a sealed container runs under gVisor, not runc.", spec.name),
            "Leave the runtime at gVisor, or make the container not sealed.",
        )),
        _ => Ok(()),
    }
}

/// Every rule against the cells that exist; the first refusal wins.
pub fn check_create(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    check_spec(spec)?;
    name_free(spec, cells)?;
    parent_can_hold_it(spec, cells)?;
    walls_share_no_network(spec, cells)?;
    walls_share_no_mount(spec, cells)?;
    walls_share_no_clone_source(spec, cells)?;
    clone_of_a_recorded_cell_with_none_inside(spec, cells)?;
    clone_on_the_same_host(spec, cells)
}

/// A source the ledger never recorded has no wall to check, and a cell
/// with cells inside would hand its copy their files unrecorded.
fn clone_of_a_recorded_cell_with_none_inside(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    let Some(source) = &spec.clone_from else { return Ok(()) };
    if find(cells, source).is_none() {
        return Err(Refusal::new(RefusalCode::CloneSourceMissing, format!("Adytum has no record of {source}, so it cannot be cloned."), "Clone a cell `adytum ls` shows."));
    }
    if cells.iter().any(|c| c.spec.parent.as_ref() == Some(source)) {
        return Err(Refusal::new(
            RefusalCode::CloneHasCells,
            format!("{source} has cells inside it, and a clone would carry them along unrecorded."),
            "Remove the cells inside it first, or clone another cell.",
        ));
    }
    Ok(())
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

fn ports_on_localhost(spec: &CellSpec) -> Result<(), Refusal> {
    match spec.ports.iter().find(|p| !is_loopback(p.bind)) {
        None => Ok(()),
        Some(p) => Err(Refusal::new(
            RefusalCode::PortNotLocalhost,
            format!("Port {} would be published on {}, which other machines can reach.", p.guest, p.bind),
            "Publish it on 127.0.0.1.",
        )),
    }
}

/// A nested cell's ports would be published inside its parent, which no
/// rule here can see.
fn nested_publishes_nothing(spec: &CellSpec) -> Result<(), Refusal> {
    match (&spec.parent, spec.ports.first()) {
        (Some(parent), Some(p)) => Err(Refusal::new(
            RefusalCode::NestedPort,
            format!("{} runs in {parent}, so it cannot publish port {}.", spec.name, p.guest),
            "Remove the ports; reach a nested cell through exec or a stream.",
        )),
        _ => Ok(()),
    }
}

fn nested_mounts_nothing(spec: &CellSpec) -> Result<(), Refusal> {
    match (&spec.parent, spec.mounts.first()) {
        (Some(parent), Some(m)) => Err(Refusal::new(
            RefusalCode::NestedMount,
            format!("{} runs in {parent}, so it cannot mount {} from this machine.", spec.name, m.source.display()),
            "Remove the mounts, and copy the files in instead.",
        )),
        _ => Ok(()),
    }
}

fn sealed_has_no_mounts(spec: &CellSpec) -> Result<(), Refusal> {
    if spec.sealed && !spec.mounts.is_empty() {
        return Err(Refusal::new(RefusalCode::SealedCellMount, "A sealed cell cannot have mounts from the host.", "Remove the mounts, or make the cell not sealed."));
    }
    Ok(())
}

fn name_free(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    if find(cells, &spec.name).is_some() {
        return Err(Refusal::new(RefusalCode::NameTaken, format!("A cell named {} already exists.", spec.name), "Pick another name, or remove that cell first."));
    }
    Ok(())
}

/// The parent exists, keeps the same wall, can run this kind of cell, is not
/// sealed unless the child is, and is not too deep already.
fn parent_can_hold_it(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    let Some(name) = &spec.parent else { return Ok(()) };
    let Some(parent) = find(cells, name) else {
        return Err(Refusal::new(
            RefusalCode::ParentMissing,
            format!("There is no cell named {name} for {} to run in.", spec.name),
            "Create the parent cell first, or leave the parent out.",
        ));
    };
    if parent.spec.wall != spec.wall {
        return Err(Refusal::new(
            RefusalCode::ParentWall,
            format!("{} would be behind {}, but its parent {name} is behind {}.", spec.name, spec.wall, parent.spec.wall),
            "Give the cell its parent's wall.",
        ));
    }
    if !parent.spec.kind.hosts(spec.kind.name()) {
        return Err(Refusal::new(
            RefusalCode::ParentCannotHost,
            format!("{name} cannot run a {} cell.", kind_word(spec)),
            "Pick a parent that can, such as a Lima cell made with containers on for a container.",
        ));
    }
    if parent.spec.sealed && !spec.sealed {
        return Err(Refusal::new(RefusalCode::ParentSealed, format!("{name} is sealed, so {} must be sealed too.", spec.name), "Make the cell sealed."));
    }
    let (have, want) = (parent.spec.resources, spec.resources);
    // A container's disk is its parent's, so only a VM's disk must fit.
    let disk_too_big = spec.kind.name() == crate::KindName::Lima && want.disk_gib > have.disk_gib;
    if want.cpus > have.cpus || want.memory_mib > have.memory_mib || disk_too_big {
        return Err(Refusal::new(
            RefusalCode::ChildTooBig,
            format!(
                "{} asks for {} CPUs, {} MiB and {} GiB, more than its parent {name} has ({} CPUs, {} MiB, {} GiB).",
                spec.name, want.cpus, want.memory_mib, want.disk_gib, have.cpus, have.memory_mib, have.disk_gib
            ),
            "Give the cell no more than its parent has, or give the parent more.",
        ));
    }
    let depth = ancestors(cells, name).len() + 1;
    if depth > MAX_DEPTH {
        return Err(Refusal::new(
            RefusalCode::TooDeep,
            format!("{} would run {depth} cells deep; at most {MAX_DEPTH} cells may stand between this machine and a cell.", spec.name),
            "Run it in a parent nearer this machine.",
        ));
    }
    Ok(())
}

fn kind_word(spec: &CellSpec) -> &'static str {
    match spec.kind.name() {
        crate::KindName::Lima => "Lima",
        crate::KindName::Container => "container",
        crate::KindName::OpenShell => "OpenShell",
    }
}

fn clone_on_the_same_host(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    match spec.clone_from.as_ref().and_then(|n| find(cells, n)) {
        Some(source) if source.spec.parent != spec.parent => Err(Refusal::new(
            RefusalCode::CloneHost,
            format!("{} runs somewhere else than {} would, and a clone is made where its source is.", source.spec.name, spec.name),
            "Give the clone its source's parent.",
        )),
        _ => Ok(()),
    }
}

fn others<'a>(spec: &'a CellSpec, cells: &'a [Cell]) -> impl Iterator<Item = &'a Cell> {
    cells.iter().filter(move |c| c.spec.wall != spec.wall)
}

fn walls_share_no_network(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    let Some(net) = &spec.network else { return Ok(()) };
    match others(spec, cells).find(|c| c.spec.network.as_ref() == Some(net)) {
        None => Ok(()),
        Some(c) => Err(Refusal::new(
            RefusalCode::WallNetwork,
            format!("Network {net} is already used by {}, which is behind another wall.", c.spec.name),
            "Use a network of its own for this wall.",
        )),
    }
}

/// Resolves `.` and `..` by looking at the path alone, never the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(part);
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    a.starts_with(&b) || b.starts_with(&a)
}

fn walls_share_no_mount(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    for m in &spec.mounts {
        for c in others(spec, cells) {
            if let Some(theirs) = c.spec.mounts.iter().map(|theirs| &theirs.source).find(|s| overlaps(&m.source, s)) {
                return Err(Refusal::new(
                    RefusalCode::WallMount,
                    format!("{} overlaps {}, which {} behind another wall already mounts.", m.source.display(), theirs.display(), c.spec.name),
                    "Mount a folder that no other wall uses.",
                ));
            }
        }
    }
    Ok(())
}

fn walls_share_no_clone_source(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    let Some(src) = &spec.clone_from else { return Ok(()) };
    match others(spec, cells).find(|c| &c.spec.name == src) {
        None => Ok(()),
        Some(c) => Err(Refusal::new(
            RefusalCode::WallClone,
            format!("{} is behind another wall and cannot be cloned.", c.spec.name),
            "Clone a cell behind the same wall, or start from the image.",
        )),
    }
}

#[cfg(test)]
mod tests;
