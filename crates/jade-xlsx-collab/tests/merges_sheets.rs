//! Merged cells and sheet management: sequenced operations, realised by
//! jade's merge_cells / add_sheet / rename_sheet / delete_sheet / move_sheet.

mod common;

use common::{assert_agree, open};
use jade_collab_core::grid::Merge;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_xlsx_collab::{CellInput, FormatChange, JadeXlsx, XlsxModel, XlsxOp};
use proptest::prelude::*;

fn ctx(origin: u64, base_seq: u64, seq: u64) -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(origin),
        base_seq,
        seq: Some(seq),
    }
}

fn stamp(n: u64) -> Id {
    Id::new(1_000_000 + n, ReplicaId(9))
}

/// Applies `op` to the model and, through the materializer, to jade.
fn run(model: &mut XlsxModel, jade: &mut JadeXlsx, op: &XlsxOp, ctx: &ApplyContext) {
    if XlsxModel::lane(op) == Lane::Sequenced {
        jade.admit(model, op)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
    }
    let effect = model
        .apply(op, ctx)
        .unwrap_or_else(|e| panic!("{op:?}: {e}"));
    jade.materialize(model, effect, ctx)
        .unwrap_or_else(|e| panic!("{op:?}: {e}"));
    assert_agree(model, jade, &format!("after {op:?}"));
}

fn cell(model: &XlsxModel, reference: &str) -> (Id, Id, Id) {
    model.locate("Operations", reference).unwrap()
}

fn set(model: &XlsxModel, reference: &str, n: u64, value: CellInput) -> XlsxOp {
    let (sheet, row, column) = cell(model, reference);
    XlsxOp::SetCell {
        sheet,
        row,
        column,
        stamp: stamp(n),
        value,
    }
}

fn merge(model: &XlsxModel, from: &str, to: &str, n: u64) -> XlsxOp {
    let (sheet, top, left) = cell(model, from);
    let (_, bottom, right) = cell(model, to);
    XlsxOp::MergeCells {
        sheet,
        merge: Merge {
            top,
            left,
            bottom,
            right,
        },
        stamp: stamp(n),
    }
}

#[test]
fn lanes() {
    let (model, _) = open("grid-operations-workbook.xlsx");
    let sheet = model.sheets()[0].id;
    for op in [
        merge(&model, "A6", "B7", 1),
        XlsxOp::UnmergeCells {
            sheet,
            top: cell(&model, "E1").1,
            left: cell(&model, "E1").2,
        },
        XlsxOp::AddSheet {
            id: stamp(1),
            name: "New".into(),
            after: None,
            rows: 2,
            columns: 2,
        },
        XlsxOp::RenameSheet {
            sheet,
            name: "Renamed".into(),
        },
        XlsxOp::DeleteSheet { sheet },
        XlsxOp::MoveSheet { sheet, to: 0 },
    ] {
        assert_eq!(XlsxModel::lane(&op), Lane::Sequenced, "{op:?}");
    }
}

#[test]
fn merging_keeps_the_top_left_value_and_clears_the_rest() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    run(
        &mut model,
        &mut jade,
        &set(&model, "A6", 1, CellInput::Number(1.0)),
        &ctx(1, 0, 1),
    );
    run(
        &mut model,
        &mut jade,
        &set(&model, "B7", 2, CellInput::Text("x".into())),
        &ctx(1, 1, 2),
    );
    run(
        &mut model,
        &mut jade,
        &merge(&model, "A6", "B7", 3),
        &ctx(1, 2, 3),
    );
    let (sheet, a6r, a6c) = cell(&model, "A6");
    let (_, b7r, b7c) = cell(&model, "B7");
    assert_eq!(model.value(sheet, a6r, a6c), Some(&CellInput::Number(1.0)));
    assert_eq!(model.value(sheet, b7r, b7c), Some(&CellInput::Blank));
    // Unmerging leaves values where they are.
    run(
        &mut model,
        &mut jade,
        &XlsxOp::UnmergeCells {
            sheet,
            top: a6r,
            left: a6c,
        },
        &ctx(1, 3, 4),
    );
    assert_eq!(model.value(sheet, a6r, a6c), Some(&CellInput::Number(1.0)));
}

