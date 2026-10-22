//! What `read_document` reports: a view rendered as JSON, a page at a time.

use std::collections::HashMap;

use jade_collab_core::id::Id;
use jade_docx_collab::intent::revision_kind_name;
use jade_docx_collab::DocxFormatting;
use jade_docx_collab::{DocxModel, Paragraph, ParagraphStory, RevisionKind, StoryId};
use jade_pptx_collab::{PptxFormatting, PptxModel};
use jade_xlsx_collab::{column_label, CellInput, LineAxis, XlsxModel, XlsxReadEntry};
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
/// footnotes and endnotes, in the order edits index them — every live
/// comment and every top-level table.
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
    let selected: std::collections::HashSet<_> =
        paragraphs[range.clone()].iter().map(|p| &p.at).collect();
    let mut formatting: HashMap<ParagraphStory, Vec<Value>> = HashMap::new();
    for span in if range.is_empty() {
        Vec::new()
    } else {
        model.formatting()
    } {
        if selected.contains(&span.paragraph) {
            formatting.entry(span.paragraph).or_default().push(json!({
                "start": span.start, "end": span.end, "change": span.change,
            }));
        }
    }
    json!({
        "format": "docx",
        "formatting_scope": "collaborative_overrides",
        "paragraph_count": paragraphs.len(),
        "start": start,
        "next": next,
        "paragraphs": paragraphs[range.clone()]
            .iter()
            .zip(range)
            .map(|(p, index)| {
                let mut entry = json!({ "index": index, "text": p.text, "story": story(p),
                    "formatting": formatting.get(&p.at).map(Vec::as_slice).unwrap_or(&[]) });
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
        "comments": model
            .comments()
            .iter()
            .map(|c| json!({
                "id": c.id.to_string(),
                "file_id": c.file_id,
                "reply_to": c.parent.map(|p| p.to_string()),
                "text": c.content.text,
                "author": c.content.author,
                "resolved": c.resolved,
            }))
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

/// Renders formatting captured with the server model at the same log position.
pub(crate) fn read_inspected_docx(
    model: &DocxModel,
    formatting: &DocxFormatting,
    start: usize,
    limit: usize,
) -> Value {
    let mut value = read_docx(model, start, limit);
    value["view"] = json!("authoritative");
    for paragraph in value["paragraphs"].as_array_mut().unwrap() {
        let index = paragraph["index"].as_u64().unwrap() as usize;
        let format = &formatting.paragraphs[index];
        paragraph["resolved_formatting"] = json!(format.runs);
        paragraph["paragraph_format"] = json!({
            "style_id": format.style_id, "alignment": format.alignment,
            "indent": format.indent, "spacing": format.spacing,
        });
    }
    value
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

/// A single sparse stream across all sheets: cells, row styles, column
/// styles and merge geometry. Only the selected items are rendered to JSON.
pub fn read_xlsx(model: &XlsxModel, start: usize, limit: usize) -> Value {
    let sheets = model.sheets();
    let items: Vec<_> = sheets.iter().map(|s| model.read_entries(s.id)).collect();
    let total = items.iter().map(Vec::len).sum();
    let cell_count: usize = sheets
        .iter()
        .zip(&items)
        .map(|(s, items)| {
            items
                .iter()
                .filter(|item| match item {
                    XlsxReadEntry::Cell { row, column } => model
                        .value(s.id, *row, *column)
                        .is_some_and(|v| *v != CellInput::Blank),
                    _ => false,
                })
                .count()
        })
        .sum();
    let (range, next) = page(total, start, limit);
    let mut offset = 0;
    let sheets: Vec<_> = sheets
        .iter()
        .zip(items)
        .map(|(sheet, items)| {
            let selected = items
                .iter()
                .enumerate()
                .filter(|(i, _)| range.contains(&(offset + i)));
            let rows: Lines = sheet
                .rows
                .iter()
                .enumerate()
                .map(|(i, id)| (*id, i))
                .collect();
            let columns: Lines = sheet
                .columns
                .iter()
                .enumerate()
                .map(|(i, id)| (*id, i))
                .collect();
            let a1 = |row: Id, column: Id| {
                format!(
                    "{}{}",
                    column_label(columns[&column] as u32),
                    rows[&row] + 1
                )
            };
            let (mut cells, mut formats, mut row_formats, mut column_formats) = (
                serde_json::Map::new(),
                serde_json::Map::new(),
                serde_json::Map::new(),
                serde_json::Map::new(),
            );
            let mut merges = Vec::new();
            let mut notes = serde_json::Map::new();
            for (_, item) in selected {
                match *item {
                    XlsxReadEntry::Cell { row, column } => {
                        let reference = a1(row, column);
                        if let Some(value) = model
                            .value(sheet.id, row, column)
                            .filter(|v| **v != CellInput::Blank)
                        {
                            cells.insert(
                                reference.clone(),
                                serde_json::to_value(value).expect("cell value serializes"),
                            );
                        }
                        formats.insert(
                            reference,
                            serde_json::to_value(model.format(sheet.id, row, column))
                                .expect("format serializes"),
                        );
                        if let Some(note) = model.note(sheet.id, row, column) {
                            notes.insert(a1(row, column), Value::String(note.to_string()));
                        }
                    }
                    XlsxReadEntry::RowFormat { row } => {
                        row_formats.insert(
                            (rows[&row] + 1).to_string(),
                            serde_json::to_value(model.line_format(sheet.id, LineAxis::Rows, row))
                                .expect("format serializes"),
                        );
                    }
                    XlsxReadEntry::ColumnFormat { column } => {
                        column_formats.insert(
                            column_label(columns[&column] as u32),
                            serde_json::to_value(model.line_format(
                                sheet.id,
                                LineAxis::Columns,
                                column,
                            ))
                            .expect("format serializes"),
                        );
                    }
                    XlsxReadEntry::Merge { range } => {
                        merges.push(format!(
                            "{}:{}",
                            a1(range.top, range.left),
                            a1(range.bottom, range.right)
                        ));
                    }
                }
            }
            offset += items.len();
            json!({
                "name": sheet.name, "rows": sheet.rows.len(), "columns": sheet.columns.len(),
                "cells": cells, "formats": formats, "row_formats": row_formats,
                "column_formats": column_formats, "merges": merges, "notes": notes,
                "default_format": model.default_format(sheet.id),
            })
        })
        .collect();
    json!({ "format": "xlsx", "cell_count": cell_count, "entry_count": total,
        "start": start, "next": next, "sheets": sheets })
}

/// What `read_document` reports for a pptx view, without `seq`/`pending`:
/// slides `start..start + limit` (numbered from 1, as edits address them),
/// each with its shapes — the shape numbers edits take, their text and
/// geometry — and its speaker notes.
pub fn read_pptx(model: &PptxModel, start: usize, limit: usize) -> Value {
    let slides = model.slides();
    let (range, next) = page(slides.len(), start, limit);
    let mut formatting: HashMap<(Id, Id), Vec<Value>> = HashMap::new();
    if !range.is_empty() {
        for run in model
            .formatting()
            .into_iter()
            .filter(|run| range.contains(&run.slide))
        {
            formatting.entry((run.shape.slide, run.shape.shape)).or_default().push(json!({
                "paragraph":run.paragraph, "start":run.start, "end":run.end, "format":run.format,
            }));
        }
    }
    json!({
        "format": "pptx",
        "formatting_scope": "collaborative_overrides",
        "slide_count": slides.len(),
        "start": start,
        "next": next,
        "slides": slides[range.clone()]
            .iter()
            .zip(range)
            .map(|(slide, index)| json!({
                "slide": index + 1,
                "hidden": slide.hidden,
                "shapes": slide.shapes.iter()
                    .map(|shape| json!({
                        "shape": shape.number,
                        "kind": shape.kind,
                        "text": shape.text,
                        "editable": shape.editable,
                        "deletable": shape.deletable,
                        "aspect_locked": shape.aspect_locked,
                        "group": shape.group.and_then(|group| slide.shapes.iter().find(|s| s.id == group).map(|s| s.number)),
                        "geometry": shape.geometry,
                        "formatting": formatting.get(&(slide.id, shape.id)).map(Vec::as_slice).unwrap_or(&[]),
                    }))
                    .collect::<Vec<_>>(),
                "notes": slide.notes,
                "notes_editable": slide.notes_editable,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Native shape text properties captured together with this committed model.
pub(crate) fn read_inspected_pptx(
    model: &PptxModel,
    formatting: &PptxFormatting,
    start: usize,
    limit: usize,
) -> Value {
    let mut value = read_pptx(model, start, limit);
    value["view"] = json!("authoritative");
    for slide in value["slides"].as_array_mut().unwrap() {
        let index = slide["slide"].as_u64().unwrap() as usize - 1;
        for (shape, format) in slide["shapes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(&formatting.slides[index].shapes)
        {
            if let Some(paragraphs) = &format.paragraphs {
                let text = shape["text"].as_str().unwrap();
                shape["paragraphs"] = json!(text.split('\n').zip(paragraphs).enumerate().map(|(index,(text,p))| json!({
                    "index":index, "text":text, "resolved_formatting":p.runs, "paragraph_format":p.style,
                })).collect::<Vec<_>>());
            }
        }
    }
    value
}
