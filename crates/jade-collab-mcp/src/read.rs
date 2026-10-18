//! What `read_document` reports: a view rendered as JSON, a page at a time.

use std::collections::HashMap;

use jade_collab_core::id::Id;
use jade_docx_collab::intent::revision_kind_name;
use jade_docx_collab::{DocxModel, Paragraph, ParagraphStory, RevisionKind, StoryId};
use jade_pptx_collab::PptxModel;
use jade_xlsx_collab::XlsxModel;
use serde_json::{json, Value};

/// A line's position among a table's rows or columns, by id.
type Lines = HashMap<Id, usize>;

/// The page `start..start + limit` of `total` items, and where the next
/// one starts (`None`: this was the last).
fn page(total: usize, start: usize, limit: usize) -> (std::ops::Range<usize>, Option<usize>) {
    let begin = start.min(total);
    let end = start.saturating_add(limit).min(total);
    (begin..end, (end < total).then_some(end))
}

/// What `read_document` reports for a docx view, without `seq`/`pending`:
/// paragraphs `start..start + limit` — body, table cells, text boxes,
/// footnotes and endnotes, in the order edits index them — and every
/// top-level table.
pub fn read_docx(model: &DocxModel, start: usize, limit: usize) -> Value {
    let paragraphs = model.paragraphs();
    // A text box names its host by body paragraph; an agent needs the index.
    let body_paragraphs: HashMap<usize, usize> = paragraphs
        .iter()
        .enumerate()
        .filter_map(|(ordinal, p)| match p.at {
            ParagraphStory::Body { index } => Some((index, ordinal)),
            _ => None,
        })
        .collect();
    let tables = model.tables();
    // Cell stories name their table, row and column by id; an agent needs
    // the positions. Nested tables are not listed, so their cells have none.
    let positions: HashMap<Id, (usize, Lines, Lines)> = tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            let lines = |ids: &[Id]| ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
            (table.id, (index, lines(&table.rows), lines(&table.columns)))
        })
        .collect();
    // A cell's story is listed at the cell that shows it — after a merge,
    // the merged cell.
    let story = |paragraph: &Paragraph| match (&paragraph.at, model.cell_of(paragraph.story)) {
        (ParagraphStory::Footnote { note_id, paragraph }, _) => {
            json!({ "kind": "footnote", "note": note_id, "paragraph": paragraph })
        }
        (ParagraphStory::Endnote { note_id, paragraph }, _) => {
            json!({ "kind": "endnote", "note": note_id, "paragraph": paragraph })
        }
        (
            ParagraphStory::TextBox {
                host,
                text_box,
                paragraph,
            },
            _,
        ) => json!({
            "kind": "text_box",
            "anchor_paragraph": body_paragraphs.get(host),
            "text_box": text_box,
            "paragraph": paragraph,
        }),
        (_, None) => json!({ "kind": "body" }),
        (_, Some((table, row, column))) => {
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
            .map(|(p, index)| {
                let mut entry = json!({ "index": index, "text": p.text, "story": story(p) });
                if !p.anchors.is_empty() {
                    entry["anchors"] = anchors(model, p);
                }
                let spans = revision_spans(p);
                if !spans.is_empty() {
                    entry["revisions"] = json!(spans);
                }
                entry
            })
            .collect::<Vec<_>>(),
        "tracking": model.tracking(),
        "revisions": model
            .revisions()
            .into_iter()
            .map(|r| json!({
                "id": r.id.to_string(),
                "kind": revision_kind_name(r.revision.kind),
                "author": r.revision.author,
                "date": r.revision.date,
                "file_id": r.revision.file_id,
                "moved": r.revision.moved,
                "text": r.text,
                "paragraphs": r.paragraphs,
            }))
            .collect::<Vec<_>>(),
        "tables": tables
            .iter()
            .enumerate()
            .map(|(index, t)| json!({
                "index": index,
                "rows": t.rows.len(),
                "columns": t.columns.len(),
                "locked": t.locked,
                "merges": t.merges.iter()
                    .map(|(top, left, bottom, right)| json!({ "top": top, "left": left, "bottom": bottom, "right": right }))
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    })
}

/// Where a paragraph's text boxes and note references sit (UTF-8 byte
/// offsets): text boxes numbered in order, notes by their `w:id`.
fn anchors(model: &DocxModel, paragraph: &Paragraph) -> Value {
    let mut text_boxes = 0;
    paragraph
        .anchors
        .iter()
        .map(|anchor| match anchor.story {
            StoryId::Footnote { .. } => json!({
                "offset": anchor.offset,
                "kind": "footnote",
                "note": model.note_id(anchor.story),
            }),
            StoryId::Endnote { .. } => json!({
                "offset": anchor.offset,
                "kind": "endnote",
                "note": model.note_id(anchor.story),
            }),
            _ => {
                text_boxes += 1;
                json!({ "offset": anchor.offset, "kind": "text_box", "text_box": text_boxes - 1 })
            }
        })
        .collect()
}

/// The UTF-8 byte spans of a paragraph's text each tracked change covers:
/// insertions, and deletions as Word attributes them (deleted text is still
/// in `text`), in order.
fn revision_spans(paragraph: &Paragraph) -> Vec<Value> {
    let mut spans: Vec<(usize, RevisionKind, Id, usize)> = Vec::new();
    let mut offset = 0;
    for (c, tracked) in paragraph.text.chars().zip(&paragraph.tracked) {
        let end = offset + c.len_utf8();
        let marks = [
            (RevisionKind::Insertion, tracked.inserted),
            (RevisionKind::Deletion, tracked.deleted),
        ];
        for (kind, revision) in marks {
            let Some(revision) = revision else { continue };
            let open = spans
                .iter_mut()
                .rev()
                .find(|(_, k, r, e)| *k == kind && *r == revision && *e == offset);
            match open {
                Some(span) => span.3 = end,
                None => spans.push((offset, kind, revision, end)),
            }
        }
        offset = end;
    }
    spans.sort_by_key(|(start, kind, _, _)| (*start, *kind));
    spans
        .into_iter()
        .map(|(start, kind, revision, end)| {
            json!({
                "kind": revision_kind_name(kind),
                "revision": revision.to_string(),
                "start": start,
                "end": end,
            })
        })
        .collect()
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

/// What `read_document` reports for a pptx view, without `seq`/`pending`:
/// slides `start..start + limit` (numbered from 1, as edits address them),
/// each with its shapes — the shape numbers edits take, their text and
/// geometry — and its speaker notes.
pub fn read_pptx(model: &PptxModel, start: usize, limit: usize) -> Value {
    let slides = model.slides();
    let (range, next) = page(slides.len(), start, limit);
    json!({
        "format": "pptx",
        "slide_count": slides.len(),
        "start": start,
        "next": next,
        "slides": slides[range.clone()]
            .iter()
            .zip(range)
            .map(|(slide, index)| json!({
                "slide": index + 1,
                "shapes": slide.shapes.iter()
                    .map(|shape| json!({
                        "shape": shape.number,
                        "kind": shape.kind,
                        "text": shape.text,
                        "editable": shape.editable,
                        "deletable": shape.deletable,
                        "geometry": shape.geometry,
                    }))
                    .collect::<Vec<_>>(),
                "notes": slide.notes,
                "notes_editable": slide.notes_editable,
            }))
            .collect::<Vec<_>>(),
    })
}
