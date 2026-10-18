//! Formatting and comments in the model: ranges are named by character ids
//! and resolved to the visible characters in log order; the model reports
//! where jade has to format, and never stores formatting itself.

use std::collections::BTreeSet;

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_docx_collab::{
    Anchor, CommentContent, DocxEffect, DocxModel, DocxOp, FormatChange, ParagraphAlignment,
    StoryId,
};
use proptest::prelude::*;

fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    }
}

fn id(n: u64) -> Id {
    Id::new(n, ReplicaId(1))
}

fn bold() -> FormatChange {
    FormatChange::Bold { value: true }
}

fn format(start: Id, end: Id, change: FormatChange) -> DocxOp {
    DocxOp::Format {
        story: StoryId::Body,
        start,
        end,
        change,
    }
}

fn apply(model: &mut DocxModel, op: DocxOp) -> Vec<DocxEffect> {
    model
        .apply(&op, &ctx())
        .unwrap_or_else(|e| panic!("{op:?}: {e}"))
}

fn delete(model: &mut DocxModel, ids: Vec<Id>) {
    apply(
        model,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids,
        },
    );
}

fn text(paragraph: usize, start: usize, end: usize) -> DocxEffect {
    DocxEffect::FormatText {
        paragraph,
        start,
        end,
        change: bold(),
    }
}

#[test]
fn a_range_across_paragraphs_formats_each_paragraph_slice() {
    let mut model = DocxModel::from_paragraphs(&["Hello", "big", "world"]);
    let p = model.paragraphs();
    let effects = apply(&mut model, format(p[0].chars[1], p[2].chars[2], bold()));
    assert_eq!(effects, vec![text(0, 1, 5), text(1, 0, 3), text(2, 0, 3)]);
}

#[test]
fn formatting_does_not_change_the_model() {
    let mut model = DocxModel::from_paragraphs(&["Hello", "world"]);
    let before = model.clone();
    let p = model.paragraphs();
    apply(&mut model, format(p[0].chars[0], p[1].chars[4], bold()));
    assert_eq!(model, before, "the model does not store formatting");
}

#[test]
fn offsets_are_utf8_bytes_of_the_visible_text() {
    let mut model = DocxModel::from_paragraphs(&["日本語です"]);
    let c = model.paragraphs()[0].chars.clone();
    let effects = apply(&mut model, format(c[1], c[2], bold()));
    assert_eq!(effects, vec![text(0, 3, 9)]);
}

#[test]
fn characters_deleted_before_the_format_shrink_the_range() {
    let mut model = DocxModel::from_paragraphs(&["Hello", "world"]);
    let p = model.paragraphs();
    // The range is e..r; its first and last characters and one inside it go.
    let (start, end) = (p[0].chars[1], p[1].chars[2]);
    delete(&mut model, vec![start, p[0].chars[3], end]);
    assert_eq!(model.paragraph_texts(), ["Hlo", "wold"]);
    let effects = apply(&mut model, format(start, end, bold()));
    assert_eq!(effects, vec![text(0, 1, 3), text(1, 0, 2)]);
}

#[test]
fn characters_inserted_inside_the_range_before_it_commits_are_formatted() {
    let mut model = DocxModel::from_paragraphs(&["abcd"]);
    let c = model.paragraphs()[0].chars.clone();
    apply(
        &mut model,
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: Some(c[1]),
            first: id(100),
            text: "XY".into(),
        },
    );
    assert_eq!(model.paragraph_texts(), ["abXYcd"]);
    let effects = apply(&mut model, format(c[1], c[2], bold()));
    assert_eq!(effects, vec![text(0, 1, 5)]);
}

#[test]
fn a_range_whose_characters_are_all_gone_is_a_no_op() {
    let mut model = DocxModel::from_paragraphs(&["abcd"]);
    let c = model.paragraphs()[0].chars.clone();
    delete(&mut model, vec![c[1], c[2]]);
    let before = model.clone();
    assert_eq!(apply(&mut model, format(c[1], c[2], bold())), vec![]);
    assert_eq!(
        apply(
            &mut model,
            format(
                c[1],
                c[2],
                FormatChange::Alignment {
                    alignment: ParagraphAlignment::Center
                }
            )
        ),
        vec![]
    );
    assert_eq!(model, before);
}

