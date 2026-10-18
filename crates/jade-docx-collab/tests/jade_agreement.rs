//! The model and jade must describe the same document: the same paragraphs
//! — body, table cells, text boxes, footnotes and endnotes — in the same
//! order with the same text, the same story positions and the same text
//! boxes and note references at the same offsets, and the same top-level
//! tables — on load, and after every operation the materializer realises.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{
    assert_comments_agree, assert_same_order, assert_stories_agree, assert_tables_agree,
    char_styles, commit, generate, generate_through_merges, jade_comments, jade_core, open,
    paragraph_formats, paragraph_snapshot, surface, CharStyle,
};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{
    CommentContent, DocxEffect, DocxModel, DocxOp, FormatChange, JadeDocx, ParagraphAlignment,
    ParagraphStory, StoryId,
};
use proptest::prelude::*;

fn assert_agree(model: &DocxModel, jade: &JadeDocx, context: &str) {
    assert_stories_agree(model, jade, context);
    assert_tables_agree(model, jade, context);
    let shapes: Vec<(usize, usize)> = model
        .tables()
        .iter()
        .map(|t| (t.rows.len(), t.columns.len()))
        .collect();
    let jade_shapes = jade.table_shapes();
    for (m, j) in model.tables().iter().zip(&jade_shapes) {
        if !m.locked {
            assert_eq!((m.rows.len(), m.columns.len()), *j, "table shape {context}");
        }
    }
    assert_eq!(shapes.len(), jade_shapes.len(), "table count {context}");
}

/// Every fixture jade can open yields a model that agrees with it. This pins
/// the model's traversal to jade's paragraph-surface enumeration (tables,
/// nested tables, text boxes, footnotes and endnotes interleaved): every
/// paragraph surface, not only the body's.
#[test]
fn every_fixture_loads_into_an_agreeing_model() {
    let dir = jade_core().join("public/fixtures");
    let mut checked = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "docx"))
        .collect();
    entries.sort();
    for path in entries {
        let Ok(jade) = JadeDocx::load(&std::fs::read(&path).unwrap()) else {
            continue;
        };
        let model = DocxModel::from_word_document(jade.word())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_agree(&model, &jade, &path.display().to_string());
        assert_same_order(&model, &jade, &path.display().to_string());
        checked += 1;
    }
    assert!(checked > 50, "only {checked} fixtures were checked");
}

#[test]
fn hello_round_trips_an_edit_through_export() {
    let (mut model, mut jade) = open("hello.docx");
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    };
    let mut next = 1_000_000u64;
    let mut ids = |n: u64| {
        let id = Id::new(next, ReplicaId(1));
        next += n;
        id
    };
    let op = jade_docx_collab::intent::resolve(
        &model,
        &jade_docx_collab::intent::IntentOp::InsertText {
            paragraph: 0,
            offset: 0,
            text: "Edited: ".into(),
        },
        &mut ids,
    )
    .unwrap()
    .remove(0);
    let effect = model.apply(&op, &ctx).unwrap();
    jade.materialize(&model, effect, &ctx).unwrap();
    assert_agree(&model, &jade, "after the edit");

    let bytes = jade.export().unwrap();
    let reloaded = JadeDocx::load(&bytes).unwrap();
    assert_eq!(reloaded.paragraph_texts(), model.paragraph_texts());
    assert!(model.paragraph_texts()[0].starts_with("Edited: "));
}

const EDIT_FIXTURES: &[&str] = &[
    "jade-core/hello.docx",
    "table-cell-spacing.docx",
    "footnote-table-textbox-order.docx",
    "table-complex-nested-merges.docx",
    "comment.docx",
    "footnote.docx",
    "footnote-long-continuation.docx",
    "endnote.docx",
    "textbox.docx",
    "floating-wrap-sides.docx",
];