#[test]
fn a_newer_concurrent_write_into_a_merged_cell_survives_the_merge() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    // Replica 2 typed into B7 with a newer stamp than the merge, and its
    // write reached the server first; the merge (older stamp) must not erase
    // it — in the model or in the file.
    run(
        &mut model,
        &mut jade,
        &set(&model, "B7", 50, CellInput::Text("newer".into())),
        &ctx(2, 0, 1),
    );
    run(
        &mut model,
        &mut jade,
        &merge(&model, "A6", "B7", 10),
        &ctx(1, 0, 2),
    );
    let (sheet, row, column) = cell(&model, "B7");
    assert_eq!(
        model.value(sheet, row, column),
        Some(&CellInput::Text("newer".into()))
    );
    // An older write arriving after the merge loses to it.
    run(
        &mut model,
        &mut jade,
        &set(&model, "A7", 5, CellInput::Text("older".into())),
        &ctx(3, 0, 3),
    );
    let (_, row, column) = cell(&model, "A7");
    assert_eq!(model.value(sheet, row, column), Some(&CellInput::Blank));
}

#[test]
fn merges_are_refused_over_other_merges_tables_and_single_cells() {
    let (model, jade) = open("grid-operations-workbook.xlsx");
    let refused = |op: XlsxOp| {
        let mut copy = model.clone();
        let err = jade
            .admit(&copy, &op)
            .err()
            .or_else(|| copy.apply(&op, &ctx(1, 0, 1)).err());
        assert!(err.is_some(), "{op:?} was accepted");
        assert_eq!(copy, model, "a refused merge changed the model");
    };
    refused(merge(&model, "D1", "E2", 1)); // overlaps E1:F1
    refused(merge(&model, "B2", "D3", 1)); // touches the Excel table A1:C4
    refused(merge(&model, "A6", "A6", 1)); // one cell
}

#[test]
fn merged_cells_show_the_anchor_format() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let (sheet, row, column) = cell(&model, "A6");
    run(
        &mut model,
        &mut jade,
        &XlsxOp::SetFormat {
            sheet,
            rows: vec![row],
            columns: vec![column],
            stamp: stamp(1),
            change: FormatChange {
                bold: Some(true),
                ..FormatChange::default()
            },
        },
        &ctx(1, 0, 1),
    );
    run(
        &mut model,
        &mut jade,
        &merge(&model, "A6", "B7", 2),
        &ctx(1, 1, 2),
    );
    let (_, b7r, b7c) = cell(&model, "B7");
    assert_eq!(model.format(sheet, b7r, b7c).bold, Some(true));
    run(
        &mut model,
        &mut jade,
        &XlsxOp::UnmergeCells {
            sheet,
            top: row,
            left: column,
        },
        &ctx(1, 2, 3),
    );
    // jade copied the anchor's format into the cells it covered.
    assert_eq!(model.format(sheet, b7r, b7c).bold, Some(true));
}

#[test]
fn sheets_are_added_renamed_moved_and_deleted() {
    let (mut model, mut jade) = open("clipboard-workbook.xlsx");
    let names = |model: &XlsxModel| {
        model
            .sheets()
            .into_iter()
            .map(|s| s.name)
            .collect::<Vec<_>>()
    };
    let jade_names = |jade: &JadeXlsx| {
        jade.workbook()
            .summary()
            .sheets
            .into_iter()
            .map(|s| s.name)
            .collect::<Vec<_>>()
    };
    let first = model.sheets()[0].id;
    run(
        &mut model,
        &mut jade,
        &XlsxOp::AddSheet {
            id: stamp(1),
            name: "Notes".into(),
            after: Some(first),
            rows: 3,
            columns: 2,
        },
        &ctx(1, 0, 1),
    );
    assert_eq!(names(&model), jade_names(&jade));
    assert_eq!(names(&model)[1], "Notes");
    // A formula on the new sheet that points at the first one…
    let notes = model.sheets()[1].clone();
    run(
        &mut model,
        &mut jade,
        &XlsxOp::SetCell {
            sheet: notes.id,
            row: notes.rows[0],
            column: notes.columns[0],
            stamp: stamp(2),
            value: CellInput::Formula("Operations!B2*2".into()),
        },
        &ctx(1, 1, 2),
    );
    // …follows a rename, as in jade.
    run(
        &mut model,
        &mut jade,
        &XlsxOp::RenameSheet {
            sheet: first,
            name: "Q1 ops".into(),
        },
        &ctx(1, 2, 3),
    );
    assert_eq!(
        model.value(notes.id, notes.rows[0], notes.columns[0]),
        Some(&CellInput::Formula("'Q1 ops'!B2*2".into()))
    );
    run(
        &mut model,
        &mut jade,
        &XlsxOp::MoveSheet {
            sheet: notes.id,
            to: 0,
        },
        &ctx(1, 3, 4),
    );
    assert_eq!(names(&model), jade_names(&jade));
    assert_eq!(names(&model)[0], "Notes");
    run(
        &mut model,
        &mut jade,
        &XlsxOp::DeleteSheet { sheet: first },
        &ctx(1, 4, 5),
    );
    assert_eq!(names(&model), jade_names(&jade));
    assert_eq!(
        model.value(notes.id, notes.rows[0], notes.columns[0]),
        Some(&CellInput::Formula("#REF!B2*2".into()))
    );
    let reopened = JadeXlsx::open(&jade.export().unwrap()).unwrap();
    assert_eq!(names(&model), jade_names(&reopened));
    assert_agree(&model, &reopened, "after export");
}