#[test]
fn a_join_before_the_format_puts_the_range_in_one_paragraph() {
    let mut model = DocxModel::from_paragraphs(&["ab", "cd"]);
    let p = model.paragraphs();
    apply(
        &mut model,
        DocxOp::JoinParagraph {
            story: StoryId::Body,
            mark: p[0].mark.unwrap(),
        },
    );
    let effects = apply(&mut model, format(p[0].chars[1], p[1].chars[0], bold()));
    assert_eq!(effects, vec![text(0, 1, 3)]);
}

#[test]
fn paragraph_changes_apply_to_every_paragraph_the_range_touches() {
    let mut model = DocxModel::from_paragraphs(&["a", "", "c", "d"]);
    let p = model.paragraphs();
    let center = FormatChange::Alignment {
        alignment: ParagraphAlignment::Center,
    };
    let effects = apply(
        &mut model,
        format(p[0].chars[0], p[2].chars[0], center.clone()),
    );
    assert_eq!(
        effects,
        (0..3)
            .map(|paragraph| DocxEffect::FormatParagraph {
                paragraph,
                change: center.clone()
            })
            .collect::<Vec<_>>()
    );
    // An empty paragraph is reached through its mark.
    let mark = p[1].mark.unwrap();
    let style = FormatChange::ParagraphStyle {
        style_id: Some("Heading1".into()),
    };
    assert_eq!(
        apply(&mut model, format(mark, mark, style.clone())),
        vec![DocxEffect::FormatParagraph {
            paragraph: 1,
            change: style
        }]
    );
    // Character formatting ignores marks.
    assert_eq!(apply(&mut model, format(mark, mark, bold())), vec![]);
}

#[test]
fn ranges_are_validated() {
    let mut model = DocxModel::from_paragraphs(&["abc", "de"]);
    let p = model.paragraphs();
    let before = model.clone();
    let rejected = |model: &mut DocxModel, op: DocxOp| {
        assert!(model.apply(&op, &ctx()).is_err(), "{op:?} was accepted");
    };
    rejected(&mut model, format(p[0].chars[2], p[0].chars[0], bold()));
    rejected(&mut model, format(id(999), p[0].chars[0], bold()));
    rejected(
        &mut model,
        DocxOp::Format {
            story: StoryId::Cell {
                table: id(1),
                row: id(2),
                column: id(3),
            },
            start: p[0].chars[0],
            end: p[0].chars[0],
            change: bold(),
        },
    );
    for change in [
        FormatChange::FontSize { half_points: 0 },
        FormatChange::FontSize { half_points: 4000 },
        FormatChange::FontFamily {
            family: "  ".into(),
        },
        FormatChange::FontColor {
            color: "red".into(),
        },
        FormatChange::FontColor {
            color: "#12345".into(),
        },
        FormatChange::Highlight {
            color: "<script>".into(),
        },
        // w:highlight takes Word's named colors only.
        FormatChange::Highlight {
            color: "#FFFF00".into(),
        },
        FormatChange::Underline {
            value: true,
            style: Some("zigzag".into()),
        },
        FormatChange::ParagraphStyle {
            style_id: Some(String::new()),
        },
    ] {
        rejected(&mut model, format(p[0].chars[0], p[1].chars[1], change));
    }
    assert_eq!(model, before);
    for change in [
        FormatChange::FontSize { half_points: 21 },
        FormatChange::FontColor {
            color: "#00ff7F".into(),
        },
        FormatChange::Highlight {
            color: "yellow".into(),
        },
        FormatChange::Highlight {
            color: "darkBlue".into(),
        },
        FormatChange::Underline {
            value: true,
            style: Some("double".into()),
        },
        FormatChange::ParagraphStyle { style_id: None },
    ] {
        apply(&mut model, format(p[0].chars[0], p[1].chars[1], change));
    }
}

#[test]
fn formatting_text_in_a_deleted_table_does_nothing() {
    let mut model = DocxModel::from_paragraphs(&["before", "after"]);
    let mark = model.paragraphs()[0].mark.unwrap();
    let (table, row, column) = (id(200), id(201), id(202));
    apply(
        &mut model,
        DocxOp::InsertTable {
            anchor: Anchor::After(mark),
            id: table,
            rows: vec![row],
            columns: vec![column],
        },
    );
    let cell = StoryId::Cell { table, row, column };
    apply(
        &mut model,
        DocxOp::InsertText {
            story: cell,
            parent: None,
            first: id(300),
            text: "xy".into(),
        },
    );
    let op = DocxOp::Format {
        story: cell,
        start: id(300),
        end: id(301),
        change: bold(),
    };
    assert_eq!(
        model.clone().apply(&op, &ctx()).unwrap(),
        vec![text(1, 0, 2)]
    );
    apply(&mut model, DocxOp::DeleteTable { table });
    assert_eq!(apply(&mut model, op), vec![]);
}

