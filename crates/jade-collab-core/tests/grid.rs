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
}
