//! A cell's place among the cells recorded: what it runs in, what runs in it.

use crate::{Cell, CellName};

/// How many cells may stand between this machine and a cell: a container in
/// a VM in a VM is as deep as it goes.
pub(crate) const MAX_DEPTH: usize = 3;

pub(crate) fn find<'a>(cells: &'a [Cell], name: &CellName) -> Option<&'a Cell> {
    cells.iter().find(|c| &c.spec.name == name)
}

/// The cells `name` runs in, its parent first. A parent the cells lack
/// ends the chain, and so does a chain longer than any rule allows.
pub(crate) fn ancestors<'a>(cells: &'a [Cell], name: &CellName) -> Vec<&'a Cell> {
    let mut out: Vec<&Cell> = Vec::new();
    let mut next = find(cells, name).and_then(|c| c.spec.parent.as_ref());
    while let Some(parent) = next {
        let Some(cell) = find(cells, parent) else { break };
        if out.len() > MAX_DEPTH || out.iter().any(|c| c.spec.name == cell.spec.name) {
            break;
        }
        out.push(cell);
        next = cell.spec.parent.as_ref();
    }
    out
}

/// Every cell that runs in `name`, at any depth, the deepest first.
pub(crate) fn descendants<'a>(cells: &'a [Cell], name: &CellName) -> Vec<&'a Cell> {
    let mut levels: Vec<Vec<&Cell>> = Vec::new();
    let mut parents = vec![name];
    for _ in 0..=MAX_DEPTH {
        let level: Vec<&Cell> = cells.iter().filter(|c| c.spec.parent.as_ref().is_some_and(|p| parents.contains(&p))).collect();
        if level.is_empty() {
            break;
        }
        parents = level.iter().map(|c| &c.spec.name).collect();
        levels.push(level);
    }
    levels.into_iter().rev().flatten().collect()
}