#[test]
fn formatting_and_comments_are_sequenced() {
    let c = CommentContent::new("note");
    for op in [
        format(id(1), id(2), bold()),
        DocxOp::AddComment {
            story: StoryId::Body,
            start: id(1),
            end: id(2),
            id: id(3),
            content: c.clone(),
        },
        DocxOp::ReplyToComment {
            comment: id(3),
            id: id(4),
            content: c,
        },
        DocxOp::ResolveComment {
            comment: id(3),
            resolved: true,
        },
        DocxOp::DeleteComment { comment: id(3) },
    ] {
        assert_eq!(DocxModel::lane(&op), Lane::Sequenced, "{op:?}");
    }
}

#[test]
fn format_ops_use_a_stable_wire_format() {
    let op: DocxOp = serde_json::from_str(
        r#"{"type":"format","story":{"kind":"body"},"start":{"lamport":1,"replica":0},
            "end":{"lamport":3,"replica":0},"change":{"kind":"font_size","half_points":21}}"#,
    )
    .unwrap();
    assert_eq!(
        op,
        DocxOp::Format {
            story: StoryId::Body,
            start: Id::new(1, ReplicaId::ORIGIN),
            end: Id::new(3, ReplicaId::ORIGIN),
            change: FormatChange::FontSize { half_points: 21 },
        }
    );
    let change: FormatChange =
        serde_json::from_str(r#"{"kind":"alignment","alignment":"justify"}"#).unwrap();
    assert_eq!(
        change,
        FormatChange::Alignment {
            alignment: ParagraphAlignment::Justify
        }
    );
    let change: FormatChange = serde_json::from_str(r#"{"kind":"clear_formatting"}"#).unwrap();
    assert_eq!(change, FormatChange::ClearFormatting);
}

// --- comments ---------------------------------------------------------------

fn add_comment(model: &mut DocxModel, start: Id, end: Id, comment: Id, text: &str) -> DocxEffect {
    let mut effects = apply(
        model,
        DocxOp::AddComment {
            story: StoryId::Body,
            start,
            end,
            id: comment,
            content: CommentContent {
                text: text.into(),
                author: Some("Ann".into()),
                initials: None,
                date: None,
            },
        },
    );
    assert_eq!(effects.len(), 1);
    effects.remove(0)
}

#[test]
fn a_comment_anchors_to_the_visible_range() {
    let mut model = DocxModel::from_paragraphs(&["Hello", "world"]);
    let p = model.paragraphs();
    delete(&mut model, vec![p[0].chars[1]]);
    let effect = add_comment(&mut model, p[0].chars[1], p[1].chars[2], id(100), "why?");
    assert_eq!(
        effect,
        DocxEffect::AddComment {
            comment: id(100),
            start_paragraph: 0,
            start: 1,
            end_paragraph: 1,
            end: 3,
            content: CommentContent {
                text: "why?".into(),
                author: Some("Ann".into()),
                initials: None,
                date: None,
            },
        }
    );
    let comments = model.comments();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].id, id(100));
    assert_eq!(comments[0].parent, None);
    assert_eq!(comments[0].content.text, "why?");
    assert!(!comments[0].resolved);
    assert_eq!(
        model.range_chars(StoryId::Body, p[0].chars[1], p[1].chars[2]),
        Some(vec![
            p[0].chars[2],
            p[0].chars[3],
            p[0].chars[4],
            p[1].chars[0],
            p[1].chars[1],
            p[1].chars[2]
        ])
    );
    assert!(model.max_lamport() >= 100);
}

#[test]
fn a_comment_on_text_that_is_gone_is_rejected() {
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let c = model.paragraphs()[0].chars.clone();
    delete(&mut model, vec![c[0], c[1]]);
    let before = model.clone();
    let op = DocxOp::AddComment {
        story: StoryId::Body,
        start: c[0],
        end: c[1],
        id: id(100),
        content: CommentContent::new("gone"),
    };
    assert!(model.apply(&op, &ctx()).is_err());
    assert_eq!(model, before);
}

