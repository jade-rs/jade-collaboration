//! Character formatting on the convergent lane: a `Mark` names its range by
//! the sides of characters (Peritext), so it commutes with every text edit
//! and with every other mark; the model stores the marks and computes what
//! each character shows. Characters no mark covers keep the formatting jade
//! has for them (the file's, or jade's rules for typed text).

use std::collections::BTreeMap;

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::marks::{Boundary, Expand};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{
    DocxModel, DocxOp, FormatChange, MarkKey, ParagraphStory, StoryId, TextFormat,
};
use proptest::prelude::*;

fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: None,
    }
}

fn apply(model: &mut DocxModel, op: &DocxOp) {
    model
        .apply(op, &ctx())
        .unwrap_or_else(|e| panic!("{op:?}: {e}"));
}

fn bold() -> FormatChange {
    FormatChange::Bold { value: true }
}

fn italic() -> FormatChange {
    FormatChange::Italic { value: true }
}

/// Resolves and applies an intent with a clock ahead of the model.
fn intent(model: &mut DocxModel, op: IntentOp, replica: u64) -> Vec<DocxOp> {
    let mut next = model.max_lamport() + 1;
    let ops = resolve(model, &op, &mut |n| {
        let id = Id::new(next, ReplicaId(replica));
        next += n;
        id
    })
    .unwrap();
    for op in &ops {
        apply(model, op);
    }
    ops
}

fn format(sp: usize, so: usize, ep: usize, eo: usize, change: FormatChange) -> IntentOp {
    IntentOp::Format {
        start_paragraph: sp,
        start_offset: so,
        end_paragraph: ep,
        end_offset: eo,
        change,
    }
}

fn type_at(paragraph: usize, offset: usize, text: &str) -> IntentOp {
    IntentOp::InsertText {
        paragraph,
        offset,
        text: text.into(),
    }
}

/// Each paragraph's text with the characters that show `change` in upper
/// case (and everything else in lower case).
fn shown(model: &DocxModel, change: &FormatChange) -> Vec<String> {
    let formats = model.character_formats();
    model
        .paragraphs()
        .iter()
        .map(|p| {
            p.text
                .chars()
                .zip(&p.chars)
                .map(|(c, id)| {
                    if formats.get(id).is_some_and(|f| f.contains(change)) {
                        c.to_ascii_uppercase()
                    } else {
                        c
                    }
                })
                .collect()
        })
        .collect()
}

#[test]
fn character_formatting_resolves_to_a_convergent_mark() {
    let mut model = DocxModel::from_paragraphs(&["hello world"]);
    let p = model.paragraphs();
    let ops = intent(&mut model, format(0, 0, 0, 5, bold()), 1);
    assert_eq!(ops.len(), 1);
    let DocxOp::Mark {
        story,
        start,
        end,
        change,
        ..
    } = &ops[0]
    else {
        panic!("{ops:?}");
    };
    assert_eq!(*story, StoryId::Body);
    assert_eq!(*start, Boundary::Before(p[0].chars[0]));
    // Bold expands: the mark ends before the character after the range.
    assert_eq!(*end, Boundary::Before(p[0].chars[5]));
    assert_eq!(*change, bold());
    assert_eq!(DocxModel::lane(&ops[0]), Lane::Convergent);
    assert_eq!(shown(&model, &bold()), ["HELLO world"]);
    assert_eq!(
        model.formatting(),
        vec![TextFormat {
            paragraph: ParagraphStory::Body { index: 0 },
            start: 0,
            end: 5,
            change: bold(),
        }]
    );
}

#[test]
fn typing_at_the_end_of_bold_text_is_bold_and_before_it_is_not() {
    let mut model = DocxModel::from_paragraphs(&["hello world", "next"]);
    intent(&mut model, format(0, 6, 0, 11, bold()), 1);
    // At the paragraph's end: the mark ends before the paragraph mark.
    intent(&mut model, type_at(0, 11, "!"), 2);
    // Right before the bold word.
    intent(&mut model, type_at(0, 6, "_"), 2);
    // Inside it.
    intent(&mut model, type_at(0, 9, "r"), 2);
    assert_eq!(shown(&model, &bold()), ["hello _WORRLD!", "next"]);
}

