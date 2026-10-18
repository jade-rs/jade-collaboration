//! A small document with one of everything: an RGA text (convergent), cell
//! values in LWW registers (convergent) and a table grid (sequenced).
#![allow(dead_code)]

use std::collections::BTreeMap;

use jade_collab_core::grid::{Grid, GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::lww::LwwRegister;
use jade_collab_core::rga::Rga;
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer, Rejection, Replica};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToyDoc {
    pub text: Rga<char>,
    pub grid: Grid,
    pub cells: BTreeMap<(Id, Id), LwwRegister<i64>>,
    /// The log position each grid op was applied at (`None` while
    /// tentative) — state that depends on the apply context, like the xlsx
    /// model's structure log.
    pub grid_log: Vec<Option<u64>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToyOp {
    Insert {
        id: Id,
        parent: Option<Id>,
        ch: char,
    },
    Delete {
        id: Id,
    },
    SetCell {
        row: Id,
        col: Id,
        stamp: Id,
        value: i64,
    },
    Grid(GridOp),
}

pub fn origin(n: u64) -> Id {
    Id::new(n, ReplicaId::ORIGIN)
}

impl ToyDoc {
    /// Text "ab", a 2x2 grid.
    pub fn sample() -> Self {
        let mut text = Rga::new();
        text.insert_run(origin(1), None, ['a', 'b']).unwrap();
        Self {
            text,
            grid: Grid::new([origin(10), origin(11)], [origin(20), origin(21)]),
            cells: BTreeMap::new(),
            grid_log: Vec::new(),
        }
    }

    pub fn string(&self) -> String {
        self.text.visible().map(|(_, c)| *c).collect()
    }

    pub fn cell(&self, row: Id, col: Id) -> Option<i64> {
        self.cells.get(&(row, col)).and_then(|r| r.get().copied())
    }
}

impl Document for ToyDoc {
    type Op = ToyOp;
    type Effect = ();

    fn lane(op: &ToyOp) -> Lane {
        match op {
            ToyOp::Grid(_) => Lane::Sequenced,
            _ => Lane::Convergent,
        }
    }

    fn apply(&mut self, op: &ToyOp, ctx: &ApplyContext) -> Result<(), Rejection> {
        match op {
            ToyOp::Insert { id, parent, ch } => {
                self.text
                    .insert(*id, *parent, *ch)
                    .map_err(Rejection::from)?;
            }
            ToyOp::Delete { id } => {
                self.text.delete(*id).map_err(Rejection::from)?;
            }
            ToyOp::SetCell {
                row,
                col,
                stamp,
                value,
            } => {
                if !self.grid.contains_row(*row) || !self.grid.contains_column(*col) {
                    return Err(Rejection::new("no such cell"));
                }
                self.cells
                    .entry((*row, *col))
                    .or_default()
                    .apply(*stamp, *value);
            }
            ToyOp::Grid(op) => {
                self.grid.apply(op).map_err(Rejection::from)?;
                self.grid_log.push(ctx.seq);
            }
        }
        Ok(())
    }

    fn max_lamport(&self) -> u64 {
        let grid = self
            .grid
            .all_rows()
            .into_iter()
            .chain(self.grid.all_columns())
            .map(|id| id.lamport)
            .max()
            .unwrap_or(0);
        let cells = self
            .cells
            .values()
            .filter_map(|r| r.stamp())
            .map(|id| id.lamport)
            .max()
            .unwrap_or(0);
        self.text.max_lamport().max(grid).max(cells)
    }
}

/// Builds a random but meaningful operation for `replica` from `choice`.
/// Convergent cell writes only target confirmed rows/columns, so they never
/// depend on a sequenced operation that may still be rejected.
pub fn generate(replica: &mut Replica<ToyDoc>, choice: u64) -> ToyOp {
    let view = replica.view().clone();
    let confirmed = replica.confirmed().clone();
    let pick = |n: usize, salt: u64| ((choice >> salt) as usize) % n.max(1);
    match choice % 10 {
        0..=3 => {
            let ids = view.text.ids();
            let parent = if ids.is_empty() || pick(ids.len() + 1, 8) == ids.len() {
                None
            } else {
                Some(ids[pick(ids.len() + 1, 8)])
            };
            let ch = (b'a' + (choice >> 20) as u8 % 26) as char;
            ToyOp::Insert {
                id: replica.next_id(),
                parent,
                ch,
            }
        }
        4 => {
            let len = view.text.visible_len();
            if len == 0 {
                return ToyOp::Insert {
                    id: replica.next_id(),
                    parent: None,
                    ch: 'z',
                };
            }
            ToyOp::Delete {
                id: view.text.visible_id_at(pick(len, 8)).unwrap(),
            }
        }
        5 => {
            let rows = confirmed.grid.all_rows();
            let cols = confirmed.grid.all_columns();
            ToyOp::SetCell {
                row: rows[pick(rows.len(), 8)],
                col: cols[pick(cols.len(), 16)],
                stamp: replica.next_id(),
                value: (choice >> 24) as i64 % 100,
            }
        }
        _ => {
            let rows = view.grid.all_rows();
            let cols = view.grid.all_columns();
            let row = |salt| rows[pick(rows.len(), salt)];
            let col = |salt| cols[pick(cols.len(), salt)];
            ToyOp::Grid(match (choice >> 4) % 6 {
                0 => GridOp::InsertRow {
                    id: replica.next_id(),
                    after: Some(row(8)),
                },
                1 => GridOp::DeleteRow { id: row(8) },
                2 => GridOp::InsertColumn {
                    id: replica.next_id(),
                    after: Some(col(8)),
                },
                3 => GridOp::DeleteColumn { id: col(8) },
                4 => GridOp::Merge(Merge {
                    top: row(8),
                    left: col(16),
                    bottom: row(24),
                    right: col(32),
                }),
                _ => GridOp::Unmerge {
                    top: row(8),
                    left: col(16),
                },
            })
        }
    }
}

/// A materializer standing in for a real file: it mirrors the text, refuses
/// structure while the mirrored text's length is a multiple of three, and
/// reports a fault whenever an operation leaves it a multiple of five. Its
/// "file" is the mirrored text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mirror {
    pub text: String,
}

impl Mirror {
    pub fn load(file: &str) -> Self {
        Self {
            text: file.to_string(),
        }
    }

    pub fn export(&self) -> String {
        self.text.clone()
    }
}

impl Materializer<ToyDoc> for Mirror {
    fn admit(&self, _: &ToyDoc, op: &ToyOp) -> Result<(), Rejection> {
        match op {
            ToyOp::Grid(_) if self.text.chars().count() % 3 == 0 => {
                Err(Rejection::new("the file is busy"))
            }
            _ => Ok(()),
        }
    }

    fn materialize(&mut self, document: &ToyDoc, _: (), _: &ApplyContext) -> Result<(), String> {
        self.text = document.string();
        if self.text.chars().count() % 5 == 0 {
            return Err(format!("{} is five-ish", self.text));
        }
        Ok(())
    }
}