#[test]
fn comment_ops_are_validated() {
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let c = model.paragraphs()[0].chars.clone();
    add_comment(&mut model, c[0], c[1], id(100), "first");
    let before = model.clone();
    for op in [
        // Blank text.
        DocxOp::AddComment {
            story: StoryId::Body,
            start: c[0],
            end: c[1],
            id: id(101),
            content: CommentContent::new(" \n"),
        },
        // An id that is taken.
        DocxOp::AddComment {
            story: StoryId::Body,
            start: c[0],
            end: c[1],
            id: id(100),
            content: CommentContent::new("again"),
        },
        DocxOp::ReplyToComment {
            comment: id(100),
            id: id(100),
            content: CommentContent::new("again"),
        },
        DocxOp::ReplyToComment {
            comment: id(100),
            id: id(102),
            content: CommentContent::new(""),
        },
        // Unknown comments.
        DocxOp::ReplyToComment {
            comment: id(7),
            id: id(103),
            content: CommentContent::new("?"),
        },
        DocxOp::ResolveComment {
            comment: id(7),
            resolved: true,
        },
        DocxOp::DeleteComment { comment: id(7) },
    ] {
        assert!(model.apply(&op, &ctx()).is_err(), "{op:?} was accepted");
        assert_eq!(model, before);
    }
}

#[test]
fn replies_join_the_thread_of_their_root() {
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let c = model.paragraphs()[0].chars.clone();
    add_comment(&mut model, c[0], c[2], id(100), "root");
    let reply = |model: &mut DocxModel, to: Id, reply: Id| {
        apply(
            model,
            DocxOp::ReplyToComment {
                comment: to,
                id: reply,
                content: CommentContent::new("reply"),
            },
        )
    };
    assert_eq!(
        reply(&mut model, id(100), id(101)),
        vec![DocxEffect::ReplyToComment {
            comment: id(100),
            reply: id(101),
            content: CommentContent::new("reply"),
        }]
    );
    // Word keeps threads flat: a reply to a reply answers the root.
    assert_eq!(
        reply(&mut model, id(101), id(102)),
        vec![DocxEffect::ReplyToComment {
            comment: id(100),
            reply: id(102),
            content: CommentContent::new("reply"),
        }]
    );
    let parents: Vec<_> = model.comments().iter().map(|c| (c.id, c.parent)).collect();
    assert_eq!(
        parents,
        [
            (id(100), None),
            (id(101), Some(id(100))),
            (id(102), Some(id(100)))
        ]
    );
}

#[test]
fn resolving_and_deleting() {
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let c = model.paragraphs()[0].chars.clone();
    add_comment(&mut model, c[0], c[2], id(100), "root");
    add_comment(&mut model, c[1], c[1], id(200), "other");
    apply(
        &mut model,
        DocxOp::ReplyToComment {
            comment: id(100),
            id: id(101),
            content: CommentContent::new("reply"),
        },
    );
    assert_eq!(
        apply(
            &mut model,
            DocxOp::ResolveComment {
                comment: id(100),
                resolved: true
            }
        ),
        vec![DocxEffect::ResolveComment {
            comment: id(100),
            resolved: true
        }]
    );
    let resolved: Vec<_> = model.comments().iter().map(|c| c.resolved).collect();
    assert_eq!(resolved, [true, false, false], "resolving does not cascade");

    // Deleting a reply removes only the reply.
    assert_eq!(
        apply(&mut model, DocxOp::DeleteComment { comment: id(101) }),
        vec![DocxEffect::DeleteComment { comment: id(101) }]
    );
    let live: Vec<_> = model.comments().iter().map(|c| c.id).collect();
    assert_eq!(live, [id(100), id(200)]);

    // Deleting a root takes its replies with it, as jade does.
    apply(
        &mut model,
        DocxOp::ReplyToComment {
            comment: id(100),
            id: id(102),
            content: CommentContent::new("again"),
        },
    );
    apply(&mut model, DocxOp::DeleteComment { comment: id(100) });
    let live: Vec<_> = model.comments().iter().map(|c| c.id).collect();
    assert_eq!(live, [id(200)]);

    // A deleted comment cannot be answered, resolved or deleted again.
    for op in [
        DocxOp::ReplyToComment {
            comment: id(100),
            id: id(103),
            content: CommentContent::new("late"),
        },
        DocxOp::ResolveComment {
            comment: id(102),
            resolved: true,
        },
        DocxOp::DeleteComment { comment: id(100) },
    ] {
        assert!(model.apply(&op, &ctx()).is_err(), "{op:?} was accepted");
    }
}