#[test]
fn every_simple_attribute_expands_at_its_end() {
    for key in [
        MarkKey::Bold,
        MarkKey::Italic,
        MarkKey::Underline,
        MarkKey::Strike,
        MarkKey::FontSize,
        MarkKey::FontFamily,
        MarkKey::FontColor,
        MarkKey::Highlight,
    ] {
        assert_eq!(key.expand(), Expand::End, "{key:?}");
    }
    assert_eq!(bold().mark_key(), Some(MarkKey::Bold));
    assert_eq!(
        FormatChange::Highlight {
            color: "yellow".into()
        }
        .mark_key(),
        Some(MarkKey::Highlight)
    );
    assert_eq!(FormatChange::ClearFormatting.mark_key(), None);
}

#[test]
fn concurrent_marks_and_typing_commute() {
    let model = DocxModel::from_paragraphs(&["abcdef"]);
    let mut a = model.clone();
    let mut b = model.clone();
    let bold_ops = intent(&mut a, format(0, 1, 0, 3, bold()), 1);
    let italic_ops = intent(&mut b, format(0, 2, 0, 5, italic()), 2);
    let typed = intent(&mut b, type_at(0, 3, "XY"), 2);
    for op in italic_ops.iter().chain(&typed) {
        apply(&mut a, op);
    }
    for op in &bold_ops {
        apply(&mut b, op);
    }
    assert_eq!(a, b);
    assert_eq!(a.character_formats(), b.character_formats());
    // "XY" was typed right after "c": inside the italic range and right at
    // the end of the bold one, which expands.
    assert_eq!(shown(&a, &bold()), ["aBCXYdef"]);
    assert_eq!(shown(&a, &italic()), ["abCXYDEf"]);
}

#[test]
fn the_newest_mark_wins_per_attribute() {
    let mut model = DocxModel::from_paragraphs(&["abcdef"]);
    intent(&mut model, format(0, 0, 0, 6, bold()), 1);
    intent(
        &mut model,
        format(0, 2, 0, 4, FormatChange::Bold { value: false }),
        2,
    );
    intent(
        &mut model,
        format(0, 0, 0, 6, FormatChange::FontSize { half_points: 40 }),
        1,
    );
    let formats = model.character_formats();
    let c = &model.paragraphs()[0].chars;
    assert_eq!(
        formats[&c[0]],
        vec![bold(), FormatChange::FontSize { half_points: 40 }]
    );
    assert_eq!(
        formats[&c[2]],
        vec![
            FormatChange::Bold { value: false },
            FormatChange::FontSize { half_points: 40 }
        ]
    );
    // A concurrent older mark arriving later loses where they overlap.
    let p = model.paragraphs();
    apply(
        &mut model,
        &DocxOp::Mark {
            story: StoryId::Body,
            id: Id::new(1, ReplicaId(9)),
            start: Boundary::Before(p[0].chars[0]),
            end: Boundary::End,
            change: FormatChange::Bold { value: false },
        },
    );
    assert_eq!(model.character_formats()[&c[0]][0], bold());
}

#[test]
fn clearing_formatting_unsets_older_marks_but_not_newer_ones_or_the_highlight() {
    let mut model = DocxModel::from_paragraphs(&["abcdef"]);
    intent(&mut model, format(0, 0, 0, 6, bold()), 1);
    let yellow = FormatChange::Highlight {
        color: "yellow".into(),
    };
    intent(&mut model, format(0, 0, 0, 6, yellow.clone()), 1);
    let ops = intent(
        &mut model,
        format(0, 1, 0, 5, FormatChange::ClearFormatting),
        2,
    );
    // A convergent mark that unsets the session's formatting, and the
    // sequenced clear of the file's own formatting.
    assert_eq!(ops.len(), 2, "{ops:?}");
    assert!(matches!(
        &ops[0],
        DocxOp::Mark {
            change: FormatChange::ClearFormatting,
            ..
        }
    ));
    assert_eq!(DocxModel::lane(&ops[0]), Lane::Convergent);
    assert!(matches!(
        &ops[1],
        DocxOp::Format {
            change: FormatChange::ClearFormatting,
            ..
        }
    ));
    assert_eq!(DocxModel::lane(&ops[1]), Lane::Sequenced);
    assert_eq!(shown(&model, &bold()), ["AbcdeF"]);
    assert_eq!(shown(&model, &yellow), ["ABCDEF"]);
    // Typing at the end of the cleared range is not bold either.
    intent(&mut model, type_at(0, 5, "z"), 3);
    assert_eq!(shown(&model, &bold()), ["AbcdezF"]);
    // A newer bold wins over the clear.
    intent(&mut model, format(0, 2, 0, 3, bold()), 3);
    assert_eq!(shown(&model, &bold()), ["AbCdezF"]);
    let c = model.paragraphs()[0].chars.clone();
    assert_eq!(model.character_formats()[&c[1]], vec![yellow]);
}

