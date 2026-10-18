#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use jade_collab_core::grid::GridOp;
use jade_collab_core::id::Id;
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{
    Anchor, CommentContent, DocxEffect, DocxModel, DocxOp, FormatChange, JadeDocx, NoteKind,
    ParagraphAlignment, StoryId,
};
use jade_docx_document_core::headless_api::execute_headless_queries_on_document;
use jade_docx_document_core::word_document::ParagraphAnchorKind;
use jade_docx_headless_api::{
    HeadlessCommentSnapshot, HeadlessParagraphSnapshot, HeadlessQuery, HeadlessQueryRequest,
    HeadlessQueryResult, HeadlessRunStyle,
};

pub fn jade_core() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core")
}

pub fn fixture(name: &str) -> Vec<u8> {
    let root = jade_core();
    // "jade-core/x.docx" is at the submodule root; bare names are fixtures.
    let path = if let Some(rest) = name.strip_prefix("jade-core/") {
        root.join(rest)
    } else {
        root.join("public/fixtures").join(name)
    };
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Loads a fixture into jade and builds the model from it.
pub fn open(name: &str) -> (DocxModel, JadeDocx) {
    let jade = JadeDocx::load(&fixture(name)).expect("jade loads the fixture");
    let model = DocxModel::from_word_document(jade.word()).expect("model builds");
    (model, jade)
}

/// The model and jade describe the same paragraphs: every paragraph surface
/// jade lists (body, table cells, text boxes, footnotes, endnotes; not
/// equations or note separators) is one model paragraph naming its story
/// position, with the same text and the same text boxes and note references
/// at the same offsets.
///
/// After a split jade lists the new paragraph before the footnotes the first
/// half references, until the file is read again (`assert_same_order`); the
/// model lists them as jade does when it reads the file. Paragraphs are
/// therefore matched by story position, which is how the materializer
/// addresses them.
pub fn assert_stories_agree(model: &DocxModel, jade: &JadeDocx, context: &str) {
    let paragraphs = model.paragraphs();
    let mut matched: Vec<Option<usize>> = paragraphs
        .iter()
        .map(|p| jade.word().paragraph_story_surface(&p.at))
        .collect();
    matched.sort();
    let surfaces: Vec<Option<usize>> = jade.paragraph_surfaces().into_iter().map(Some).collect();
    let texts: Vec<&str> = paragraphs.iter().map(|p| p.text.as_str()).collect();
    assert_eq!(
        matched,
        surfaces,
        "paragraphs disagree {context}: model {texts:?}, jade {:?}",
        jade.paragraph_texts()
    );
    for paragraph in &paragraphs {
        let surface = surface(jade, paragraph);
        assert_eq!(
            jade.word().paragraph(surface).map(|p| p
                .runs
                .iter()
                .map(|r| r.text.as_str())
                .collect::<String>()),
            Some(paragraph.text.clone()),
            "text of {:?} {context}",
            paragraph.at
        );
        let ours: Vec<(usize, String)> = paragraph
            .anchors
            .iter()
            .map(|a| (a.offset, describe(model, a.story)))
            .collect();
        let theirs: Vec<(usize, String)> = jade
            .word()
            .paragraph_anchors(surface)
            .into_iter()
            .map(|a| {
                let kind = match a.kind {
                    ParagraphAnchorKind::TextBox { .. } => "text box".to_string(),
                    ParagraphAnchorKind::Footnote { note_id } => format!("footnote {note_id}"),
                    ParagraphAnchorKind::Endnote { note_id } => format!("endnote {note_id}"),
                };
                (a.offset, kind)
            })
            .collect();
        assert_eq!(ours, theirs, "anchors of {:?} {context}", paragraph.text);
    }
}

/// jade's paragraph surface for a model paragraph.
pub fn surface(jade: &JadeDocx, paragraph: &jade_docx_collab::Paragraph) -> usize {
    jade.word()
        .paragraph_story_surface(&paragraph.at)
        .unwrap_or_else(|| panic!("jade has no paragraph {:?}", paragraph.at))
}

/// The model lists jade's paragraphs in jade's order — as jade lists them
/// when it reads a file.
pub fn assert_same_order(model: &DocxModel, jade: &JadeDocx, context: &str) {
    assert_eq!(
        model.paragraph_texts(),
        jade.paragraph_texts(),
        "paragraph order {context}"
    );
    assert_stories_agree(model, jade, context);
}

/// How jade names the story an anchor places.
fn describe(model: &DocxModel, story: StoryId) -> String {
    match story {
        StoryId::Footnote { .. } => format!("footnote {}", model.note_id(story).unwrap()),
        StoryId::Endnote { .. } => format!("endnote {}", model.note_id(story).unwrap()),
        StoryId::TextBox { .. } => "text box".to_string(),
        other => panic!("{other:?} is not placed by an anchor"),
    }
}

/// Applies `op` as the server does — admit (sequenced), apply, materialize —
/// and checks the model and jade still agree.
pub fn commit(model: &mut DocxModel, jade: &mut JadeDocx, op: DocxOp) -> Vec<DocxEffect> {
    let ctx = ApplyContext {
        origin: jade_collab_core::id::ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    };
    if DocxModel::lane(&op) == Lane::Sequenced {
        jade.admit(model, &op)
            .unwrap_or_else(|e| panic!("jade refused {op:?}: {e}"));
    }
    let effect = model
        .apply(&op, &ctx)
        .unwrap_or_else(|e| panic!("{op:?}: {e}"));
    jade.materialize(model, effect.clone(), &ctx)
        .unwrap_or_else(|e| panic!("{op:?}: {e}"));
    derive(model, jade, &op);
    assert_stories_agree(model, jade, &format!("after {op:?}"));
    assert_comments_agree(model, jade, &format!("after {op:?}"));
    effect
}

/// Commits what the server derives after `cause` (the numbers jade gave
/// what it made), as the sequencer does right after it.
pub fn derive(model: &mut DocxModel, jade: &mut JadeDocx, cause: &DocxOp) {
    let ctx = ApplyContext {
        origin: jade_collab_core::id::ReplicaId::ORIGIN,
        base_seq: 0,
        seq: Some(1),
    };
    for op in jade.derived(model, cause) {
        let effect = model
            .apply(&op, &ctx)
            .unwrap_or_else(|e| panic!("derived {op:?}: {e}"));
        jade.materialize(model, effect, &ctx)
            .unwrap_or_else(|e| panic!("derived {op:?}: {e}"));
    }
}

/// Turns a random `choice` into an operation on `view`. `ids(n)` reserves `n`
/// consecutive identifiers. Returns `None` when the choice does not fit the
/// document (e.g. no tables to edit).
pub fn generate(view: &DocxModel, choice: u64, ids: &mut dyn FnMut(u64) -> Id) -> Option<DocxOp> {
    let paragraphs = view.paragraphs();
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let words = ["x", "日本", "é", "Lorem ", "  ", "ab"];
    match choice % 36 {
        // Typing at a random caret, through the index-addressed intent API.
        0..=4 => {
            let p = pick(paragraphs.len(), 8);
            let text = &paragraphs[p].text;
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            let offset = boundaries[pick(boundaries.len(), 16)];
            let op = IntentOp::InsertText {
                paragraph: p,
                offset,
                text: words[pick(words.len(), 24)].to_string(),
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        // Deleting a few characters somewhere.
        5..=6 => {
            let p = pick(paragraphs.len(), 8);
            let atoms = &paragraphs[p].chars;
            if atoms.is_empty() {
                return None;
            }
            // Up to four characters, `stride` apart: a stride over 1 deletes
            // several separate ranges in one operation.
            let start = pick(atoms.len(), 16);
            let len = 1 + pick(4, 24);
            let stride = 1 + pick(3, 28);
            let ids: Vec<Id> = atoms[start..]
                .iter()
                .step_by(stride)
                .take(len)
                .copied()
                .collect();
            Some(DocxOp::DeleteText {
                story: paragraphs[p].story,
                ids,
            })
        }
        7..=8 => {
            let p = pick(paragraphs.len(), 8);
            let text = &paragraphs[p].text;
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            let op = IntentOp::InsertParagraphBreak {
                paragraph: p,
                offset: boundaries[pick(boundaries.len(), 16)],
                count: 1,
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        9 => {
            let marked: Vec<_> = paragraphs.iter().filter(|p| p.mark.is_some()).collect();
            if marked.is_empty() {
                return None;
            }
            let p = marked[pick(marked.len(), 8)];
            Some(DocxOp::JoinParagraph {
                story: p.story,
                mark: p.mark.unwrap(),
            })
        }
        10 => {
            let marks: Vec<Id> = paragraphs
                .iter()
                .filter(|p| p.story == StoryId::Body)
                .filter_map(|p| p.mark)
                .collect();
            let anchor = if marks.is_empty() || pick(4, 8) == 0 {
                Anchor::Start
            } else {
                Anchor::After(marks[pick(marks.len(), 12)])
            };
            let rows = 1 + pick(2, 20) as u64;
            let columns = 1 + pick(3, 24) as u64;
            let first = ids(1 + rows + columns);
            Some(DocxOp::InsertTable {
                anchor,
                id: first,
                rows: (1..=rows).map(|k| first.offset(k)).collect(),
                columns: (1..=columns).map(|k| first.offset(rows + k)).collect(),
            })
        }
        11..=14 => {
            let tables = view.tables();
            let editable: Vec<_> = tables.iter().filter(|t| !t.locked).collect();
            if editable.is_empty() {
                return None;
            }
            let table = editable[pick(editable.len(), 8)];
            let row = table.rows[pick(table.rows.len(), 12)];
            let column = table.columns[pick(table.columns.len(), 16)];
            let op = match choice % 36 {
                11 => GridOp::InsertRow {
                    id: ids(1),
                    after: (pick(3, 20) != 0).then_some(row),
                },
                12 => GridOp::DeleteRow { id: row },
                13 => GridOp::InsertColumn {
                    id: ids(1),
                    after: (pick(3, 20) != 0).then_some(column),
                },
                _ => GridOp::DeleteColumn { id: column },
            };
            Some(DocxOp::Table {
                table: table.id,
                op,
            })
        }
        15 => {
            let tables = view.tables();
            if tables.is_empty() || pick(3, 8) != 0 {
                return None;
            }
            Some(DocxOp::DeleteTable {
                table: tables[pick(tables.len(), 12)].id,
            })
        }
        // Character formatting of a random range, through the intent API.
        16..=17 => {
            let ((sp, so), (ep, eo)) = random_range(&paragraphs, choice)?;
            let changes = character_changes();
            let op = IntentOp::Format {
                start_paragraph: sp,
                start_offset: so,
                end_paragraph: ep,
                end_offset: eo,
                change: changes[pick(changes.len(), 40)].clone(),
            };
            let ops = resolve(view, &op, ids).ok()?;
            ops.get(pick(ops.len(), 44)).cloned()
        }
        // Paragraph formatting of a few paragraphs.
        18 => {
            let start = pick(paragraphs.len(), 8);
            let end = (start + pick(3, 16)).min(paragraphs.len() - 1);
            let changes = paragraph_changes();
            let op = IntentOp::FormatParagraphs {
                start_paragraph: start,
                end_paragraph: end,
                change: changes[pick(changes.len(), 20)].clone(),
            };
            let ops = resolve(view, &op, ids).ok()?;
            ops.get(pick(ops.len(), 44)).cloned()
        }
        // A comment on a random range of one story.
        19 => {
            let ((sp, so), (ep, eo)) = random_range(&paragraphs, choice)?;
            let op = IntentOp::AddComment {
                start_paragraph: sp,
                start_offset: Some(so),
                end_paragraph: ep,
                end_offset: Some(eo),
                text: ["Why?", "日本語のコメント", "ok"][pick(3, 40)].to_string(),
                author: [None, Some("Ann".to_string())][pick(2, 42)].clone(),
                initials: None,
                date: None,
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        // Answering, resolving or editing a live comment.
        20 => {
            let comments = view.comments();
            if comments.is_empty() {
                return None;
            }
            let comment = comments[pick(comments.len(), 8)].id;
            Some(match pick(3, 16) {
                0 => DocxOp::ReplyToComment {
                    comment,
                    id: ids(1),
                    content: CommentContent::new("reply"),
                },
                1 => DocxOp::UpdateComment {
                    comment,
                    text: ["edited", "直した", "a  b"][pick(3, 24)].to_string(),
                },
                _ => DocxOp::ResolveComment {
                    comment,
                    resolved: pick(3, 20) != 0,
                },
            })
        }
        // Merging up to 2 × 2 cells of an editable table.
        22 | 23 => {
            let tables = view.tables();
            let editable: Vec<(usize, _)> = tables
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.locked)
                .collect();
            let (index, table) = editable.get(pick(editable.len(), 8))?;
            let top = pick(table.rows.len(), 12);
            let left = pick(table.columns.len(), 16);
            let op = IntentOp::MergeTableCells {
                table: *index,
                top_row: top,
                left_column: left,
                bottom_row: (top + pick(2, 20)).min(table.rows.len() - 1),
                right_column: (left + 1).min(table.columns.len() - 1),
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        24 => {
            let tables = view.tables();
            let merged: Vec<(usize, (usize, usize, usize, usize))> = tables
                .iter()
                .enumerate()
                .flat_map(|(i, t)| t.merges.iter().map(move |m| (i, *m)))
                .collect();
            let (table, (row, column, _, _)) = merged.get(pick(merged.len(), 8))?;
            let op = IntentOp::UnmergeTableCells {
                table: *table,
                row: *row,
                column: *column,
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        // Edits right next to a text box or a note reference.
        25..=29 => around_anchor(view, &paragraphs, choice, ids),
        // A new footnote or endnote right after a text box or note
        // reference (the anchor's atom its parent).
        32 if pick(2, 30) == 0 => {
            let anchored: Vec<(StoryId, Id)> = paragraphs
                .iter()
                .flat_map(|q| q.anchors.iter().map(move |a| (q.story, a.id)))
                .collect();
            let &(story, parent) = anchored.get(pick(anchored.len(), 8))?;
            let id = ids(3);
            Some(DocxOp::InsertNote {
                kind: if pick(2, 12) == 0 {
                    NoteKind::Endnote
                } else {
                    NoteKind::Footnote
                },
                story,
                parent: Some(parent),
                id,
                text: "n".into(),
            })
        }
        // A new footnote or endnote at a random caret, or where a text box
        // or note reference already sits.
        30..=32 => {
            let (p, offset) = if choice % 36 == 32 {
                let anchored: Vec<(usize, usize)> = paragraphs
                    .iter()
                    .enumerate()
                    .flat_map(|(p, q)| q.anchors.iter().map(move |a| (p, a.offset)))
                    .collect();
                *anchored.get(pick(anchored.len(), 8))?
            } else {
                let p = pick(paragraphs.len(), 8);
                let text = &paragraphs[p].text;
                let boundaries: Vec<usize> = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .chain([text.len()])
                    .collect();
                (p, boundaries[pick(boundaries.len(), 16)])
            };
            let text = ["", "note", "注"][pick(3, 24)].to_string();
            let op = if pick(3, 28) == 0 {
                IntentOp::InsertEndnote {
                    paragraph: p,
                    offset,
                    text,
                }
            } else {
                IntentOp::InsertFootnote {
                    paragraph: p,
                    offset,
                    text,
                }
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        // Deleting a footnote or endnote some paragraph references.
        33 => {
            let notes: Vec<StoryId> = paragraphs
                .iter()
                .flat_map(|p| p.anchors.iter().map(|a| a.story))
                .filter(|s| !matches!(s, StoryId::TextBox { .. }))
                .collect();
            Some(match *notes.get(pick(notes.len(), 8))? {
                StoryId::Footnote { id } => DocxOp::DeleteNote {
                    kind: NoteKind::Footnote,
                    note: id,
                },
                StoryId::Endnote { id } => DocxOp::DeleteNote {
                    kind: NoteKind::Endnote,
                    note: id,
                },
                _ => unreachable!("filtered"),
            })
        }
        // A new text box at a random caret.
        34 => {
            let p = pick(paragraphs.len(), 8);
            let text = &paragraphs[p].text;
            let boundaries: Vec<usize> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            let op = IntentOp::InsertTextBox {
                paragraph: p,
                offset: boundaries[pick(boundaries.len(), 16)],
                text: ["", "box", "箱"][pick(3, 24)].to_string(),
                width_px: None,
                height_px: None,
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        // Deleting a text box some paragraph places.
        35 => {
            let boxes: Vec<(usize, usize)> = paragraphs
                .iter()
                .enumerate()
                .flat_map(|(p, q)| {
                    let count = q
                        .anchors
                        .iter()
                        .filter(|a| matches!(a.story, StoryId::TextBox { .. }))
                        .count();
                    (0..count).map(move |k| (p, k))
                })
                .collect();
            let &(paragraph, text_box) = boxes.get(pick(boxes.len(), 8))?;
            let op = IntentOp::DeleteTextBox {
                paragraph,
                text_box,
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        _ => {
            let comments = view.comments();
            if comments.is_empty() {
                return None;
            }
            Some(DocxOp::DeleteComment {
                comment: comments[pick(comments.len(), 8)].id,
            })
        }
    }
}

/// An edit at a random anchor (a text box or a note reference): typing or
/// splitting at its offset, deleting the characters on both sides of it,
/// emptying its paragraph up to it, or joining its paragraph with the next
/// or the previous one.
fn around_anchor(
    view: &DocxModel,
    paragraphs: &[jade_docx_collab::Paragraph],
    choice: u64,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Option<DocxOp> {
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let anchored: Vec<(usize, usize)> = paragraphs
        .iter()
        .enumerate()
        .flat_map(|(p, paragraph)| (0..paragraph.anchors.len()).map(move |a| (p, a)))
        .collect();
    let &(p, a) = anchored.get(pick(anchored.len(), 8))?;
    let paragraph = &paragraphs[p];
    let anchor = &paragraph.anchors[a];
    // Characters before the anchor.
    let before = paragraph.text[..anchor.offset].chars().count();
    match choice % 36 {
        25 => {
            let op = IntentOp::InsertText {
                paragraph: p,
                offset: anchor.offset,
                text: ["a", "日本"][pick(2, 16)].to_string(),
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        26 => {
            let op = IntentOp::InsertParagraphBreak {
                paragraph: p,
                offset: anchor.offset,
                count: 1,
            };
            resolve(view, &op, ids).ok()?.into_iter().next()
        }
        27 => {
            let chars: Vec<Id> = paragraph.chars[before.saturating_sub(1)..]
                .iter()
                .take(2)
                .copied()
                .collect();
            (!chars.is_empty()).then_some(DocxOp::DeleteText {
                story: paragraph.story,
                ids: chars,
            })
        }
        28 => {
            let chars = paragraph.chars[..before].to_vec();
            (!chars.is_empty()).then_some(DocxOp::DeleteText {
                story: paragraph.story,
                ids: chars,
            })
        }
        _ => {
            // This paragraph's mark, or the previous one of its story.
            let mark = if pick(2, 16) == 0 {
                paragraph.mark
            } else {
                paragraphs[..p]
                    .iter()
                    .rev()
                    .find(|q| q.story == paragraph.story)
                    .and_then(|q| q.mark)
            }?;
            Some(DocxOp::JoinParagraph {
                story: paragraph.story,
                mark,
            })
        }
    }
}

pub fn character_changes() -> Vec<FormatChange> {
    vec![
        FormatChange::Bold { value: true },
        FormatChange::Bold { value: false },
        FormatChange::Italic { value: true },
        FormatChange::Italic { value: false },
        FormatChange::Underline {
            value: true,
            style: None,
        },
        FormatChange::Underline {
            value: true,
            style: Some("double".into()),
        },
        FormatChange::Underline {
            value: false,
            style: None,
        },
        FormatChange::Strike {
            value: true,
            double: false,
        },
        FormatChange::Strike {
            value: false,
            double: false,
        },
        FormatChange::FontSize { half_points: 21 },
        FormatChange::FontSize { half_points: 40 },
        FormatChange::FontFamily {
            family: "Arial".into(),
        },
        FormatChange::FontColor {
            color: "#C00000".into(),
        },
        FormatChange::Highlight {
            color: "yellow".into(),
        },
        FormatChange::ClearFormatting,
    ]
}

pub fn paragraph_changes() -> Vec<FormatChange> {
    vec![
        FormatChange::Alignment {
            alignment: ParagraphAlignment::Center,
        },
        FormatChange::Alignment {
            alignment: ParagraphAlignment::Right,
        },
        FormatChange::Alignment {
            alignment: ParagraphAlignment::Justify,
        },
        FormatChange::ParagraphStyle {
            style_id: Some("Heading1".into()),
        },
        FormatChange::ParagraphStyle { style_id: None },
    ]
}

/// Two carets, in order, within one story (the second may be in a later
/// paragraph); `None` when the choice lands on an empty range.
fn random_range(
    paragraphs: &[jade_docx_collab::Paragraph],
    choice: u64,
) -> Option<((usize, usize), (usize, usize))> {
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let p = pick(paragraphs.len(), 8);
    let same_story: Vec<usize> = (p..paragraphs.len())
        .filter(|q| paragraphs[*q].story == paragraphs[p].story)
        .take(3)
        .collect();
    let q = same_story[pick(same_story.len(), 12)];
    let boundaries =
        |t: &str| -> Vec<usize> { t.char_indices().map(|(i, _)| i).chain([t.len()]).collect() };
    let (bp, bq) = (
        boundaries(&paragraphs[p].text),
        boundaries(&paragraphs[q].text),
    );
    let so = bp[pick(bp.len(), 16)];
    let eo = bq[pick(bq.len(), 24)];
    if p == q && so >= eo {
        return None;
    }
    Some(((p, so), (q, eo)))
}

/// The style jade reports for each visible character of the model (its run
/// in jade's paragraph query), and its `w:highlight`, which the query leaves
/// out and is read from the run itself.
#[derive(Clone, Debug, PartialEq)]
pub struct CharStyle {
    pub style: HeadlessRunStyle,
    pub highlight: Option<String>,
}

fn query(jade: &JadeDocx, query: HeadlessQuery) -> HeadlessQueryResult {
    execute_headless_queries_on_document(
        jade.word(),
        &HeadlessQueryRequest {
            queries: vec![query],
        },
    )
    .expect("query runs")
    .results
    .remove(0)
}

pub fn paragraph_snapshot(jade: &JadeDocx, surface: usize) -> HeadlessParagraphSnapshot {
    match query(jade, HeadlessQuery::Paragraph { paragraph: surface }) {
        HeadlessQueryResult::Paragraph {
            value: Some(value), ..
        } => value,
        other => panic!("paragraph {surface}: {other:?}"),
    }
}

/// Every visible character of `model`, with what jade says about it.
pub fn char_styles(model: &DocxModel, jade: &JadeDocx) -> BTreeMap<Id, CharStyle> {
    let mut out = BTreeMap::new();
    for paragraph in model.paragraphs().iter() {
        let surface = surface(jade, paragraph);
        let snapshot = paragraph_snapshot(jade, surface);
        let runs = &jade.word().paragraph(surface).expect("surface exists").runs;
        let mut offset = 0;
        for (c, id) in paragraph.text.chars().zip(&paragraph.chars) {
            // jade's commands take rendered offsets (an inline text box counts
            // as one), but its paragraph query reports offsets in the runs'
            // text — the model's text.
            let run = snapshot
                .runs
                .iter()
                .find(|r| r.start_offset <= offset && offset < r.end_offset)
                .unwrap_or_else(|| panic!("no run at {offset} of {snapshot:?}"));
            assert_eq!(
                snapshot.text.get(offset..offset + c.len_utf8()),
                Some(c.encode_utf8(&mut [0; 4]) as &str),
                "offset {offset} is not {c:?} in {:?}",
                snapshot.text
            );
            let mut start = 0;
            let highlight = runs
                .iter()
                .find(|r| {
                    let found = start <= offset && offset < start + r.text.len();
                    start += r.text.len();
                    found
                })
                .and_then(|r| r.properties.as_ref())
                .and_then(|p| p.highlight.as_deref())
                .map(str::to_string);
            out.insert(
                *id,
                CharStyle {
                    style: run.style.clone(),
                    highlight,
                },
            );
            offset += c.len_utf8();
        }
    }
    out
}

/// (alignment, style id) of each model paragraph, as jade reports them.
pub fn paragraph_formats(
    model: &DocxModel,
    jade: &JadeDocx,
) -> Vec<(Option<String>, Option<String>)> {
    model
        .paragraphs()
        .iter()
        .map(|paragraph| {
            let snapshot = paragraph_snapshot(jade, surface(jade, paragraph));
            (snapshot.alignment, snapshot.style_id)
        })
        .collect()
}

pub fn jade_comments(jade: &JadeDocx) -> Vec<HeadlessCommentSnapshot> {
    match query(
        jade,
        HeadlessQuery::Comments {
            start_comment: 0,
            max_comments: usize::MAX,
            author: None,
            resolved: None,
        },
    ) {
        HeadlessQueryResult::Comments { comments, .. } => comments,
        other => panic!("{other:?}"),
    }
}

/// The model's live comments are exactly jade's: same text and author, same
/// thread structure, same resolved state.
pub fn assert_comments_agree(model: &DocxModel, jade: &JadeDocx, context: &str) {
    let ours = model.comments();
    let theirs = jade_comments(jade);
    assert_eq!(ours.len(), theirs.len(), "comment count {context}");
    let jade_id = |id: Id| {
        jade.comment_id(model, id)
            .unwrap_or_else(|| panic!("comment {id} has no jade id {context}"))
    };
    let mut seen = std::collections::BTreeSet::new();
    for comment in &ours {
        let key = jade_id(comment.id);
        assert!(
            seen.insert(key.clone()),
            "two comments map to {key} {context}"
        );
        let found = theirs
            .iter()
            .find(|c| c.id == key)
            .unwrap_or_else(|| panic!("jade has no comment {key} {context}"));
        assert_eq!(found.text, comment.content.text, "comment text {context}");
        assert_eq!(
            found.author, comment.content.author,
            "comment author {context}"
        );
        assert_eq!(
            found.resolved.unwrap_or(false),
            comment.resolved,
            "resolved state of {key} {context}"
        );
        assert_eq!(
            found.reply_to_comment_id,
            comment.parent.map(jade_id),
            "thread of {key} {context}"
        );
    }
}