/// Whether `style` shows `change` (for the properties jade's queries report).
fn shows(change: &FormatChange, style: &CharStyle) -> bool {
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

/// jade after a `Format` op: the change shows on exactly the characters the
/// model resolved (or on exactly the paragraphs the model touched), and
/// nothing else changed.
#[allow(clippy::too_many_arguments)]
fn assert_formatted(
    model_before: &DocxModel,
    op: &DocxOp,
    effects: &[DocxEffect],
    styles_before: &BTreeMap<Id, CharStyle>,
    styles_after: &BTreeMap<Id, CharStyle>,
    formats_before: &[(Option<String>, Option<String>)],
    formats_after: &[(Option<String>, Option<String>)],
    context: &str,
) {
    let DocxOp::Format {
        story,
        start,
        end,
        change,
    } = op
    else {
        return;
    };
    if change.is_paragraph_level() {
        let touched: BTreeSet<ParagraphStory> = effects
            .iter()
            .filter_map(|e| match e {
                DocxEffect::FormatParagraph { paragraph, .. } => Some(paragraph.clone()),
                _ => None,
            })
            .collect();
        let paragraphs = model_before.paragraphs();
        for (ordinal, paragraph) in paragraphs.iter().enumerate() {
            if touched.contains(&paragraph.at) {
                if let FormatChange::Alignment { alignment } = change {
                    // jade keeps `both` where it was already justified.
                    assert_eq!(
                        formats_after[ordinal]
                            .0
                            .as_deref()
                            .and_then(ParagraphAlignment::from_jade),
                        Some(*alignment),
                        "alignment of paragraph {ordinal} {context}"
                    );
                }
            } else {
                assert_eq!(
                    formats_after[ordinal], formats_before[ordinal],
                    "untouched paragraph {ordinal} changed {context}"
                );
                for c in &paragraph.chars {
                    assert_eq!(
                        styles_after[c], styles_before[c],
                        "a character of untouched paragraph {ordinal} changed {context}"
                    );
                }
            }
        }
        return;
    }
    let in_range: BTreeSet<Id> = model_before
        .range_chars(*story, *start, *end)
        .expect("an applied range resolves")
        .into_iter()
        .collect();
    for (c, after) in styles_after {
        if in_range.contains(c) {
            assert!(
                shows(change, after),
                "{c} in range lacks {change:?}: {after:?} {context}"
            );
        } else {
            assert_eq!(
                Some(after),
                styles_before.get(c),
                "{c} outside the range changed {context}"
            );
        }
    }
    assert_eq!(
        formats_after, formats_before,
        "paragraphs changed {context}"
    );
}

/// The text jade anchors comment `key` to, piece by piece.
fn anchored_text(jade: &JadeDocx, key: &str) -> Vec<String> {
    let comment = jade_comments(jade)
        .into_iter()
        .find(|c| c.id == key)
        .unwrap_or_else(|| panic!("no comment {key}"));
    comment
        .anchors
        .iter()
        .map(|a| paragraph_snapshot(jade, a.paragraph).text[a.start..a.end].to_string())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Random edit sequences — typing, deleting, splitting and joining in any
    /// story, notes and text boxes included, and around the text boxes and
    /// note references that place them — keep the model and jade in
    /// agreement after every single operation.
    #[test]
    fn random_edits_keep_model_and_jade_in_agreement(
        fixture_index in 0..EDIT_FIXTURES.len(),
        choices in prop::collection::vec(any::<u64>(), 1..25),
    ) {
        let name = EDIT_FIXTURES[fixture_index];
        let (mut model, mut jade) = open(name);
        assert_agree(&model, &jade, "on load");
        let mut next = 1_000_000u64;
        for (step, choice) in choices.into_iter().enumerate() {
            let mut ids = |n: u64| {
                let id = Id::new(next, ReplicaId(1));
                next += n;
                id
            };
            let Some(op) = generate(&model, choice, &mut ids) else { continue };
            let ctx = ApplyContext { origin: ReplicaId(1), base_seq: 0, seq: Some(step as u64 + 1) };
            // As the sequencer does: jade may refuse a sequenced op first.
            if DocxModel::lane(&op) == Lane::Sequenced && jade.admit(&model, &op).is_err() {
                continue;
            }
            let before = model.clone();
            let formatting = matches!(op, DocxOp::Format { .. });
            let styles_before = formatting.then(|| char_styles(&model, &jade));
            let formats_before = formatting.then(|| paragraph_formats(&model, &jade));
            match model.apply(&op, &ctx) {
                Ok(effect) => {
                    let context = format!("{name} after {op:?}");
                    jade.materialize(&model, effect.clone(), &ctx)
                        .unwrap_or_else(|e| panic!("{name}: jade refused {op:?}: {e}"));
                    assert_agree(&model, &jade, &context);
                    assert_comments_agree(&model, &jade, &context);
                    if let (Some(styles_before), Some(formats_before)) = (styles_before, formats_before) {
                        assert_formatted(
                            &before,
                            &op,
                            &effect,
                            &styles_before,
                            &char_styles(&model, &jade),
                            &formats_before,
                            &paragraph_formats(&model, &jade),
                            &context,
                        );
                    }
                    if let DocxOp::AddComment { story, start, end, id, .. } = &op {
                        // Within the comment's story jade anchors exactly the
                        // resolved characters.
                        let key = jade.comment_id(&model, *id).unwrap();
                        let text: String = before
                            .paragraphs()
                            .iter()
                            .filter(|p| p.story == *story)
                            .flat_map(|p| p.text.chars().zip(p.chars.clone()))
                            .filter(|(_, c)| before.range_chars(*story, *start, *end).unwrap().contains(c))
                            .map(|(ch, _)| ch)
                            .collect();
                        let story_surfaces: BTreeSet<usize> = model
                            .paragraphs()
                            .iter()
                            .filter(|p| p.story == *story)
                            .map(|p| surface(&jade, p))
                            .collect();
                        let comment = jade_comments(&jade).into_iter().find(|c| c.id == key).unwrap();
                        let anchored: String = comment
                            .anchors
                            .iter()
                            .filter(|a| story_surfaces.contains(&a.paragraph))
                            .map(|a| paragraph_snapshot(&jade, a.paragraph).text[a.start..a.end].to_string())
                            .collect();
                        prop_assert_eq!(anchored, text, "anchor of {} {}", key, context);
                    }
                }
                Err(_) => prop_assert_eq!(&model, &before, "a rejected op changed the model"),
            }
        }
        // And the file jade writes reads back as the same document, with its
        // notes and text boxes where the model has them.
        let reloaded = JadeDocx::load(&jade.export().unwrap()).unwrap();
        let reloaded_model = DocxModel::from_word_document(reloaded.word()).unwrap();
        assert_same_order(&reloaded_model, &reloaded, &format!("{name} reloaded"));
        assert_same_order(&model, &reloaded, &format!("{name} against the reloaded file"));
        prop_assert_eq!(anchor_layout(&reloaded_model), anchor_layout(&model));
        prop_assert_eq!(jade_comments(&reloaded).len(), model.comments().len());
    }
}

/// Fixtures whose tables have merged cells, and one without a table (a 3 × 3
/// table is inserted first).
const MERGE_FIXTURES: &[&str] = &[
    "jade-core/hello.docx",
    "merged-table-over-page.docx",
    "table-complex-merged-continuations.docx",
    "table-complex-staggered-merges.docx",
    "table-legacy-hmerge.docx",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Rows and columns inserted and deleted through merged cells — above,
    /// inside and below merges, with merges, unmerges and typing in between —
    /// keep the model and jade in agreement cell by cell after every
    /// operation, and the file jade writes reads back as the same tables.
    #[test]
    fn rows_and_columns_through_merges_agree_with_jade(
        fixture_index in 0..MERGE_FIXTURES.len(),
        choices in prop::collection::vec(any::<u64>(), 1..30),
    ) {
        let name = MERGE_FIXTURES[fixture_index];
        let (mut model, mut jade) = open(name);
        let mut next = 1_000_000u64;
        let mut ids = |n: u64| {
            let id = Id::new(next, ReplicaId(1));
            next += n;
            id
        };
        if model.tables().iter().all(|t| t.locked) {
            let first = ids(7);
            commit(&mut model, &mut jade, DocxOp::InsertTable {
                anchor: jade_docx_collab::Anchor::Start,
                id: first,
                rows: (1..=3).map(|k| first.offset(k)).collect(),
                columns: (4..=6).map(|k| first.offset(k)).collect(),
            });
        }
        assert_agree(&model, &jade, "on load");
        for (step, choice) in choices.into_iter().enumerate() {
            let Some(op) = generate_through_merges(&model, choice, &mut ids) else { continue };
            let ctx = ApplyContext { origin: ReplicaId(1), base_seq: 0, seq: Some(step as u64 + 1) };
            if DocxModel::lane(&op) == Lane::Sequenced && jade.admit(&model, &op).is_err() {
                continue;
            }
            let before = model.clone();
            match model.apply(&op, &ctx) {
                Ok(effect) => {
                    jade.materialize(&model, effect, &ctx)
                        .unwrap_or_else(|e| panic!("{name}: jade refused {op:?}: {e}"));
                    assert_agree(&model, &jade, &format!("{name} after {op:?}"));
                }
                Err(_) => prop_assert_eq!(&model, &before, "a rejected op changed the model"),
            }
        }
        let reloaded = JadeDocx::load(&jade.export().unwrap()).unwrap();
        let reloaded_model = DocxModel::from_word_document(reloaded.word()).unwrap();
        assert_tables_agree(&reloaded_model, &reloaded, &format!("{name} reloaded"));
        assert_tables_agree(&model, &reloaded, &format!("{name} against the reloaded file"));
    }
}

/// Table structure end to end in jade: a new table, rows and columns in and
/// out, text in a cell, and the table gone again.
#[test]
fn table_structure_is_realised_in_jade() {
    use jade_collab_core::grid::GridOp;
    use jade_docx_collab::{Anchor, DocxOp, StoryId};

    let (mut model, mut jade) = open("jade-core/hello.docx");
    let mut seq = 0;
    let mut next = 1_000_000u64;
    let mut id = || {
        next += 1;
        Id::new(next, ReplicaId(1))
    };
    let mut step = |model: &mut DocxModel, jade: &mut JadeDocx, op: DocxOp| {
        seq += 1;
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: 0,
            seq: Some(seq),
        };
        jade.admit(model, &op).unwrap();
        let effect = model
            .apply(&op, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        jade.materialize(model, effect, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        assert_agree(model, jade, &format!("after {op:?}"));
    };

    // hello.docx may be a single paragraph: make a mark to anchor after.
    let end = model.paragraphs()[0].chars.last().copied();
    let mark = id();
    step(
        &mut model,
        &mut jade,
        DocxOp::SplitParagraph {
            story: StoryId::Body,
            parent: end,
            id: mark,
        },
    );
    let (table, r1, r2, c1, c2) = (id(), id(), id(), id(), id());
    step(
        &mut model,
        &mut jade,
        DocxOp::InsertTable {
            anchor: Anchor::After(mark),
            id: table,
            rows: vec![r1, r2],
            columns: vec![c1, c2],
        },
    );
    let r3 = id();
    step(
        &mut model,
        &mut jade,
        DocxOp::Table {
            table,
            op: GridOp::InsertRow {
                id: r3,
                after: Some(r1),
            },
        },
    );
    let c3 = id();
    step(
        &mut model,
        &mut jade,
        DocxOp::Table {
            table,
            op: GridOp::InsertColumn {
                id: c3,
                after: None,
            },
        },
    );
    let first = id();
    step(
        &mut model,
        &mut jade,
        DocxOp::InsertText {
            story: StoryId::Cell {
                table,
                row: r3,
                column: c2,
            },
            parent: None,
            first,
            text: "cell".into(),
        },
    );
    assert_eq!(model.tables()[0].rows.len(), 3);
    assert_eq!(model.tables()[0].columns.len(), 3);
    step(
        &mut model,
        &mut jade,
        DocxOp::Table {
            table,
            op: GridOp::DeleteRow { id: r1 },
        },
    );
    step(
        &mut model,
        &mut jade,
        DocxOp::Table {
            table,
            op: GridOp::DeleteColumn { id: c1 },
        },
    );
    assert!(model.paragraph_texts().contains(&"cell".to_string()));
    step(&mut model, &mut jade, DocxOp::DeleteTable { table });
    assert!(model.tables().is_empty());
    let reloaded = JadeDocx::load(&jade.export().unwrap()).unwrap();
    assert_eq!(reloaded.paragraph_texts(), model.paragraph_texts());
}

/// The characters of `model` (in order) for which `pick` holds in jade.
fn marked(model: &DocxModel, jade: &JadeDocx, pick: impl Fn(&CharStyle) -> bool) -> String {
    let styles = char_styles(model, jade);
    model
        .paragraphs()
        .iter()
        .flat_map(|p| p.text.chars().zip(p.chars.clone()).collect::<Vec<_>>())
        .filter(|(_, id)| pick(&styles[id]))
        .map(|(c, _)| c)
        .collect()
}

#[test]
fn formatting_lands_on_exactly_the_resolved_characters() {
    let (mut model, mut jade) = open("jade-core/hello.docx");
    assert_eq!(model.paragraph_texts(), ["Hello\u{c}", "World"]);
    let p = model.paragraphs();
    let format = |start: Id, end: Id, change: FormatChange| DocxOp::Format {
        story: StoryId::Body,
        start,
        end,
        change,
    };

    // Across the paragraph boundary.
    commit(
        &mut model,
        &mut jade,
        format(
            p[0].chars[1],
            p[1].chars[2],
            FormatChange::Bold { value: true },
        ),
    );
    assert_eq!(
        marked(&model, &jade, |s| s.style.bold == Some(true)),
        "ello\u{c}Wor"
    );

    // A deletion that commits first shrinks the next range.
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: vec![p[0].chars[2], p[1].chars[1]],
        },
    );
    commit(
        &mut model,
        &mut jade,
        format(
            p[0].chars[2],
            p[1].chars[1],
            FormatChange::Italic { value: true },
        ),
    );
    assert_eq!(
        marked(&model, &jade, |s| s.style.italic == Some(true)),
        "lo\u{c}W"
    );

    for (change, expect) in [
        (
            FormatChange::FontSize { half_points: 21 },
            &(|s: &CharStyle| s.style.font_size == Some(10.5)) as &dyn Fn(&CharStyle) -> bool,
        ),
        (
            FormatChange::FontColor {
                color: "#C00000".into(),
            },
            &|s: &CharStyle| s.style.color.as_deref() == Some("#C00000"),
        ),
        (
            FormatChange::Highlight {
                color: "yellow".into(),
            },
            &|s: &CharStyle| s.highlight.as_deref() == Some("yellow"),
        ),
        (
            FormatChange::Underline {
                value: true,
                style: None,
            },
            &|s: &CharStyle| s.style.underline == Some(true),
        ),
        (
            FormatChange::FontFamily {
                family: "Arial".into(),
            },
            &|s: &CharStyle| s.style.font_family.as_deref() == Some("Arial"),
        ),
    ] {
        commit(
            &mut model,
            &mut jade,
            format(p[1].chars[2], p[1].chars[3], change.clone()),
        );
        assert_eq!(marked(&model, &jade, expect), "rl", "{change:?}");
    }

    // Clearing takes the direct formatting off again (Word keeps highlight).
    commit(
        &mut model,
        &mut jade,
        format(p[0].chars[0], p[1].chars[4], FormatChange::ClearFormatting),
    );
    assert_eq!(marked(&model, &jade, |s| s.style.bold == Some(true)), "");
    assert_eq!(
        marked(&model, &jade, |s| s.style.font_size == Some(10.5)),
        ""
    );

    // Paragraph alignment reaches the paragraphs the range touches.
    commit(
        &mut model,
        &mut jade,
        format(
            p[1].chars[0],
            p[1].chars[0],
            FormatChange::Alignment {
                alignment: ParagraphAlignment::Center,
            },
        ),
    );
    let formats = paragraph_formats(&model, &jade);
    assert_eq!(formats[0].0.as_deref(), Some("left"));
    assert_eq!(formats[1].0.as_deref(), Some("center"));
    commit(
        &mut model,
        &mut jade,
        format(
            p[0].chars[0],
            p[0].chars[0],
            FormatChange::ParagraphStyle {
                style_id: Some("Heading1".into()),
            },
        ),
    );
    assert_eq!(
        paragraph_formats(&model, &jade)[0].1.as_deref(),
        Some("Heading1")
    );

    // Everything survives export.
    let reloaded = JadeDocx::load(&jade.export().unwrap()).unwrap();
    let reloaded_model = DocxModel::from_word_document(reloaded.word()).unwrap();
    assert_eq!(
        marked(&reloaded_model, &reloaded, |s| s.highlight.as_deref()
            == Some("yellow")),
        "rl"
    );
    assert_eq!(
        paragraph_formats(&reloaded_model, &reloaded)[1]
            .0
            .as_deref(),
        Some("center")
    );
}