#[test]
fn deleted_text_keeps_anchoring_marks() {
    let mut model = DocxModel::from_paragraphs(&["abcdef"]);
    intent(&mut model, format(0, 1, 0, 3, bold()), 1);
    // Another replica deletes "bc" and "d" (the mark's end anchor), then
    // types where they were.
    intent(
        &mut model,
        IntentOp::ReplaceRange {
            start_paragraph: 0,
            start_offset: 1,
            end_paragraph: 0,
            end_offset: 4,
            text: "XY".into(),
        },
        2,
    );
    assert_eq!(model.paragraph_texts(), ["aXYef"]);
    // The caret is after "a": before the tombstones, so outside the mark.
    assert_eq!(shown(&model, &bold()), ["aXYef"]);
}

#[test]
fn a_range_across_paragraphs_formats_each_paragraph_and_splits_inside_it_stay_formatted() {
    let mut model = DocxModel::from_paragraphs(&["abc", "def"]);
    intent(&mut model, format(0, 1, 1, 2, italic()), 1);
    assert_eq!(shown(&model, &italic()), ["aBC", "DEf"]);
    // A paragraph split inside the range keeps both halves italic, and
    // text typed in the new paragraph is inside the mark.
    intent(
        &mut model,
        IntentOp::InsertParagraphBreak {
            paragraph: 0,
            offset: 2,
            count: 1,
        },
        2,
    );
    intent(&mut model, type_at(1, 0, "x"), 2);
    assert_eq!(shown(&model, &italic()), ["aB", "XC", "DEf"]);
    assert_eq!(
        model.formatting(),
        vec![
            TextFormat {
                paragraph: ParagraphStory::Body { index: 0 },
                start: 1,
                end: 2,
                change: italic(),
            },
            TextFormat {
                paragraph: ParagraphStory::Body { index: 1 },
                start: 0,
                end: 2,
                change: italic(),
            },
            TextFormat {
                paragraph: ParagraphStory::Body { index: 2 },
                start: 0,
                end: 2,
                change: italic(),
            },
        ]
    );
}

#[test]
fn adjacent_marks_with_the_same_value_make_one_run() {
    let mut model = DocxModel::from_paragraphs(&["abcdef"]);
    intent(&mut model, format(0, 0, 0, 3, bold()), 1);
    intent(&mut model, format(0, 3, 0, 6, bold()), 2);
    assert_eq!(
        model.formatting(),
        vec![TextFormat {
            paragraph: ParagraphStory::Body { index: 0 },
            start: 0,
            end: 6,
            change: bold(),
        }]
    );
}

#[test]
fn marks_are_validated_alike_everywhere() {
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let c = model.paragraphs()[0].chars.clone();
    let before = model.clone();
    let mark = |start, end, change| DocxOp::Mark {
        story: StoryId::Body,
        id: Id::new(100, ReplicaId(1)),
        start,
        end,
        change,
    };
    for op in [
        // Paragraph formatting has no mark form.
        mark(
            Boundary::Before(c[0]),
            Boundary::End,
            FormatChange::Alignment {
                alignment: jade_docx_collab::ParagraphAlignment::Center,
            },
        ),
        // Values Word cannot read.
        mark(
            Boundary::Before(c[0]),
            Boundary::End,
            FormatChange::FontColor {
                color: "red".into(),
            },
        ),
        // An anchor the story does not have.
        mark(
            Boundary::Before(Id::new(999, ReplicaId(4))),
            Boundary::End,
            bold(),
        ),
        // Nothing between the boundaries.
        mark(Boundary::After(c[2]), Boundary::Before(c[0]), bold()),
        // A story that does not exist.
        DocxOp::Mark {
            story: StoryId::TextBox {
                id: Id::new(5, ReplicaId(5)),
            },
            id: Id::new(100, ReplicaId(1)),
            start: Boundary::Start,
            end: Boundary::End,
            change: bold(),
        },
    ] {
        assert!(model.apply(&op, &ctx()).is_err(), "{op:?} was accepted");
    }
    assert_eq!(model, before);
    let ok = mark(Boundary::Before(c[0]), Boundary::End, bold());
    apply(&mut model, &ok);
    assert!(
        model.apply(&ok, &ctx()).is_err(),
        "a mark id is used once"
    );
    // The mark's id moves the model's clock past it.
    assert!(model.max_lamport() >= 100);
}

