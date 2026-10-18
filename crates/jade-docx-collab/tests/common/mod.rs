#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use jade_collab_core::grid::GridOp;
use jade_collab_core::id::Id;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{
    Anchor, CommentContent, DocxModel, DocxOp, FormatChange, JadeDocx, ParagraphAlignment, StoryId,
};
use jade_docx_document_core::headless_api::execute_headless_queries_on_document;
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

/// Turns a random `choice` into an operation on `view`. `ids(n)` reserves `n`
/// consecutive identifiers. Returns `None` when the choice does not fit the
/// document (e.g. no tables to edit).
pub fn generate(view: &DocxModel, choice: u64, ids: &mut dyn FnMut(u64) -> Id) -> Option<DocxOp> {
    let paragraphs = view.paragraphs();
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let words = ["x", "日本", "é", "Lorem ", "  ", "ab"];
    match choice % 22 {
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
            let op = match choice % 22 {
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
        // Answering or resolving a live comment.
        20 => {
            let comments = view.comments();
            if comments.is_empty() {
                return None;
            }
            let comment = comments[pick(comments.len(), 8)].id;
            Some(if pick(2, 16) == 0 {
                DocxOp::ReplyToComment {
                    comment,
                    id: ids(1),
                    content: CommentContent::new("reply"),
                }
            } else {
                DocxOp::ResolveComment {
                    comment,
                    resolved: pick(3, 20) != 0,
                }
            })
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
    let surfaces = jade.body_surfaces();
    let mut out = BTreeMap::new();
    for (ordinal, paragraph) in model.paragraphs().iter().enumerate() {
        let surface = surfaces[ordinal];
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
    let surfaces = jade.body_surfaces();
    (0..model.paragraphs().len())
        .map(|ordinal| {
            let snapshot = paragraph_snapshot(jade, surfaces[ordinal]);
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
