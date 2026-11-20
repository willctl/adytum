use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

use crate::{Cell, CellName, CellSpec, Refusal, RefusalCode};

/// The rules a spec must keep on its own, whatever else exists: the ones
/// that guard the host. A driver checks these again before it writes
/// anything.
pub fn check_spec(spec: &CellSpec) -> Result<(), Refusal> {
    ports_on_localhost(spec)?;
    sealed_has_no_mounts(spec)
}

/// Every rule against the cells that exist; the first refusal wins.
pub fn check_create(spec: &CellSpec, cells: &[Cell]) -> Result<(), Refusal> {
    check_spec(spec)?;
    name_free(spec, cells)?;
    walls_share_no_network(spec, cells)?;
    walls_share_no_mount(spec, cells)?;
    walls_share_no_clone_source(spec, cells)
}

fn find<'a>(cells: &'a [Cell], name: &CellName) -> Option<&'a Cell> {
    cells.iter().find(|c| &c.spec.name == name)
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