#[test]
fn marks_travel_and_persist_as_json() {
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let ops = intent(&mut model, format(0, 0, 0, 2, bold()), 1);
    let json = serde_json::to_string(&ops[0]).unwrap();
    assert!(json.contains(r#""type":"mark""#), "{json}");
    assert!(json.contains(r#""side":"before""#), "{json}");
    let back: DocxOp = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ops[0]);
    let saved = serde_json::to_string(&model).unwrap();
    let restored: DocxModel = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored, model);
    assert_eq!(restored.character_formats(), model.character_formats());
}

// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Step {
    Edit { replica: usize, choice: u64 },
    Sync { from: usize, to: usize },
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            4 => (0..3usize, any::<u64>()).prop_map(|(replica, choice)| Step::Edit { replica, choice }),
            2 => (0..3usize, 0..3usize).prop_map(|(from, to)| Step::Sync { from, to }),
        ],
        0..50,
    )
}

const CHANGES: [fn() -> FormatChange; 5] = [
    || FormatChange::Bold { value: true },
    || FormatChange::Bold { value: false },
    || FormatChange::Italic { value: true },
    || FormatChange::FontSize { half_points: 30 },
    || FormatChange::ClearFormatting,
];

/// A convergent edit (typing, deleting, splitting, or a mark) at a random
/// place of `view`.
fn edit(view: &DocxModel, choice: u64, replica: u64) -> Vec<DocxOp> {
    let paragraphs = view.paragraph_texts();
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let p = pick(paragraphs.len(), 8);
    let q = (p + pick(2, 12)).min(paragraphs.len() - 1);
    let boundaries = |t: &str| -> Vec<usize> { t.char_indices().map(|(i, _)| i).chain([t.len()]).collect() };
    let (bp, bq) = (boundaries(&paragraphs[p]), boundaries(&paragraphs[q]));
    let (so, eo) = (bp[pick(bp.len(), 16)], bq[pick(bq.len(), 24)]);
    let op = match choice % 6 {
        0 | 1 => type_at(p, so, ["x", "日本", "ab"][pick(3, 32)]),
        2 => IntentOp::DeleteText {
            paragraph: p,
            offset: so,
            direction: Default::default(),
            count: 1 + pick(3, 32),
        },
        3 => IntentOp::InsertParagraphBreak {
            paragraph: p,
            offset: so,
            count: 1,
        },
        _ => format(p, so, q, eo, CHANGES[pick(CHANGES.len(), 40)]()),
    };
    let mut next = view.max_lamport() + 1;
    let Ok(ops) = resolve(view, &op, &mut |n| {
        let id = Id::new(next, ReplicaId(replica));
        next += n;
        id
    }) else {
        return Vec::new();
    };
    // Only the convergent part (joins and the sequenced half of a clear
    // need the server).
    ops.into_iter()
        .filter(|op| DocxModel::lane(op) == Lane::Convergent)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Replicas exchanging typing, deletions, splits and marks peer to peer,
    /// in any causal order, end with the same text and the same formatting
    /// on every character — no server needed.
    #[test]
    fn replicas_converge_on_text_and_formatting(steps in steps()) {
        let start = DocxModel::from_paragraphs(&["Hello world", "second", ""]);
        let mut log: Vec<DocxOp> = Vec::new();
        let mut replicas: Vec<(DocxModel, Vec<usize>)> = vec![(start.clone(), Vec::new()); 3];
        for step in &steps {
            match *step {
                Step::Edit { replica, choice } => {
                    let ops = edit(&replicas[replica].0, choice, replica as u64 + 1);
                    for op in ops {
                        apply(&mut replicas[replica].0, &op);
                        log.push(op);
                        replicas[replica].1.push(log.len() - 1);
                    }
                }
                Step::Sync { from, to } => {
                    let theirs = replicas[from].1.clone();
                    for index in theirs {
                        if !replicas[to].1.contains(&index) {
                            apply(&mut replicas[to].0, &log[index]);
                            replicas[to].1.push(index);
                        }
                    }
                }
            }
        }
        for replica in &mut replicas {
            for (index, op) in log.iter().enumerate() {
                if !replica.1.contains(&index) {
                    apply(&mut replica.0, op);
                    replica.1.push(index);
                }
            }
        }
        let formats: Vec<BTreeMap<Id, Vec<FormatChange>>> =
            replicas.iter().map(|r| r.0.character_formats()).collect();
        for (replica, f) in replicas.iter().zip(&formats) {
            prop_assert_eq!(replica.0.paragraph_texts(), replicas[0].0.paragraph_texts());
            prop_assert_eq!(f, &formats[0]);
            prop_assert_eq!(replica.0.formatting(), replicas[0].0.formatting());
        }
    }
}