#[test]
fn comments_are_created_answered_resolved_and_deleted_in_jade() {
    let (mut model, mut jade) = open("jade-core/hello.docx");
    let p = model.paragraphs();
    let mut next = 1_000u64;
    let mut fresh = || {
        next += 1;
        Id::new(next, ReplicaId(1))
    };
    let author = |text: &str| CommentContent {
        text: text.into(),
        author: Some("Ann".into()),
        initials: Some("A".into()),
        date: None,
    };

    let root = fresh();
    commit(
        &mut model,
        &mut jade,
        DocxOp::AddComment {
            story: StoryId::Body,
            start: p[0].chars[1],
            end: p[0].chars[4],
            id: root,
            content: author("Hello?"),
        },
    );
    let key = jade.comment_id(&model, root).unwrap();
    assert_eq!(anchored_text(&jade, &key), ["ello"]);

    // Across paragraphs, after part of the range was deleted.
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: vec![p[1].chars[0]],
        },
    );
    let wide = fresh();
    commit(
        &mut model,
        &mut jade,
        DocxOp::AddComment {
            story: StoryId::Body,
            start: p[0].chars[3],
            end: p[1].chars[2],
            id: wide,
            content: author("both"),
        },
    );
    assert_eq!(
        anchored_text(&jade, &jade.comment_id(&model, wide).unwrap()).concat(),
        "lo\u{c}or"
    );

    let (r1, r2) = (fresh(), fresh());
    commit(
        &mut model,
        &mut jade,
        DocxOp::ReplyToComment {
            comment: root,
            id: r1,
            content: author("Yes."),
        },
    );
    // A reply to a reply joins the root's thread, in jade as in the model.
    commit(
        &mut model,
        &mut jade,
        DocxOp::ReplyToComment {
            comment: r1,
            id: r2,
            content: author("Sure?"),
        },
    );
    commit(
        &mut model,
        &mut jade,
        DocxOp::ResolveComment {
            comment: root,
            resolved: true,
        },
    );
    commit(&mut model, &mut jade, DocxOp::DeleteComment { comment: r1 });
    assert_eq!(jade_comments(&jade).len(), 3);
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteComment { comment: root },
    );
    assert_eq!(jade_comments(&jade).len(), 1, "the root took its replies");

    // jade reuses the freed ids; the model's ids stay distinct and map right.
    let late = fresh();
    commit(
        &mut model,
        &mut jade,
        DocxOp::AddComment {
            story: StoryId::Body,
            start: p[0].chars[0],
            end: p[0].chars[0],
            id: late,
            content: author("H"),
        },
    );
    assert_eq!(
        anchored_text(&jade, &jade.comment_id(&model, late).unwrap()),
        ["H"]
    );

    let reloaded = JadeDocx::load(&jade.export().unwrap()).unwrap();
    let texts: BTreeSet<String> = jade_comments(&reloaded)
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert_eq!(texts, BTreeSet::from(["both".to_string(), "H".to_string()]));
}

