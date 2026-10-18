//! What jade itself does when a row or column goes through merged cells —
//! the behaviour the model follows (`merged_tables.rs`), pinned here with
//! jade's own commands so a change in jade shows up as a failure.
//!
//! As in Word: a row (column) inserted strictly inside a vertical
//! (horizontal) merge continues it, one inserted at its edge does not;
//! deleting a merge's top row drops the merged cell's `w:tc` and makes the
//! next one restart the merge; deleting its left column keeps the `w:tc`
//! one column narrower; a merge left with one cell is an ordinary cell.

mod common;

use common::{fixture, jade_tables, TableLayout};
use futures::executor::block_on;
use jade_docx_collab::JadeDocx;
use jade_docx_document_core::headless_api::apply_headless_commands_to_document;
use jade_docx_document_core::word_document::WordDocument;
use jade_docx_document_core::Options;
use jade_docx_headless_api::{HeadlessCommand, HeadlessEditRequest, HeadlessTableCellSelection};

fn run(word: &mut WordDocument, command: HeadlessCommand) {
    apply_headless_commands_to_document(
        word,
        &HeadlessEditRequest {
            commands: vec![command],
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

/// A 3 × 3 table holding "r{row}c{column}", with the cells from (`top`,
/// `left`) to (`bottom`, `right`) merged.
fn merged(top: usize, left: usize, bottom: usize, right: usize) -> WordDocument {
    let mut word = block_on(WordDocument::load(
        &fixture("jade-core/hello.docx"),
        &Options::default(),
    ))
    .unwrap();
    run(
        &mut word,
        HeadlessCommand::InsertTable {
            after_paragraph: 0,
            rows: 3,
            columns: 3,
            column_widths: Vec::new(),
            caret_offset: None,
        },
    );
    for row in 0..3 {
        for column in 0..3 {
            run(
                &mut word,
                HeadlessCommand::SetTableCellText {
                    table_order_index: 0,
                    row,
                    column,
                    text: format!("r{row}c{column}"),
                },
            );
        }
    }
    let selected_cells = (top..=bottom)
        .flat_map(|row| {
            (left..=right).map(move |column| HeadlessTableCellSelection {
                row,
                column,
                column_span: 1,
            })
        })
        .collect();
    run(
        &mut word,
        HeadlessCommand::MergeTableCells {
            table_order_index: 0,
            selected_cells,
        },
    );
    word
}

fn layout(word: &WordDocument) -> TableLayout {
    let jade = JadeDocx::new(word.clone());
    let layout = jade_tables(&jade).remove(0).expect("a regular table");
    // What jade writes reads back the same.
    let reloaded = JadeDocx::load(&jade.export_base().unwrap()).unwrap();
    assert_eq!(jade_tables(&reloaded).remove(0).as_ref(), Some(&layout));
    layout
}

fn cell(layout: &TableLayout, row: usize, column: usize) -> Option<Vec<&str>> {
    layout
        .cells
        .get(&(row, column))
        .map(|t| t.iter().map(String::as_str).collect())
}

fn insert_row(word: &mut WordDocument, row_index: usize) {
    run(
        word,
        HeadlessCommand::InsertTableRow {
            table_order_index: 0,
            row_index,
            template_row: None,
        },
    );
}

fn insert_column(word: &mut WordDocument, column_index: usize) {
    run(
        word,
        HeadlessCommand::InsertTableColumn {
            table_order_index: 0,
            column_index,
            width: None,
            template_column: None,
        },
    );
}

fn delete_row(word: &mut WordDocument, row_index: usize) {
    run(
        word,
        HeadlessCommand::DeleteTableRow {
            table_order_index: 0,
            row_index,
        },
    );
}

fn delete_column(word: &mut WordDocument, column_index: usize) {
    run(
        word,
        HeadlessCommand::DeleteTableColumn {
            table_order_index: 0,
            column_index,
        },
    );
}

#[test]
fn merging_puts_the_content_in_the_top_left_cell() {
    let word = merged(0, 0, 1, 1);
    let l = layout(&word);
    assert_eq!(l.merges, vec![(0, 0, 1, 1)]);
    assert_eq!(cell(&l, 0, 0).unwrap(), ["r0c0", "r0c1", "r1c0", "r1c1"]);
    assert_eq!(cell(&l, 1, 0).unwrap(), [""]);
    assert_eq!(cell(&l, 0, 1), None);
}

#[test]
fn a_row_inserted_inside_a_vertical_merge_continues_it() {
    let mut word = merged(0, 0, 1, 1);
    insert_row(&mut word, 1);
    let l = layout(&word);
    assert_eq!(l.merges, vec![(0, 0, 2, 1)]);
    assert_eq!(cell(&l, 1, 0).unwrap(), [""]);
    assert_eq!(cell(&l, 1, 1), None);
    assert_eq!(cell(&l, 1, 2).unwrap(), [""]);
}

#[test]
fn a_row_inserted_at_a_merge_edge_is_an_ordinary_row() {
    let mut word = merged(0, 0, 1, 1);
    insert_row(&mut word, 2);
    insert_row(&mut word, 0);
    let l = layout(&word);
    assert_eq!(l.merges, vec![(1, 0, 2, 1)]);
    for row in [0, 3] {
        for column in 0..3 {
            assert_eq!(cell(&l, row, column).unwrap(), [""]);
        }
    }
}

#[test]
fn a_column_inserted_inside_a_horizontal_merge_widens_it() {
    let mut word = merged(0, 0, 1, 1);
    insert_column(&mut word, 1);
    insert_column(&mut word, 3);
    let l = layout(&word);
    assert_eq!(l.merges, vec![(0, 0, 1, 2)]);
    assert_eq!(cell(&l, 0, 1), None);
    assert_eq!(cell(&l, 0, 3).unwrap(), [""]);
    assert_eq!(cell(&l, 1, 3).unwrap(), [""]);
}

#[test]
fn deleting_a_merge_top_row_makes_the_next_row_restart_it() {
    let mut word = merged(0, 1, 2, 2);
    delete_row(&mut word, 0);
    let l = layout(&word);
    assert_eq!(l.merges, vec![(0, 1, 1, 2)]);
    assert_eq!(
        cell(&l, 0, 1).unwrap(),
        [""],
        "the merged content went with its row"
    );
    delete_row(&mut word, 0);
    let l = layout(&word);
    assert_eq!(
        l.merges,
        vec![(0, 1, 0, 2)],
        "one row left: a horizontal merge"
    );
}

#[test]
fn deleting_a_merge_left_column_keeps_the_merged_cell() {
    let mut word = merged(0, 0, 1, 1);
    delete_column(&mut word, 0);
    let l = layout(&word);
    assert_eq!(l.merges, vec![(0, 0, 1, 0)]);
    assert_eq!(cell(&l, 0, 0).unwrap(), ["r0c0", "r0c1", "r1c0", "r1c1"]);
    delete_row(&mut word, 1);
    let l = layout(&word);
    assert!(l.merges.is_empty(), "a single cell is no merge");
    assert_eq!(cell(&l, 0, 0).unwrap(), ["r0c0", "r0c1", "r1c0", "r1c1"]);
}

#[test]
fn deleting_inner_lines_shrinks_a_merge() {
    let mut word = merged(0, 0, 2, 2);
    delete_row(&mut word, 1);
    delete_column(&mut word, 1);
    let l = layout(&word);
    assert_eq!(l.merges, vec![(0, 0, 1, 1)]);
    assert_eq!(l.cells.len(), 2);
}
