//! The structure of a table: ordered rows and columns with tombstones, and
//! merge regions named by their corner rows and columns.
//!
//! Grid operations are not invariant-confluent — two merges that are each
//! valid can overlap once both are applied — so they travel the sequenced
//! lane: the server validates each one against the state left by everything
//! committed before it ([`Grid::apply`] rejects and leaves the grid untouched).
//! Deletion only tombstones a row or column, so a concurrent edit to one of
//! its cells still has somewhere to land and still commutes.

use crate::id::Id;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Line {
    id: Id,
    alive: bool,
}

/// A merged region, inclusive of its corner rows and columns. Rows inserted
/// between `top` and `bottom` later belong to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merge {
    pub top: Id,
    pub left: Id,
    pub bottom: Id,
    pub right: Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GridOp {
    /// `after: None` inserts at the top.
    InsertRow {
        id: Id,
        after: Option<Id>,
    },
    DeleteRow {
        id: Id,
    },
    /// `after: None` inserts at the left.
    InsertColumn {
        id: Id,
        after: Option<Id>,
    },
    DeleteColumn {
        id: Id,
    },
    Merge(Merge),
    Unmerge {
        top: Id,
        left: Id,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridError {
    UnknownRow(Id),
    UnknownColumn(Id),
    DuplicateRow(Id),
    DuplicateColumn(Id),
    DeletedRow(Id),
    DeletedColumn(Id),
    LastRow,
    LastColumn,
    InvertedRange,
    SingleCell,
    Overlap,
    NoSuchMerge,
}

impl fmt::Display for GridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GridError::UnknownRow(id) => write!(f, "row {id} does not exist"),
            GridError::UnknownColumn(id) => write!(f, "column {id} does not exist"),
            GridError::DuplicateRow(id) => write!(f, "row {id} already exists"),
            GridError::DuplicateColumn(id) => write!(f, "column {id} already exists"),
            GridError::DeletedRow(id) => write!(f, "row {id} was deleted"),
            GridError::DeletedColumn(id) => write!(f, "column {id} was deleted"),
            GridError::LastRow => write!(f, "a table keeps at least one row"),
            GridError::LastColumn => write!(f, "a table keeps at least one column"),
            GridError::InvertedRange => write!(f, "merge corners are out of order"),
            GridError::SingleCell => write!(f, "a merge must cover at least two cells"),
            GridError::Overlap => write!(f, "merged regions overlap"),
            GridError::NoSuchMerge => write!(f, "no merged region starts at that cell"),
        }
    }
}

impl std::error::Error for GridError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grid {
    rows: Vec<Line>,
    columns: Vec<Line>,
    merges: Vec<Merge>,
}

#[derive(Clone, Copy)]
enum Axis {
    Rows,
    Columns,
}

impl Grid {
    pub fn new(rows: impl IntoIterator<Item = Id>, columns: impl IntoIterator<Item = Id>) -> Self {
        Self::with_merges_unchecked(rows, columns, Vec::new())
    }

    /// Builds a grid without checking it — for loading a file whose merges
    /// may already be inconsistent, or for showing what a naive merge of two
    /// replicas would produce. Call [`Grid::check_invariants`] afterwards.
    pub fn with_merges_unchecked(
        rows: impl IntoIterator<Item = Id>,
        columns: impl IntoIterator<Item = Id>,
        merges: Vec<Merge>,
    ) -> Self {
        let line = |id| Line { id, alive: true };
        Self {
            rows: rows.into_iter().map(line).collect(),
            columns: columns.into_iter().map(line).collect(),
            merges,
        }
    }

    fn lines(&self, axis: Axis) -> &Vec<Line> {
        match axis {
            Axis::Rows => &self.rows,
            Axis::Columns => &self.columns,
        }
    }

    fn lines_mut(&mut self, axis: Axis) -> &mut Vec<Line> {
        match axis {
            Axis::Rows => &mut self.rows,
            Axis::Columns => &mut self.columns,
        }
    }

    /// Alive rows, top to bottom.
    pub fn rows(&self) -> Vec<Id> {
        alive(&self.rows)
    }

    /// Alive columns, left to right.
    pub fn columns(&self) -> Vec<Id> {
        alive(&self.columns)
    }

    /// Every row ever inserted, deleted ones included, in order.
    pub fn all_rows(&self) -> Vec<Id> {
        self.rows.iter().map(|line| line.id).collect()
    }

    /// Includes tombstoned rows, without allocating an identifier list.
    pub fn stored_row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn all_columns(&self) -> Vec<Id> {
        self.columns.iter().map(|line| line.id).collect()
    }