#[test]
fn comments_in_the_file_are_addressable_by_their_file_ids() {
    let (mut model, mut jade) = open("comment.docx");
    let loaded = model.comments();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].file_id.as_deref(), Some("0"));
    assert_eq!(loaded[1].parent, Some(loaded[0].id));
    assert_comments_agree(&model, &jade, "on load");

    let mut next = model.max_lamport();
    let mut ids = |n: u64| {
        let id = Id::new(next + 1, ReplicaId(1));
        next += n;
        id
    };
    for intent in [
        IntentOp::ReplyToComment {
            comment_id: "0".into(),
            text: "answered".into(),
            author: Some("Bob".into()),
            initials: None,
            date: None,
        },
        IntentOp::ResolveComment {
            comment_id: "1".into(),
            resolved: true,
        },
    ] {
        for op in resolve(&model, &intent, &mut ids).unwrap() {
            commit(&mut model, &mut jade, op);
        }
    }
    assert_eq!(jade_comments(&jade).len(), 3);
    for op in resolve(
        &model,
        &IntentOp::DeleteComment {
            comment_id: "0".into(),
        },
        &mut ids,
    )
    .unwrap()
    {
        commit(&mut model, &mut jade, op);
    }
    assert!(jade_comments(&jade).is_empty());
    assert!(model.comments().is_empty());
}

