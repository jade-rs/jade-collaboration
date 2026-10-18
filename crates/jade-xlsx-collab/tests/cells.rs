//! Listing a sheet's filled cells by A1 reference.

mod common;

use common::{generate, open};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_xlsx_collab::{column_label, CellInput, XlsxModel};
use proptest::prelude::*;

/// The cells a reader sees: every visible position, row by row, holding
/// something other than a blank.
fn walk(model: &XlsxModel, sheet: usize) -> Vec<(String, CellInput)> {
    let view = &model.sheets()[sheet];
    let mut cells = Vec::new();
    for (r, row) in view.rows.iter().enumerate() {
        for (c, column) in view.columns.iter().enumerate() {
            match model.value(view.id, *row, *column) {
                None | Some(CellInput::Blank) => {}
                Some(value) => cells.push((
                    format!("{}{}", column_label(c as u32), r + 1),
                    value.clone(),
                )),
            }
        }
    }
    cells
}

fn filled(model: &XlsxModel, sheet: usize) -> Vec<(String, CellInput)> {
    let id = model.sheets()[sheet].id;
    model
        .filled_cells(id)
        .into_iter()
        .map(|(reference, value)| (reference, value.clone()))
        .collect()
}

#[test]
fn a_loaded_sheet_lists_its_values_row_by_row() {
    let (model, _) = open("grid-operations-workbook.xlsx");
    let cells = filled(&model, 0);
    let references: Vec<&str> = cells.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(&references[..4], ["A1", "B1", "C1", "E1"]);
    assert_eq!(cells[5], ("B2".into(), CellInput::Number(10.0)));
    assert_eq!(cells, walk(&model, 0));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// After any edits — values, blanks, rows and columns inserted and
    /// deleted — the listing is the visible grid's non-blank cells in order.
    #[test]
    fn filled_cells_are_the_visible_non_blank_cells(
        choices in prop::collection::vec(any::<u64>(), 0..25),
    ) {
        let (mut model, _) = open("grid-operations-workbook.xlsx");
        let mut next = 1_000_000u64;
        for (step, choice) in choices.into_iter().enumerate() {
            let mut ids = |n: u64| {
                let id = Id::new(next, ReplicaId(1));
                next += n;
                id
            };
            let Some(op) = generate(&model, choice, &mut ids) else { continue };
            let seq = step as u64 + 1;
            let ctx = ApplyContext { origin: ReplicaId(1), base_seq: seq - 1, seq: Some(seq) };
            let _ = model.apply(&op, &ctx);
            prop_assert_eq!(filled(&model, 0), walk(&model, 0));
        }
    }
}