    /// Includes tombstoned columns, without allocating an identifier list.
    pub fn stored_column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn contains_row(&self, id: Id) -> bool {
        self.rows.iter().any(|line| line.id == id)
    }

    pub fn contains_column(&self, id: Id) -> bool {
        self.columns.iter().any(|line| line.id == id)
    }

    /// Index among the alive rows.
    pub fn row_index(&self, id: Id) -> Option<usize> {
        alive_index(&self.rows, id)
    }

    pub fn column_index(&self, id: Id) -> Option<usize> {
        alive_index(&self.columns, id)
    }

    pub fn merges(&self) -> &[Merge] {
        &self.merges
    }

    /// The merged region covering the alive cell (`row`, `column`); `None`
    /// for a cell no merge covers, or a deleted or unknown line.
    ///
    /// Lines inserted between a merge's corners belong to it, so a row
    /// inserted strictly inside a merge joins it and one inserted above its
    /// top or below its bottom does not — Word's and Excel's rule — and a
    /// deleted corner moves to the next alive line inward (Lean:
    /// `Grid.insertRow_covers`, `Grid.deleteRow_covers`).
    pub fn merge_at(&self, row: Id, column: Id) -> Option<&Merge> {
        self.row_index(row)?;
        self.column_index(column)?;
        let inside = |lines: &[Line], from: Id, to: Id, id: Id| match (
            position(lines, from),
            position(lines, to),
            position(lines, id),
        ) {
            (Some(a), Some(b), Some(i)) => a <= i && i <= b,
            _ => false,
        };
        self.merges.iter().find(|m| {
            inside(&self.rows, m.top, m.bottom, row)
                && inside(&self.columns, m.left, m.right, column)
        })
    }

    /// Whether `op` would apply; exactly what [`Grid::apply`] would return.
    pub fn validate(&self, op: &GridOp) -> Result<(), GridError> {
        self.clone().apply(op)
    }

    /// Applies `op` if it keeps the grid valid. On error nothing changes.
    pub fn apply(&mut self, op: &GridOp) -> Result<(), GridError> {
        match *op {
            GridOp::InsertRow { id, after } => self.insert(Axis::Rows, id, after),
            GridOp::DeleteRow { id } => self.delete(Axis::Rows, id),
            GridOp::InsertColumn { id, after } => self.insert(Axis::Columns, id, after),
            GridOp::DeleteColumn { id } => self.delete(Axis::Columns, id),
            GridOp::Merge(merge) => {
                self.check_merge(&merge)?;
                if self.merges.iter().any(|other| self.overlaps(&merge, other)) {
                    return Err(GridError::Overlap);
                }
                self.merges.push(merge);
                Ok(())
            }
            GridOp::Unmerge { top, left } => {
                let index = self
                    .merges
                    .iter()
                    .position(|merge| merge.top == top && merge.left == left)
                    .ok_or(GridError::NoSuchMerge)?;
                self.merges.remove(index);
                Ok(())
            }
        }
    }

    fn insert(&mut self, axis: Axis, id: Id, after: Option<Id>) -> Result<(), GridError> {
        let lines = self.lines(axis);
        if lines.iter().any(|line| line.id == id) {
            return Err(duplicate(axis, id));
        }
        let at = match after {
            None => 0,
            Some(after) => position(lines, after).ok_or_else(|| unknown(axis, after))? + 1,
        };
        self.lines_mut(axis).insert(at, Line { id, alive: true });
        Ok(())
    }

    fn delete(&mut self, axis: Axis, id: Id) -> Result<(), GridError> {
        let lines = self.lines(axis);
        let index = position(lines, id).ok_or_else(|| unknown(axis, id))?;
        if !lines[index].alive {
            return Err(deleted(axis, id));
        }
        if lines.iter().filter(|line| line.alive).count() == 1 {
            return Err(match axis {
                Axis::Rows => GridError::LastRow,
                Axis::Columns => GridError::LastColumn,
            });
        }
        self.lines_mut(axis)[index].alive = false;
        self.normalize_merges();
        Ok(())
    }

    /// Moves merge corners off deleted rows/columns and drops merges that no
    /// longer cover two cells.
    fn normalize_merges(&mut self) {
        let merges = std::mem::take(&mut self.merges);
        self.merges = merges
            .into_iter()
            .filter_map(|merge| {
                let (top, bottom) = alive_span(&self.rows, merge.top, merge.bottom)?;
                let (left, right) = alive_span(&self.columns, merge.left, merge.right)?;
                let normalized = Merge {
                    top,
                    left,
                    bottom,
                    right,
                };
                (self.cell_count(&normalized) >= 2).then_some(normalized)
            })
            .collect();
    }

