//! What `read_document` reports: a view rendered as JSON, a page at a time.

use std::collections::HashMap;

use jade_collab_core::id::Id;
use jade_docx_collab::{DocxModel, StoryId};
use jade_xlsx_collab::XlsxModel;
use serde_json::{json, Value};

/// The page `start..start + limit` of `total` items, and where the next
/// one starts (`None`: this was the last).
fn page(total: usize, start: usize, limit: usize) -> (std::ops::Range<usize>, Option<usize>) {
    let begin = start.min(total);
    let end = start.saturating_add(limit).min(total);
    (begin..end, (end < total).then_some(end))
}

/// What `read_document` reports for a docx view, without `seq`/`pending`:
/// paragraphs `start..start + limit` and every top-level table.
pub fn read_docx(model: &DocxModel, start: usize, limit: usize) -> Value {
    let paragraphs = model.paragraphs();
    let tables = model.tables();
    // Cell stories name their table, row and column by id; an agent needs
    // the positions. Nested tables are not listed, so their cells have none.
    let positions: HashMap<Id, (usize, HashMap<Id, usize>, HashMap<Id, usize>)> = tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            let lines = |ids: &[Id]| ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
            (table.id, (index, lines(&table.rows), lines(&table.columns)))
        })
        .collect();
    let story = |story: StoryId| match story {
        StoryId::Body => json!({ "kind": "body" }),
        StoryId::Cell { table, row, column } => {
            let at = positions.get(&table).and_then(|(index, rows, columns)| {
                Some((*index, *rows.get(&row)?, *columns.get(&column)?))
            });
            match at {
                Some((table, row, column)) => {
                    json!({ "kind": "table_cell", "table": table, "row": row, "column": column })
                }
                None => json!({ "kind": "nested_table_cell" }),
            }
        }
    };
    let (range, next) = page(paragraphs.len(), start, limit);
    json!({
        "format": "docx",
        "paragraph_count": paragraphs.len(),
        "start": start,
        "next": next,
        "paragraphs": paragraphs[range.clone()]
            .iter()
            .zip(range)
            .map(|(p, index)| json!({ "index": index, "text": p.text, "story": story(p.story) }))
            .collect::<Vec<_>>(),
        "tables": tables
            .iter()
            .enumerate()
            .map(|(index, t)| json!({
                "index": index,
                "rows": t.rows.len(),
                "columns": t.columns.len(),
                "locked": t.locked,
            }))
            .collect::<Vec<_>>(),
    })
}

/// What `read_document` reports for an xlsx view, without `seq`/`pending`:
/// cells `start..start + limit` of the non-blank cells of every sheet in
/// order.
pub fn read_xlsx(model: &XlsxModel, start: usize, limit: usize) -> Value {
    let sheets = model.sheets();
    let cells: Vec<Vec<(String, Value)>> = sheets
        .iter()
        .map(|sheet| {
            model
                .filled_cells(sheet.id)
                .into_iter()
                .map(|(reference, value)| {
                    let value = serde_json::to_value(value).expect("cell values serialize");
                    (reference, value)
                })
                .collect()
        })
        .collect();
    let total = cells.iter().map(Vec::len).sum();
    let (range, next) = page(total, start, limit);
    let mut first = 0;
    let sheets: Vec<Value> = sheets
        .iter()
        .zip(cells)
        .map(|(sheet, cells)| {
            let offset = first;
            first += cells.len();
            let listed: serde_json::Map<String, Value> = cells
                .into_iter()
                .enumerate()
                .filter(|(i, _)| range.contains(&(offset + i)))
                .map(|(_, cell)| cell)
                .collect();
            json!({
                "name": sheet.name,
                "rows": sheet.rows.len(),
                "columns": sheet.columns.len(),
                "cells": listed,
            })
        })
        .collect();
    json!({
        "format": "xlsx",
        "cell_count": total,
        "start": start,
        "next": next,
        "sheets": sheets,
    })
}