#[test]
fn the_model_round_trips_through_json_with_comments() {
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let c = model.paragraphs()[0].chars.clone();
    add_comment(&mut model, c[0], c[2], id(100), "root");
    let json = serde_json::to_string(&model).unwrap();
    let back: DocxModel = serde_json::from_str(&json).unwrap();
    assert_eq!(back, model);
}

// --- properties -------------------------------------------------------------

/// A body of up to four paragraphs of short ASCII/CJK text.
fn paragraphs() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-c日本 ]{0,6}", 1..5)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// After arbitrary deletions, a format op reports exactly the surviving
    /// characters between its endpoints, one contiguous slice per paragraph,
    /// at the byte offsets of the current text — and nothing else changes.
    #[test]
    fn a_format_reports_exactly_the_surviving_characters_in_range(
        texts in paragraphs(),
        a in any::<prop::sample::Index>(),
        b in any::<prop::sample::Index>(),
        deleted in prop::collection::vec(any::<prop::sample::Index>(), 0..8),
    ) {
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut model = DocxModel::from_paragraphs(&texts);
        let all: Vec<Id> = model.paragraphs().iter().flat_map(|p| p.chars.clone()).collect();
        prop_assume!(!all.is_empty());
        let (a, b) = (a.index(all.len()), b.index(all.len()));
        let (a, b) = (a.min(b), a.max(b));
        let gone: BTreeSet<Id> = deleted.iter().map(|i| all[i.index(all.len())]).collect();
        delete(&mut model, gone.iter().copied().collect());
        let in_range: BTreeSet<Id> = all[a..=b].iter().filter(|c| !gone.contains(c)).copied().collect();

        let mut expected = Vec::new();
        for (ordinal, paragraph) in model.paragraphs().iter().enumerate() {
            let mut offset = 0;
            let mut span: Option<(usize, usize)> = None;
            for (c, id) in paragraph.text.chars().zip(&paragraph.chars) {
                if in_range.contains(id) {
                    let s = span.get_or_insert((offset, offset));
                    prop_assert_eq!(s.1, offset, "a range is contiguous within a paragraph");
                    s.1 = offset + c.len_utf8();
                }
                offset += c.len_utf8();
            }
            if let Some((start, end)) = span {
                expected.push(text(ordinal, start, end));
            }
        }
        prop_assert_eq!(
            model.range_chars(StoryId::Body, all[a], all[b]),
            Some(model.paragraphs().iter().flat_map(|p| p.chars.clone()).filter(|c| in_range.contains(c)).collect::<Vec<_>>())
        );
        let before = model.clone();
        let effects = apply(&mut model, format(all[a], all[b], bold()));
        prop_assert_eq!(effects, expected);
        prop_assert_eq!(model, before);
    }

    /// Paragraph changes reach exactly the paragraphs that still hold a
    /// visible atom of the range.
    #[test]
    fn a_paragraph_change_reaches_exactly_the_touched_paragraphs(
        texts in paragraphs(),
        a in any::<prop::sample::Index>(),
        b in any::<prop::sample::Index>(),
        deleted in prop::collection::vec(any::<prop::sample::Index>(), 0..8),
    ) {
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut model = DocxModel::from_paragraphs(&texts);
        // Every atom: characters, then the paragraph's mark.
        let atoms: Vec<(usize, Id, bool)> = model
            .paragraphs()
            .iter()
            .enumerate()
            .flat_map(|(n, p)| {
                p.chars.iter().map(move |c| (n, *c, true)).chain(p.mark.map(|m| (n, m, false)))
            })
            .collect();
        prop_assume!(!atoms.is_empty());
        let (a, b) = (a.index(atoms.len()), b.index(atoms.len()));
        let (a, b) = (a.min(b), a.max(b));
        let gone: BTreeSet<Id> = deleted
            .iter()
            .map(|i| atoms[i.index(atoms.len())])
            .filter(|(_, _, is_char)| *is_char)
            .map(|(_, id, _)| id)
            .collect();
        delete(&mut model, gone.iter().copied().collect());
        let touched: BTreeSet<usize> = atoms[a..=b]
            .iter()
            .filter(|(_, id, _)| !gone.contains(id))
            .map(|(n, _, _)| *n)
            .collect();
        let change = FormatChange::Alignment { alignment: ParagraphAlignment::Right };
        let effects = apply(&mut model, format(atoms[a].1, atoms[b].1, change.clone()));
        let expected: Vec<DocxEffect> = touched
            .into_iter()
            .map(|paragraph| DocxEffect::FormatParagraph { paragraph, change: change.clone() })
            .collect();
        prop_assert_eq!(effects, expected);
    }

    /// Random comment histories keep the table consistent: no live reply
    /// outlives its root, every reply's parent is a root, and a rejected op
    /// changes nothing.
    #[test]
    fn comment_threads_stay_well_formed(
        steps in prop::collection::vec((0..4u8, any::<prop::sample::Index>(), any::<bool>()), 1..40),
    ) {
        let mut model = DocxModel::from_paragraphs(&["abcdef", "ghi"]);
        let chars: Vec<Id> = model.paragraphs().iter().flat_map(|p| p.chars.clone()).collect();
        let mut next = 1_000;
        let mut known: Vec<Id> = Vec::new();
        for (kind, pick, flag) in steps {
            next += 1;
            let op = match kind {
                0 => {
                    let i = pick.index(chars.len());
                    DocxOp::AddComment {
                        story: StoryId::Body,
                        start: chars[i],
                        end: chars[(i + 2).min(chars.len() - 1)],
                        id: id(next),
                        content: CommentContent::new("c"),
                    }
                }
                _ if known.is_empty() => continue,
                1 => DocxOp::ReplyToComment {
                    comment: *pick.get(&known),
                    id: id(next),
                    content: CommentContent::new("r"),
                },
                2 => DocxOp::ResolveComment { comment: *pick.get(&known), resolved: flag },
                _ => DocxOp::DeleteComment { comment: *pick.get(&known) },
            };
            let before = model.clone();
            match model.apply(&op, &ctx()) {
                Ok(_) => {
                    if let DocxOp::AddComment { id, .. } | DocxOp::ReplyToComment { id, .. } = op {
                        known.push(id);
                    }
                }
                Err(_) => prop_assert_eq!(&model, &before),
            }
            let live = model.comments();
            let roots: BTreeSet<Id> = live.iter().filter(|c| c.parent.is_none()).map(|c| c.id).collect();
            for comment in &live {
                if let Some(parent) = comment.parent {
                    prop_assert!(roots.contains(&parent), "{:?} hangs off a dead or nested parent", comment);
                }
            }
        }
    }
}