#[test]
fn bad_sheet_names_and_the_last_sheet_are_refused() {
    let (model, _) = open("grid-operations-workbook.xlsx");
    let only = model.sheets()[0].id;
    for op in [
        XlsxOp::RenameSheet {
            sheet: only,
            name: "a/b".into(),
        },
        XlsxOp::AddSheet {
            id: stamp(1),
            name: "operations".into(), // duplicate ignoring case
            after: None,
            rows: 1,
            columns: 1,
        },
        XlsxOp::DeleteSheet { sheet: only },
    ] {
        let mut copy = model.clone();
        assert!(copy.apply(&op, &ctx(1, 0, 1)).is_err(), "{op:?}");
        assert_eq!(copy, model);
    }
}

#[test]
fn a_formula_typed_before_a_rename_it_did_not_see_is_rewritten() {
    let (mut model, mut jade) = open("clipboard-workbook.xlsx");
    let first = model.sheets()[0].id;
    let target = model.sheets()[1].clone();
    // Replica 2 renames (seq 1); replica 1, still at seq 0, typed a formula
    // naming the old sheet.
    run(
        &mut model,
        &mut jade,
        &XlsxOp::RenameSheet {
            sheet: first,
            name: "Renamed".into(),
        },
        &ctx(2, 0, 1),
    );
    run(
        &mut model,
        &mut jade,
        &XlsxOp::SetCell {
            sheet: target.id,
            row: target.rows[0],
            column: target.columns[0],
            stamp: stamp(3),
            value: CellInput::Formula("Operations!A2".into()),
        },
        &ctx(1, 0, 2),
    );
    assert_eq!(
        model.value(target.id, target.rows[0], target.columns[0]),
        Some(&CellInput::Formula("Renamed!A2".into()))
    );
}

#[test]
fn writes_to_a_deleted_sheet_are_accepted_and_invisible() {
    let (mut model, mut jade) = open("clipboard-workbook.xlsx");
    let gone = model.sheets()[1].clone();
    run(
        &mut model,
        &mut jade,
        &XlsxOp::DeleteSheet { sheet: gone.id },
        &ctx(2, 0, 1),
    );
    // A convergent write made before the deletion was seen still applies
    // (it must commute with the deletion) but shows nowhere.
    let op = XlsxOp::SetCell {
        sheet: gone.id,
        row: gone.rows[0],
        column: gone.columns[0],
        stamp: stamp(9),
        value: CellInput::Number(7.0),
    };
    let effect = model.apply(&op, &ctx(1, 0, 2)).unwrap();
    assert!(effect.is_empty());
    assert!(model.sheets().iter().all(|s| s.id != gone.id));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// A merge commutes with any convergent cell write: whichever order a
    /// replica applies them in, the model is the same (the LWW clear is a
    /// write with the merge's stamp; Lean: `Lww.write_comm`,
    /// `CellCommute.cell_edit_comm_grid`).
    #[test]
    fn merges_commute_with_cell_writes(
        writes in prop::collection::vec((0usize..2, 0usize..2, 0u64..40, -5i32..5), 1..6),
        merge_stamp in 0u64..40,
    ) {
        let (model, _) = open("grid-operations-workbook.xlsx");
        let merge_op = merge(&model, "A6", "B7", merge_stamp * 2 + 1);
        let refs = [["A6", "B6"], ["A7", "B7"]];
        let ops: Vec<XlsxOp> = writes
            .iter()
            .map(|(r, c, s, v)| set(&model, refs[*r][*c], s * 2, CellInput::Number(f64::from(*v))))
            .collect();
        let mut merged_first = model.clone();
        merged_first.apply(&merge_op, &ctx(1, 0, 1)).unwrap();
        for op in &ops {
            merged_first.apply(op, &ctx(2, 0, 2)).unwrap();
        }
        let mut merged_last = model.clone();
        for op in &ops {
            merged_last.apply(op, &ctx(2, 0, 1)).unwrap();
        }
        merged_last.apply(&merge_op, &ctx(1, 0, 2)).unwrap();
        prop_assert_eq!(merged_first, merged_last);
    }
}
