#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use jade_collab_core::grid::GridOp;
use jade_collab_core::id::Id;
use jade_collab_core::id::ReplicaId;
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_docx_collab::intent::{resolve, resolve_with, Context, IntentOp};
use jade_docx_collab::{
    Anchor, CommentContent, DocxEffect, DocxModel, DocxOp, FormatChange, JadeDocx, MarkKey,
    NoteKind, ParagraphAlignment, RevisionKind, StoryId,
};
use jade_docx_document_core::document::RevisionKind as JadeRevisionKind;
use jade_docx_document_core::headless_api::execute_headless_queries_on_document;
use jade_docx_document_core::word_document::{ParagraphAnchorKind, WordDocument};
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

/// (top, left, bottom, right) indices of a merged region.
pub type Region = (usize, usize, usize, usize);

/// What a table shows, cell by cell: the paragraph texts of each `w:tc`
/// keyed by (row, first grid column), and its merged regions.
#[derive(Debug, PartialEq)]
pub struct TableLayout {
    pub rows: usize,
    pub columns: usize,
    pub cells: BTreeMap<(usize, usize), Vec<String>>,
    pub merges: Vec<Region>,
}

/// jade's top-level body tables as [`TableLayout`]s (`w:gridSpan` across,
/// `w:vMerge` restart/continue down); `None` for a ragged one.
pub fn jade_tables(jade: &JadeDocx) -> Vec<Option<TableLayout>> {
    use jade_docx_document_core::document::BodyElement;
    let Some(body) = jade.word().document.as_ref().and_then(|d| d.body.as_ref()) else {
        return Vec::new();
    };
    body.elements
        .iter()
        .filter_map(|e| match e {
            BodyElement::Table(t) => Some(t),
            _ => None,
        })
        .map(|table| {
            let mut cells = BTreeMap::new();
            let mut merges: Vec<Region> = Vec::new();
            let mut width = None;
            for (r, row) in table.rows.iter().enumerate() {
                let mut c = 0;
                for cell in &row.cells {
                    let span = cell.span.unwrap_or(1).max(1) as usize;
                    let right = c + span - 1;
                    let texts = cell
                        .blocks
                        .iter()
                        .filter_map(|b| match b {
                            BodyElement::Paragraph(p) => {
                                Some(p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
                            }
                            _ => None,
                        })
                        .collect();
                    cells.insert((r, c), texts);
                    match cell.vertical_merge.as_deref() {
                        Some("continue") => {
                            let above = merges
                                .iter_mut()
                                .find(|m| m.1 == c && m.3 == right && m.2 + 1 == r)?;
                            above.2 = r;
                        }
                        Some(_) => merges.push((r, c, r, right)),
                        None if right > c => merges.push((r, c, r, right)),
                        None => {}
                    }
                    c = right + 1;
                }
                if *width.get_or_insert(c) != c {
                    return None;
                }
            }
            merges.retain(|(t, l, b, r)| (b - t + 1) * (r - l + 1) >= 2);
            merges.sort();
            Some(TableLayout {
                rows: table.rows.len(),
                columns: width.unwrap_or(0),
                cells,
                merges,
            })
        })
        .collect()
}

/// The model's top-level body tables as [`TableLayout`]s: the paragraphs of
/// the stories each alive cell position shows; `None` for a locked table.
pub fn model_tables(model: &DocxModel) -> Vec<Option<TableLayout>> {
    let paragraphs = model.paragraphs();
    model
        .tables()
        .into_iter()
        .map(|table| {
            if table.locked {
                return None;
            }
            let mut cells: BTreeMap<(usize, usize), Vec<String>> = BTreeMap::new();
            for p in &paragraphs {
                let Some((t, row, column)) = model.cell_of(p.story) else {
                    continue;
                };
                if t != table.id {
                    continue;
                }
                let r = table.rows.iter().position(|x| *x == row)?;
                let c = table.columns.iter().position(|x| *x == column)?;
                cells.entry((r, c)).or_default().push(p.text.clone());
            }
            let mut merges = table.merges.clone();
            merges.sort();
            Some(TableLayout {
                rows: table.rows.len(),
                columns: table.columns.len(),
                cells,
                merges,
            })
        })
        .collect()
}

/// Every unlocked top-level table shows, cell by cell, what jade's does:
/// the same cells at the same grid positions with the same paragraphs, and
/// the same merged regions.
pub fn assert_tables_agree(model: &DocxModel, jade: &JadeDocx, context: &str) {
    let ours = model_tables(model);
    let theirs = jade_tables(jade);
    assert_eq!(ours.len(), theirs.len(), "table count {context}");
    for (k, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
        if let Some(ours) = ours {
            assert_eq!(Some(ours), theirs.as_ref(), "table {k} {context}");
        }
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
    commit_as(model, jade, op, ReplicaId(1))
}

/// [`commit`] on behalf of `origin`.
pub fn commit_as(
    model: &mut DocxModel,
    jade: &mut JadeDocx,
    op: DocxOp,
    origin: ReplicaId,
) -> Vec<DocxEffect> {
    let ctx = ApplyContext {
        origin,
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
    assert_tables_agree(model, jade, &format!("after {op:?}"));
    assert_comments_agree(model, jade, &format!("after {op:?}"));
    assert_revisions_agree(model, jade, &format!("after {op:?}"));
    assert_formatting_agrees(model, jade, &format!("after {op:?}"));
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
    for op in jade.derived(model, cause).unwrap() {
        let effect = model
            .apply(&op, &ctx)
            .unwrap_or_else(|e| panic!("derived {op:?}: {e}"));
        jade.materialize(model, effect, &ctx)
            .unwrap_or_else(|e| panic!("derived {op:?}: {e}"));
    }
}

/// A tracked change as Word shows it on one character: who made it and when.
pub type Mark = Option<(Option<String>, Option<String>)>;

/// (inserted, deleted) for every character of every paragraph, as the
/// model shows them.
pub fn model_marks(model: &DocxModel) -> Vec<Vec<(Mark, Mark)>> {
    let mark = |id: Option<Id>| {
        id.map(|id| {
            let revision = model.revision(id).expect("a shown revision exists");
            (revision.author.clone(), revision.date.clone())
        })
    };
    model
        .paragraphs()
        .iter()
        .map(|p| {
            assert_eq!(p.tracked.len(), p.chars.len(), "one mark per character");
            p.tracked
                .iter()
                .map(|t| (mark(t.inserted), mark(t.deleted)))
                .collect()
        })
        .collect()
}

/// The same, read from jade's runs (a deletion inside an insertion carries
/// both).
pub fn jade_marks(model: &DocxModel, jade: &JadeDocx) -> Vec<Vec<(Mark, Mark)>> {
    word_marks(model, jade.word())
}

/// [`jade_marks`] read from any jade document holding the model's
/// paragraphs — the formatted copy an export writes, say.
pub fn word_marks(model: &DocxModel, word: &WordDocument) -> Vec<Vec<(Mark, Mark)>> {
    model
        .paragraphs()
        .iter()
        .map(|p| {
            let surface = word
                .paragraph_story_surface(&p.at)
                .unwrap_or_else(|| panic!("jade has no paragraph {:?}", p.at));
            let runs = &word.paragraph(surface).expect("surface exists").runs;
            let mut out = Vec::new();
            for run in runs {
                let of = |m: &jade_docx_document_core::document::RevisionMark| {
                    Some((m.author.clone(), m.date.clone()))
                };
                let (inserted, deleted) = match &run.revision {
                    None => (None, None),
                    Some(m) => match m.kind {
                        JadeRevisionKind::Inserted | JadeRevisionKind::MovedTo => (of(m), None),
                        JadeRevisionKind::Deleted | JadeRevisionKind::MovedFrom => {
                            (m.insertion.as_deref().and_then(of), of(m))
                        }
                    },
                };
                out.extend(
                    run.text
                        .chars()
                        .map(|_| (inserted.clone(), deleted.clone())),
                );
            }
            out
        })
        .collect()
}

/// Every character shows the same tracked insertion and deletion (author
/// and date) in the model and in jade.
///
/// Also in the copy an export writes, where convergent marks are applied
/// over the revisions: formatting tracked text must not move or drop them.
pub fn assert_revisions_agree(model: &DocxModel, jade: &JadeDocx, context: &str) {
    let ours = model_marks(model);
    let formatted = jade
        .formatted(model)
        .unwrap_or_else(|e| panic!("jade cannot realise the marks: {e}"));
    for (which, theirs) in [
        ("jade", jade_marks(model, jade)),
        ("the formatted copy", word_marks(model, &formatted)),
    ] {
        for (p, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            assert_eq!(
                a,
                b,
                "tracked changes of paragraph {p} ({:?}) in {which} {context}",
                model.paragraphs()[p].text
            );
        }
    }
}

/// Who is editing and when, for a generated edit.
pub fn context(replica: ReplicaId, choice: u64) -> Context {
    let dates = [
        Some("2026-10-18T09:00:00Z"),
        Some("2026-10-18T10:30:00Z"),
        None,
    ];
    Context {
        replica: Some(replica),
        date: dates[((choice >> 50) % 3) as usize].map(str::to_string),
    }
}

/// [`generate`], with tracked changes: the mode switched, tracked typing and
/// deleting (as the intent API makes them with tracking on, for `ctx`),
/// tracked deletions of arbitrary characters (already deleted ones and one's
/// own insertions included), and accepting and rejecting revisions.
pub fn generate_as(
    view: &DocxModel,
    choice: u64,
    ctx: &Context,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Option<DocxOp> {
    // generate_plain's 36 kinds, then 10 of tracked changes (30..=39).
    let kind = choice % 46;
    if kind < 36 {
        return generate_plain(view, choice, ids);
    }
    let kind = kind - 6;
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let tracking = || {
        let mut v = view.clone();
        let local = ApplyContext {
            origin: ctx.replica.unwrap_or(ReplicaId(1)),
            base_seq: 0,
            seq: None,
        };
        v.apply(&DocxOp::SetTracking { on: true }, &local)
            .expect("tracking can always be switched");
        v
    };
    let paragraphs = view.paragraphs();
    let boundaries =
        |t: &str| -> Vec<usize> { t.char_indices().map(|(i, _)| i).chain([t.len()]).collect() };
    match kind {
        30 => Some(DocxOp::SetTracking {
            on: pick(3, 8) != 0,
        }),
        31..=33 => {
            let p = pick(paragraphs.len(), 8);
            let b = boundaries(&paragraphs[p].text);
            let op = IntentOp::InsertText {
                paragraph: p,
                offset: b[pick(b.len(), 16)],
                text: ["x", "日本", "Lorem ", "é"][pick(4, 24)].to_string(),
            };
            resolve_with(&tracking(), &op, ctx, ids)
                .ok()?
                .into_iter()
                .next()
        }
        34 | 35 => {
            let p = pick(paragraphs.len(), 8);
            let b = boundaries(&paragraphs[p].text);
            let op = if kind == 34 {
                IntentOp::DeleteText {
                    paragraph: p,
                    offset: b[pick(b.len(), 16)],
                    direction: if pick(2, 28) == 0 {
                        jade_docx_headless_api::HeadlessDeleteDirection::Forward
                    } else {
                        jade_docx_headless_api::HeadlessDeleteDirection::Backward
                    },
                    count: 1 + pick(5, 24),
                }
            } else {
                let (so, eo) = (b[pick(b.len(), 16)], b[pick(b.len(), 24)]);
                IntentOp::ReplaceRange {
                    start_paragraph: p,
                    start_offset: so.min(eo),
                    end_paragraph: p,
                    end_offset: so.max(eo),
                    text: ["", "y", "置換"][pick(3, 32)].to_string(),
                }
            };
            let ops = resolve_with(&tracking(), &op, ctx, ids).ok()?;
            ops.get(pick(ops.len(), 40)).cloned()
        }
        36 => {
            let p = pick(paragraphs.len(), 8);
            let chars = &paragraphs[p].chars;
            if chars.is_empty() {
                return None;
            }
            let start = pick(chars.len(), 16);
            let ids_to_delete: Vec<Id> = chars[start..]
                .iter()
                .take(1 + pick(4, 24))
                .copied()
                .collect();
            Some(DocxOp::TrackDeleteText {
                story: paragraphs[p].story,
                ids: ids_to_delete,
                revision: ids(1),
                date: ctx.date.clone(),
            })
        }
        37..=39 => {
            let revisions = view.revisions();
            if revisions.is_empty() {
                return None;
            }
            let chosen: Vec<Id> = if kind == 39 {
                revisions.iter().map(|r| r.id).collect()
            } else {
                revisions
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| (choice >> (8 + (i % 40))) & 1 == 1)
                    .map(|(_, r)| r.id)
                    .collect()
            };
            let chosen = if chosen.is_empty() {
                vec![revisions[pick(revisions.len(), 12)].id]
            } else {
                chosen
            };
            Some(if (kind == 37) ^ (kind == 39 && pick(2, 30) == 0) {
                DocxOp::AcceptRevisions { revisions: chosen }
            } else {
                DocxOp::RejectRevisions { revisions: chosen }
            })
        }
        _ => None,
    }
}

/// The kinds of the model's live revisions, for assertions.
pub fn revision_kinds(model: &DocxModel) -> Vec<RevisionKind> {
    model.revisions().iter().map(|r| r.revision.kind).collect()
}

/// Turns a random `choice` into an operation on `view`. `ids(n)` reserves `n`
/// consecutive identifiers. Returns `None` when the choice does not fit the
/// document (e.g. no tables to edit). Includes tracked changes, made by an
/// unnamed replica.
pub fn generate(view: &DocxModel, choice: u64, ids: &mut dyn FnMut(u64) -> Id) -> Option<DocxOp> {
    generate_as(view, choice, &Context::default(), ids)
}

/// [`generate`] without tracked changes.
pub fn generate_plain(
    view: &DocxModel,
    choice: u64,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Option<DocxOp> {
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

/// An operation on an editable top-level table that goes through its merged
/// cells most of the time: a row or column inserted just above, inside or
/// just below a merge, or deleted from one; a new merge of up to 3 × 3
/// cells; an unmerge; or typing into a cell. `None` when there is no
/// editable table.
pub fn generate_through_merges(
    view: &DocxModel,
    choice: u64,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Option<DocxOp> {
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let tables = view.tables();
    let editable: Vec<(usize, &jade_docx_collab::TableView)> = tables
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.locked)
        .collect();
    let &(index, table) = editable.get(pick(editable.len(), 8))?;
    // A merge's span on one axis, widened by one line before it, or the
    // whole axis when there is no merge (or now and then anyway).
    let merge = (pick(4, 12) != 0)
        .then(|| table.merges.get(pick(table.merges.len(), 14)))
        .flatten();
    let line = |rows: bool, salt: u32| -> Option<usize> {
        let n = if rows {
            table.rows.len()
        } else {
            table.columns.len()
        };
        match merge {
            Some(&(top, left, bottom, right)) => {
                let (first, last) = if rows { (top, bottom) } else { (left, right) };
                let from = first.saturating_sub(1);
                let k = from + pick(last - from + 1, salt);
                // `None` stands for "insert at the start" (before line 0).
                (!(first == 0 && k == from && pick(2, salt + 5) == 0)).then_some(k)
            }
            None => Some(pick(n, salt)),
        }
    };
    let op = match choice % 10 {
        0 | 1 => GridOp::InsertRow {
            id: ids(1),
            after: line(true, 20).map(|k| table.rows[k]),
        },
        2 | 3 => GridOp::InsertColumn {
            id: ids(1),
            after: line(false, 20).map(|k| table.columns[k]),
        },
        4 => GridOp::DeleteRow {
            id: table.rows[line(true, 20).unwrap_or(0)],
        },
        5 => GridOp::DeleteColumn {
            id: table.columns[line(false, 20).unwrap_or(0)],
        },
        6 | 7 => {
            let top = pick(table.rows.len(), 20);
            let left = pick(table.columns.len(), 24);
            let op = IntentOp::MergeTableCells {
                table: index,
                top_row: top,
                left_column: left,
                bottom_row: (top + pick(3, 28)).min(table.rows.len() - 1),
                right_column: (left + pick(3, 32)).min(table.columns.len() - 1),
            };
            return resolve(view, &op, ids).ok()?.into_iter().next();
        }
        8 => {
            let &(row, column, _, _) = merge?;
            let op = IntentOp::UnmergeTableCells {
                table: index,
                row,
                column,
            };
            return resolve(view, &op, ids).ok()?.into_iter().next();
        }
        _ => {
            // Typing at the start of a cell of this table.
            let paragraphs = view.paragraphs();
            let cells: Vec<usize> = paragraphs
                .iter()
                .enumerate()
                .filter(|(_, p)| view.cell_of(p.story).is_some_and(|(t, _, _)| t == table.id))
                .map(|(k, _)| k)
                .collect();
            let p = *cells.get(pick(cells.len(), 20))?;
            let op = IntentOp::InsertText {
                paragraph: p,
                offset: 0,
                text: ["m", "表"][pick(2, 30)].to_string(),
            };
            return resolve(view, &op, ids).ok()?.into_iter().next();
        }
    };
    Some(DocxOp::Table {
        table: table.id,
        op,
    })
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
    /// Whether jade's run has properties at all. jade leaves the runs of a
    /// shape it inserts without the defaults it resolves when it reads a
    /// file (the document's font size, say) until some later edit resolves
    /// them; the file is the same either way.
    pub resolved: bool,
}

fn query(word: &WordDocument, query: HeadlessQuery) -> HeadlessQueryResult {
    execute_headless_queries_on_document(
        word,
        &HeadlessQueryRequest {
            queries: vec![query],
        },
    )
    .expect("query runs")
    .results
    .remove(0)
}

pub fn paragraph_snapshot(jade: &JadeDocx, surface: usize) -> HeadlessParagraphSnapshot {
    snapshot_in(jade.word(), surface)
}

fn snapshot_in(word: &WordDocument, surface: usize) -> HeadlessParagraphSnapshot {
    match query(word, HeadlessQuery::Paragraph { paragraph: surface }) {
        HeadlessQueryResult::Paragraph {
            value: Some(value), ..
        } => value,
        other => panic!("paragraph {surface}: {other:?}"),
    }
}

/// Every visible character of `model`, with what jade's own document (the
/// base the model's marks are realised on) says about it.
pub fn char_styles(model: &DocxModel, jade: &JadeDocx) -> BTreeMap<Id, CharStyle> {
    char_styles_in(model, jade.word())
}

/// Every visible character of `model`, with what jade's own document says
/// about it once written and read back. jade resolves the properties of a
/// run the file gives none (a text box made in the session) only when some
/// edit touches it, sometimes to theme fonts and sometimes to their names;
/// the file is the same either way, so formatting checks compare files.
pub fn written_char_styles(model: &DocxModel, jade: &JadeDocx) -> BTreeMap<Id, CharStyle> {
    let bytes = jade
        .export_base()
        .unwrap_or_else(|e| panic!("jade cannot write the document: {e}"));
    let reread = JadeDocx::load(&bytes).unwrap_or_else(|e| panic!("jade cannot reread it: {e}"));
    char_styles_in(model, reread.word())
}

/// Every visible character of `model`, with what the file jade writes —
/// its document with the model's marks realised — says about it.
pub fn formatted_char_styles(model: &DocxModel, jade: &JadeDocx) -> BTreeMap<Id, CharStyle> {
    let formatted = jade
        .formatted(model)
        .unwrap_or_else(|e| panic!("jade cannot realise the marks: {e}"));
    char_styles_in(model, &formatted)
}

/// Every visible character of `model`, with what `word` says about it.
pub fn char_styles_in(model: &DocxModel, word: &WordDocument) -> BTreeMap<Id, CharStyle> {
    let mut out = BTreeMap::new();
    for paragraph in model.paragraphs().iter() {
        let surface = word
            .paragraph_story_surface(&paragraph.at)
            .unwrap_or_else(|| panic!("jade has no paragraph {:?}", paragraph.at));
        let snapshot = snapshot_in(word, surface);
        let runs = &word.paragraph(surface).expect("surface exists").runs;
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
            let properties = runs
                .iter()
                .find(|r| {
                    let found = start <= offset && offset < start + r.text.len();
                    start += r.text.len();
                    found
                })
                .and_then(|r| r.properties.as_ref());
            let highlight = properties
                .and_then(|p| p.highlight.as_deref())
                .map(str::to_string);
            out.insert(
                *id,
                CharStyle {
                    style: run.style.clone(),
                    highlight,
                    resolved: properties.is_some(),
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
        jade.word(),
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

/// Whether `style` shows `change` (for the properties jade's queries report).
pub fn shows(change: &FormatChange, style: &CharStyle) -> bool {
    let s = &style.style;
    match change {
        FormatChange::Bold { value } => s.bold.unwrap_or(false) == *value,
        FormatChange::Italic { value } => s.italic.unwrap_or(false) == *value,
        FormatChange::Underline { value, .. } => s.underline.unwrap_or(false) == *value,
        // The query reports `w:strike` only, not `w:dstrike`.
        FormatChange::Strike { value, double } => *double || s.strike.unwrap_or(false) == *value,
        FormatChange::FontSize { half_points } => s.font_size == Some(*half_points as f32 / 2.0),
        FormatChange::FontFamily { family } => s.font_family.as_deref() == Some(family.as_str()),
        FormatChange::FontColor { color } => s.color.as_deref() == Some(color.as_str()),
        FormatChange::Highlight { color } => style.highlight.as_deref() == Some(color.as_str()),
        // Clearing restores whatever the style says; only "elsewhere nothing
        // changed" is checked for it.
        FormatChange::ClearFormatting => true,
        FormatChange::Alignment { .. } | FormatChange::ParagraphStyle { .. } => true,
    }
}

pub const MARK_KEYS: [MarkKey; 8] = MarkKey::ALL;

/// What `style` reports for one attribute.
pub fn attribute(style: &CharStyle, key: MarkKey) -> String {
    let s = &style.style;
    match key {
        MarkKey::Bold => format!("{:?}", s.bold),
        MarkKey::Italic => format!("{:?}", s.italic),
        MarkKey::Underline => format!("{:?}", s.underline),
        MarkKey::Strike => format!("{:?}", s.strike),
        MarkKey::FontSize => format!("{:?}", s.font_size),
        MarkKey::FontFamily => format!("{:?}", s.font_family),
        MarkKey::FontColor => format!("{:?}", s.color),
        MarkKey::Highlight => format!("{:?}", style.highlight),
    }
}

/// The file the server writes shows the model's formatting: every
/// character a mark sets shows that value, and every attribute no mark sets
/// is what jade's own document has (the file's formatting, jade's rules for
/// typed text, and sequenced formatting) — whatever jade's typing rules
/// would have made of the marks around it.
pub fn assert_formatting_agrees(model: &DocxModel, jade: &JadeDocx, context: &str) {
    let base = char_styles(model, jade);
    let formatted = formatted_char_styles(model, jade);
    let overrides = model.character_formats();
    for (c, style) in &formatted {
        let set = overrides.get(c).map(Vec::as_slice).unwrap_or_default();
        for key in MARK_KEYS {
            match set.iter().find(|change| change.mark_key() == Some(key)) {
                Some(change) => assert!(
                    shows(change, style),
                    "{c} does not show {change:?}: {style:?} {context}"
                ),
                None => assert_eq!(
                    attribute(style, key),
                    attribute(&base[c], key),
                    "{key:?} of {c}, which no mark sets, is not jade's own {context}"
                ),
            }
        }
    }
}