    fn check_merge(&self, merge: &Merge) -> Result<(), GridError> {
        let top = self.alive_position(Axis::Rows, merge.top)?;
        let bottom = self.alive_position(Axis::Rows, merge.bottom)?;
        let left = self.alive_position(Axis::Columns, merge.left)?;
        let right = self.alive_position(Axis::Columns, merge.right)?;
        if top > bottom || left > right {
            return Err(GridError::InvertedRange);
        }
        if self.cell_count(merge) < 2 {
            return Err(GridError::SingleCell);
        }
        Ok(())
    }

    fn alive_position(&self, axis: Axis, id: Id) -> Result<usize, GridError> {
        let lines = self.lines(axis);
        let index = position(lines, id).ok_or_else(|| unknown(axis, id))?;
        if !lines[index].alive {
            return Err(deleted(axis, id));
        }
        Ok(index)
    }

    fn cell_count(&self, merge: &Merge) -> usize {
        let count =
            |lines: &[Line], from: Id, to: Id| match (position(lines, from), position(lines, to)) {
                (Some(a), Some(b)) if a <= b => {
                    lines[a..=b].iter().filter(|line| line.alive).count()
                }
                _ => 0,
            };
        count(&self.rows, merge.top, merge.bottom) * count(&self.columns, merge.left, merge.right)
    }

    /// Intervals are compared on raw positions; with alive corners that is the
    /// same as comparing the alive cells they cover.
    fn overlaps(&self, a: &Merge, b: &Merge) -> bool {
        let span = |lines: &[Line], from: Id, to: Id| {
            (
                position(lines, from).unwrap_or(usize::MAX),
                position(lines, to).unwrap_or(0),
            )
        };
        let intersects = |(a0, a1): (usize, usize), (b0, b1): (usize, usize)| a0 <= b1 && b0 <= a1;
        intersects(
            span(&self.rows, a.top, a.bottom),
            span(&self.rows, b.top, b.bottom),
        ) && intersects(
            span(&self.columns, a.left, a.right),
            span(&self.columns, b.left, b.right),
        )
    }

    /// The validity predicate the sequencer maintains.
    pub fn check_invariants(&self) -> Result<(), GridError> {
        for (axis, lines) in [(Axis::Rows, &self.rows), (Axis::Columns, &self.columns)] {
            for (i, line) in lines.iter().enumerate() {
                if lines[..i].iter().any(|other| other.id == line.id) {
                    return Err(duplicate(axis, line.id));
                }
            }
        }
        if !self.rows.iter().any(|line| line.alive) {
            return Err(GridError::LastRow);
        }
        if !self.columns.iter().any(|line| line.alive) {
            return Err(GridError::LastColumn);
        }
        for (i, merge) in self.merges.iter().enumerate() {
            self.check_merge(merge)?;
            if self.merges[..i]
                .iter()
                .any(|other| self.overlaps(merge, other))
            {
                return Err(GridError::Overlap);
            }
        }
        Ok(())
    }
}

fn alive(lines: &[Line]) -> Vec<Id> {
    lines
        .iter()
        .filter(|line| line.alive)
        .map(|line| line.id)
        .collect()
}

fn position(lines: &[Line], id: Id) -> Option<usize> {
    lines.iter().position(|line| line.id == id)
}

fn alive_index(lines: &[Line], id: Id) -> Option<usize> {
    let index = position(lines, id)?;
    lines[index]
        .alive
        .then(|| lines[..index].iter().filter(|line| line.alive).count())
}

/// First and last alive line between `from` and `to` inclusive.
fn alive_span(lines: &[Line], from: Id, to: Id) -> Option<(Id, Id)> {
    let (a, b) = (position(lines, from)?, position(lines, to)?);
    if a > b {
        return None;
    }
    let mut alive = lines[a..=b].iter().filter(|line| line.alive);
    let first = alive.next()?.id;
    let last = alive.next_back().map_or(first, |line| line.id);
    Some((first, last))
}

fn unknown(axis: Axis, id: Id) -> GridError {
    match axis {
        Axis::Rows => GridError::UnknownRow(id),
        Axis::Columns => GridError::UnknownColumn(id),
    }
}

fn duplicate(axis: Axis, id: Id) -> GridError {
    match axis {
        Axis::Rows => GridError::DuplicateRow(id),
        Axis::Columns => GridError::DuplicateColumn(id),
    }
}

fn deleted(axis: Axis, id: Id) -> GridError {
    match axis {
        Axis::Rows => GridError::DeletedRow(id),
        Axis::Columns => GridError::DeletedColumn(id),
    }
}