/// The model cannot know that jade re-anchors a reply on the root's text;
/// once that text is gone jade cannot place the reply, and `admit` refuses it
/// instead of letting the server fault.
#[test]
fn jade_refuses_a_reply_it_cannot_anchor() {
    let (mut model, mut jade) = open("jade-core/hello.docx");
    let p = model.paragraphs();
    let root = Id::new(1_000, ReplicaId(1));
    commit(
        &mut model,
        &mut jade,
        DocxOp::AddComment {
            story: StoryId::Body,
            start: p[0].chars[1],
            end: p[0].chars[2],
            id: root,
            content: CommentContent::new("el"),
        },
    );
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: vec![p[0].chars[1], p[0].chars[2]],
        },
    );
    let reply = DocxOp::ReplyToComment {
        comment: root,
        id: Id::new(1_001, ReplicaId(1)),
        content: CommentContent::new("too late"),
    };
    let before = jade_comments(&jade);
    assert!(jade.admit(&model, &reply).is_err());
    assert_eq!(jade_comments(&jade), before, "admit changed jade");
    // Resolving still works: it does not need an anchor.
    commit(
        &mut model,
        &mut jade,
        DocxOp::ResolveComment {
            comment: root,
            resolved: true,
        },
    );
}

/// Each paragraph's text with its anchors, as `text[anchor]text`, story by
/// kind — what must survive an export.
fn anchor_layout(model: &DocxModel) -> Vec<String> {
    model
        .paragraphs()
        .iter()
        .map(|p| {
            let mut out = String::new();
            let mut at = 0;
            for anchor in &p.anchors {
                out.push_str(&p.text[at..anchor.offset]);
                at = anchor.offset;
                out.push_str(&match anchor.story {
                    StoryId::TextBox { .. } => "[box]".to_string(),
                    story => format!("[{}]", model.note_id(story).unwrap()),
                });
            }
            out.push_str(&p.text[at..]);
            out
        })
        .collect()
}