/// jade drops the comments anchored in cells it deletes; the model follows.
#[test]
fn comments_in_removed_cells_go_with_them() {
    use jade_collab_core::grid::GridOp;

    let mut model = DocxModel::from_paragraphs(&["before", "after"]);
    let mark = model.paragraphs()[0].mark.unwrap();
    let (table, r1, r2, column) = (id(200), id(201), id(202), id(203));
    apply(
        &mut model,
        DocxOp::InsertTable {
            anchor: Anchor::After(mark),
            id: table,
            rows: vec![r1, r2],
            columns: vec![column],
        },
    );
    for (row, first) in [(r1, 300), (r2, 310)] {
        let story = StoryId::Cell { table, row, column };
        apply(
            &mut model,
            DocxOp::InsertText {
                story,
                parent: None,
                first: id(first),
                text: "xy".into(),
            },
        );
        apply(
            &mut model,
            DocxOp::AddComment {
                story,
                start: id(first),
                end: id(first + 1),
                id: id(first + 5),
                content: CommentContent::new("cell"),
            },
        );
    }
    apply(
        &mut model,
        DocxOp::ReplyToComment {
            comment: id(305),
            id: id(306),
            content: CommentContent::new("reply"),
        },
    );
    let p = model.paragraphs();
    add_comment(&mut model, p[0].chars[0], p[3].chars[0], id(400), "body");
    apply(
        &mut model,
        DocxOp::Table {
            table,
            op: GridOp::DeleteRow { id: r1 },
        },
    );
    let live: Vec<Id> = model.comments().iter().map(|c| c.id).collect();
    assert_eq!(live, [id(315), id(400)]);
    apply(&mut model, DocxOp::DeleteTable { table });
    let live: Vec<Id> = model.comments().iter().map(|c| c.id).collect();
    assert_eq!(live, [id(400)], "a body comment around the table stays");
}
