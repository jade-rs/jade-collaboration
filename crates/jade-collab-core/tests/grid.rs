use jade_collab_core::grid::{Grid, GridError, GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use proptest::prelude::*;

fn id(n: u64) -> Id {
    Id::new(n, ReplicaId(0))
}

/// A grid with rows r1..=r{rows} and columns c101..=c{100+cols}.
fn grid(rows: u64, cols: u64) -> Grid {
    Grid::new((1..=rows).map(id), (101..=100 + cols).map(id))
}

fn merge(top: u64, left: u64, bottom: u64, right: u64) -> Merge {
    Merge {
        top: id(top),
        left: id(left),
        bottom: id(bottom),
        right: id(right),
    }
}

#[test]
fn inserts_rows_and_columns_after_an_anchor() {
    let mut g = grid(2, 2);
    g.apply(&GridOp::InsertRow {
        id: id(50),
        after: Some(id(1)),
    })
    .unwrap();
    g.apply(&GridOp::InsertRow {
        id: id(51),
        after: None,
    })
    .unwrap();
    g.apply(&GridOp::InsertColumn {
        id: id(150),
        after: Some(id(102)),
    })
    .unwrap();
    assert_eq!(g.rows(), vec![id(51), id(1), id(50), id(2)]);
    assert_eq!(g.columns(), vec![id(101), id(102), id(150)]);
    assert_eq!(g.row_index(id(50)), Some(2));
}

#[test]
fn deleted_rows_keep_anchoring_concurrent_inserts() {
    let mut g = grid(3, 1);
    g.apply(&GridOp::DeleteRow { id: id(2) }).unwrap();
    g.apply(&GridOp::InsertRow {
        id: id(60),
        after: Some(id(2)),
    })
    .unwrap();
    assert_eq!(g.rows(), vec![id(1), id(60), id(3)]);
    assert!(g.contains_row(id(2)));
    assert_eq!(g.row_index(id(2)), None);
}

#[test]
fn refuses_to_delete_the_last_row_or_column() {
    let mut g = grid(1, 1);
    assert_eq!(
        g.apply(&GridOp::DeleteRow { id: id(1) }),
        Err(GridError::LastRow)
    );
    assert_eq!(
        g.apply(&GridOp::DeleteColumn { id: id(101) }),
        Err(GridError::LastColumn)
    );
}

#[test]
fn merges_must_not_overlap() {
    let mut g = grid(3, 3);
    g.apply(&GridOp::Merge(merge(1, 101, 2, 102))).unwrap();
    assert_eq!(
        g.apply(&GridOp::Merge(merge(2, 102, 3, 103))),
        Err(GridError::Overlap)
    );
    g.apply(&GridOp::Merge(merge(3, 101, 3, 103))).unwrap();
    assert_eq!(g.merges().len(), 2);
}

#[test]
fn merges_must_be_well_formed() {
    let mut g = grid(3, 3);
    assert_eq!(
        g.apply(&GridOp::Merge(merge(2, 101, 1, 102))),
        Err(GridError::InvertedRange)
    );
    assert_eq!(
        g.apply(&GridOp::Merge(merge(1, 101, 1, 101))),
        Err(GridError::SingleCell)
    );
    assert_eq!(
        g.apply(&GridOp::Merge(merge(1, 101, 9, 102))),
        Err(GridError::UnknownRow(id(9)))
    );
}

#[test]
fn deleting_an_anchor_row_shrinks_the_merge() {
    let mut g = grid(3, 1);
    g.apply(&GridOp::Merge(merge(1, 101, 3, 101))).unwrap();
    g.apply(&GridOp::DeleteRow { id: id(1) }).unwrap();
    assert_eq!(g.merges(), &[merge(2, 101, 3, 101)]);
    g.apply(&GridOp::DeleteRow { id: id(3) }).unwrap();
    assert!(
        g.merges().is_empty(),
        "a merge left with one cell disappears"
    );
    g.check_invariants().unwrap();
}

#[test]
fn unmerge_removes_the_region_at_its_top_left() {
    let mut g = grid(2, 2);
    g.apply(&GridOp::Merge(merge(1, 101, 2, 102))).unwrap();
    assert_eq!(
        g.apply(&GridOp::Unmerge {
            top: id(2),
            left: id(101)
        }),
        Err(GridError::NoSuchMerge)
    );
    g.apply(&GridOp::Unmerge {
        top: id(1),
        left: id(101),
    })
    .unwrap();
    assert!(g.merges().is_empty());
}

/// The reason tables need a sequencer: two merges that are each valid on the
/// common ancestor overlap once a CRDT unions them. (Lean:
/// `crdt_union_breaks_invariant`.)
#[test]
fn naive_union_of_concurrent_merges_breaks_the_invariant() {
    let base = grid(3, 3);
    let mut alice = base.clone();
    alice.apply(&GridOp::Merge(merge(1, 101, 2, 102))).unwrap();
    let mut bob = base.clone();
    bob.apply(&GridOp::Merge(merge(2, 102, 3, 103))).unwrap();
    alice.check_invariants().unwrap();
    bob.check_invariants().unwrap();

    let mut merges = alice.merges().to_vec();
    merges.extend(bob.merges().iter().cloned());
    let union = Grid::with_merges_unchecked(base.all_rows(), base.all_columns(), merges);
    assert_eq!(union.check_invariants(), Err(GridError::Overlap));

    // Sequenced: whichever arrives second is validated against the first.
    let mut sequenced = alice.clone();
    assert_eq!(
        sequenced.apply(&GridOp::Merge(merge(2, 102, 3, 103))),
        Err(GridError::Overlap)
    );
}

/// Word and Excel: a row inserted strictly inside a merge's rows joins it
/// (the merge grows); one inserted above its top or below its bottom does
/// not. The merge keeps its corners either way.
#[test]
fn a_row_inserted_inside_a_merge_joins_it() {
    let mut g = grid(3, 3);
    let region = merge(1, 101, 2, 102);
    g.apply(&GridOp::Merge(region)).unwrap();
    g.apply(&GridOp::InsertRow {
        id: id(50),
        after: Some(id(1)),
    })
    .unwrap();
    assert_eq!(g.merges(), &[region]);
    assert_eq!(g.merge_at(id(50), id(101)), Some(&region));
    assert_eq!(g.merge_at(id(50), id(102)), Some(&region));
    assert_eq!(g.merge_at(id(50), id(103)), None);
    // Below the bottom and above the top: outside.
    g.apply(&GridOp::InsertRow {
        id: id(51),
        after: Some(id(2)),
    })
    .unwrap();
    g.apply(&GridOp::InsertRow {
        id: id(52),
        after: None,
    })
    .unwrap();
    assert_eq!(g.merge_at(id(51), id(101)), None);
    assert_eq!(g.merge_at(id(52), id(101)), None);
    assert_eq!(g.merges(), &[region]);
    g.check_invariants().unwrap();
}

#[test]
fn a_column_inserted_inside_a_merge_joins_it() {
    let mut g = grid(2, 3);
    let region = merge(1, 101, 1, 103);
    g.apply(&GridOp::Merge(region)).unwrap();
    g.apply(&GridOp::InsertColumn {
        id: id(150),
        after: Some(id(102)),
    })
    .unwrap();
    assert_eq!(g.merge_at(id(1), id(150)), Some(&region));
    assert_eq!(g.merge_at(id(2), id(150)), None);
    g.apply(&GridOp::InsertColumn {
        id: id(151),
        after: Some(id(103)),
    })
    .unwrap();
    assert_eq!(g.merge_at(id(1), id(151)), None);
    assert_eq!(g.merges(), &[region]);
}

/// A line inserted after a deleted line inside a merge is inside too: what
/// counts is where it lands among the alive lines.
#[test]
fn an_insert_after_a_deleted_line_inside_a_merge_joins_it() {
    let mut g = grid(3, 1);
    let region = merge(1, 101, 3, 101);
    g.apply(&GridOp::Merge(region)).unwrap();
    g.apply(&GridOp::DeleteRow { id: id(2) }).unwrap();
    g.apply(&GridOp::InsertRow {
        id: id(60),
        after: Some(id(2)),
    })
    .unwrap();
    assert_eq!(g.merge_at(id(60), id(101)), Some(&region));
    // After a deleted line just below the bottom: outside.
    g.apply(&GridOp::DeleteRow { id: id(3) }).unwrap();
    assert_eq!(g.merges(), &[merge(1, 101, 60, 101)]);
    g.apply(&GridOp::InsertRow {
        id: id(61),
        after: Some(id(3)),
    })
    .unwrap();
    assert_eq!(g.merge_at(id(61), id(101)), None);
}

/// Deleting a merge's edge line moves that edge inward to the next alive
/// line; deleting an inner line just shrinks it.
#[test]
fn deleting_a_merge_edge_moves_it_inward() {
    let mut g = grid(4, 4);
    g.apply(&GridOp::Merge(merge(1, 101, 3, 103))).unwrap();
    g.apply(&GridOp::DeleteRow { id: id(3) }).unwrap();
    assert_eq!(g.merges(), &[merge(1, 101, 2, 103)]);
    g.apply(&GridOp::DeleteColumn { id: id(101) }).unwrap();
    assert_eq!(g.merges(), &[merge(1, 102, 2, 103)]);
    g.apply(&GridOp::DeleteColumn { id: id(102) }).unwrap();
    assert_eq!(g.merges(), &[merge(1, 103, 2, 103)]);
    assert_eq!(g.merge_at(id(2), id(103)), Some(&merge(1, 103, 2, 103)));
    assert_eq!(g.merge_at(id(2), id(101)), None, "a deleted column");
    g.check_invariants().unwrap();
}

/// Deleting the last line a merge has on an axis removes it, and so does a
/// deletion that leaves it a single cell (it is then just that cell).
#[test]
fn a_merge_without_two_cells_disappears() {
    let mut g = grid(3, 3);
    g.apply(&GridOp::Merge(merge(1, 101, 1, 103))).unwrap();
    g.apply(&GridOp::Merge(merge(2, 101, 3, 101))).unwrap();
    g.apply(&GridOp::DeleteRow { id: id(1) }).unwrap();
    assert_eq!(g.merges(), &[merge(2, 101, 3, 101)]);
    g.apply(&GridOp::DeleteRow { id: id(2) }).unwrap();
    assert!(g.merges().is_empty());
    assert_eq!(g.merge_at(id(3), id(101)), None);
    // A 1 × 2 merge losing a column.
    let mut g = grid(2, 2);
    g.apply(&GridOp::Merge(merge(1, 101, 1, 102))).unwrap();
    g.apply(&GridOp::DeleteColumn { id: id(101) }).unwrap();
    assert!(g.merges().is_empty());
    g.check_invariants().unwrap();
}

#[test]
fn merge_at_names_the_region_covering_an_alive_cell() {
    let mut g = grid(3, 3);
    let region = merge(2, 102, 3, 103);
    g.apply(&GridOp::Merge(region)).unwrap();
    assert_eq!(g.merge_at(id(3), id(103)), Some(&region));
    assert_eq!(g.merge_at(id(1), id(103)), None);
    assert_eq!(g.merge_at(id(9), id(103)), None, "an unknown row");
    g.apply(&GridOp::DeleteRow { id: id(3) }).unwrap();
    assert_eq!(g.merge_at(id(3), id(103)), None, "a deleted row");
    assert_eq!(g.merge_at(id(2), id(103)), Some(&merge(2, 102, 2, 103)));
}

// ---------------------------------------------------------------------------

fn op() -> impl Strategy<Value = GridOp> {
    // Identifiers come from a small pool so operations hit existing, deleted
    // and unknown rows/columns alike.
    let row = (0u64..12).prop_map(id);
    let col = (100u64..112).prop_map(id);
    prop_oneof![
        (row.clone(), prop::option::of(row.clone()))
            .prop_map(|(id, after)| GridOp::InsertRow { id, after }),
        row.clone().prop_map(|id| GridOp::DeleteRow { id }),
        (col.clone(), prop::option::of(col.clone()))
            .prop_map(|(id, after)| GridOp::InsertColumn { id, after }),
        col.clone().prop_map(|id| GridOp::DeleteColumn { id }),
        (row.clone(), col.clone(), row.clone(), col.clone()).prop_map(
            |(top, left, bottom, right)| {
                GridOp::Merge(Merge {
                    top,
                    left,
                    bottom,
                    right,
                })
            }
        ),
        (row, col).prop_map(|(top, left)| GridOp::Unmerge { top, left }),
    ]
}

/// Alive lines from `first` through `last`, in order.
fn alive_between(lines: &[Id], first: Id, last: Id) -> Vec<Id> {
    match (
        lines.iter().position(|l| *l == first),
        lines.iter().position(|l| *l == last),
    ) {
        (Some(a), Some(b)) if a <= b => lines[a..=b].to_vec(),
        _ => Vec::new(),
    }
}

/// The alive cells each merge covers, sorted, in the grid's merge order.
fn covered(g: &Grid) -> Vec<Vec<(Id, Id)>> {
    g.merges()
        .iter()
        .map(|m| {
            let mut cells: Vec<(Id, Id)> = alive_between(&g.rows(), m.top, m.bottom)
                .into_iter()
                .flat_map(|r| {
                    alive_between(&g.columns(), m.left, m.right)
                        .into_iter()
                        .map(move |c| (r, c))
                })
                .collect();
            cells.sort();
            cells
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// Validated application keeps every invariant, and a rejected operation
    /// leaves the grid exactly as it was (Lean: `sequenced_preserves`).
    #[test]
    fn validated_ops_preserve_invariants(ops in prop::collection::vec(op(), 0..60)) {
        let mut g = grid(3, 3);
        for op in &ops {
            let before = g.clone();
            match g.apply(op) {
                Ok(()) => {}
                Err(_) => prop_assert_eq!(&g, &before),
            }
            prop_assert_eq!(g.check_invariants(), Ok(()));
            prop_assert!(!g.rows().is_empty());
            prop_assert!(!g.columns().is_empty());
        }
    }

    /// `validate` predicts `apply` exactly.
    #[test]
    fn validate_agrees_with_apply(ops in prop::collection::vec(op(), 0..40)) {
        let mut g = grid(2, 2);
        for op in &ops {
            let predicted = g.validate(op);
            prop_assert_eq!(predicted, g.apply(op));
        }
    }

    /// Rows and columns go through merges as in Word and Excel, stated on
    /// the cells a merge covers rather than on its corners: an insert at
    /// alive index `i` joins every merge whose alive span `t..=b` has
    /// `t < i <= b`, and nothing else changes; a deletion takes its line out
    /// of every merge, and a merge left with fewer than two cells is gone.
    /// `merge_at` agrees with the covered cells.
    #[test]
    fn structure_goes_through_merges_like_word(ops in prop::collection::vec(op(), 0..60)) {
        let mut g = grid(4, 4);
        for op in &ops {
            let before = g.clone();
            if g.apply(op).is_err() {
                continue;
            }
            let old = covered(&before);
            let new = covered(&g);
            match *op {
                GridOp::InsertRow { id: line, .. } | GridOp::InsertColumn { id: line, .. } => {
                    let rows_axis = matches!(op, GridOp::InsertRow { .. });
                    let index = if rows_axis { g.row_index(line) } else { g.column_index(line) }.unwrap();
                    prop_assert_eq!(g.merges(), before.merges());
                    let expected: Vec<Vec<(Id, Id)>> = before
                        .merges()
                        .iter()
                        .zip(&old)
                        .map(|(m, cells)| {
                            let (t, b) = if rows_axis {
                                (g.row_index(m.top).unwrap(), g.row_index(m.bottom).unwrap())
                            } else {
                                (g.column_index(m.left).unwrap(), g.column_index(m.right).unwrap())
                            };
                            let mut cells = cells.clone();
                            if t < index && index <= b {
                                if rows_axis {
                                    let across = alive_between(&g.columns(), m.left, m.right);
                                    cells.extend(across.iter().map(|c| (line, *c)));
                                } else {
                                    let down = alive_between(&g.rows(), m.top, m.bottom);
                                    cells.extend(down.iter().map(|r| (*r, line)));
                                }
                            }
                            cells.sort();
                            cells
                        })
                        .collect();
                    prop_assert_eq!(&new, &expected);
                }
                GridOp::DeleteRow { id: line } | GridOp::DeleteColumn { id: line } => {
                    let rows_axis = matches!(op, GridOp::DeleteRow { .. });
                    let expected: Vec<Vec<(Id, Id)>> = old
                        .iter()
                        .map(|cells| {
                            cells
                                .iter()
                                .copied()
                                .filter(|(r, c)| if rows_axis { *r != line } else { *c != line })
                                .collect::<Vec<_>>()
                        })
                        .filter(|cells| cells.len() >= 2)
                        .collect();
                    prop_assert_eq!(&new, &expected);
                }
                _ => {}
            }
            for (m, cells) in g.merges().iter().zip(&new) {
                for (r, c) in cells {
                    prop_assert_eq!(g.merge_at(*r, *c), Some(m));
                }
            }
            for r in g.rows() {
                for c in g.columns() {
                    if !new.iter().any(|cells| cells.contains(&(r, c))) {
                        prop_assert_eq!(g.merge_at(r, c), None);
                    }
                }
            }
        }
    }
}
