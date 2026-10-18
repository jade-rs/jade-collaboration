//! Text formatting in shapes. Character formatting is a convergent
//! Peritext mark over the shape's RGA (`jade-collab-core::marks`), exactly
//! as in docx: the newest mark of an attribute wins per character, typing at
//! a mark's end is covered and typing at its start is not. Paragraph
//! alignment is sequenced and resolved in log order.

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::marks::{Boundary, Expand};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_pptx_collab::{
    Alignment, CharFormat, FormatRun, PptxEffect, PptxModel, PptxOp, ShapeRef, StoryId,
};

fn at(origin: u64, base_seq: u64, seq: Option<u64>) -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(origin),
        base_seq,
        seq,
    }
}

fn id(n: u64, replica: u64) -> Id {
    Id::new(1000 + n, ReplicaId(replica))
}

fn deck() -> PptxModel {
    PptxModel::from_texts(&[&["Title", "one\ntwo\nthree"], &["Second"]])
}

fn shape_story(model: &PptxModel, slide: usize, shape: usize) -> StoryId {
    let s = &model.slides()[slide];
    StoryId::Shape {
        slide: s.id,
        shape: s.shapes[shape].id,
    }
}

fn chars(model: &PptxModel, story: StoryId) -> Vec<Id> {
    model
        .paragraphs(story)
        .unwrap()
        .iter()
        .flat_map(|p| p.chars.clone())
        .collect()
}

const BOLD: CharFormat = CharFormat::Bold { value: true };

/// A mark over characters `first..=last` (indices into the story's visible
/// characters) as the intent API makes it.
fn mark_over(
    model: &PptxModel,
    story: StoryId,
    first: usize,
    last: usize,
    mark: Id,
    format: CharFormat,
) -> PptxOp {
    let chars = chars(model, story);
    let (start, end) = model
        .mark_anchors(story, chars[first], chars[last], Expand::End)
        .unwrap();
    PptxOp::Mark {
        story,
        id: mark,
        start,
        end,
        format,
    }
}

/// Each visible character's formats, in story order.
fn formats(model: &PptxModel, story: StoryId) -> Vec<Vec<CharFormat>> {
    let all = model.char_formats(story).unwrap();
    chars(model, story)
        .iter()
        .map(|c| all.get(c).cloned().unwrap_or_default())
        .collect()
}

#[test]
fn marks_are_convergent_and_alignment_is_sequenced() {
    let model = deck();
    let story = shape_story(&model, 0, 0);
    let mark = mark_over(&model, story, 0, 1, id(1, 1), BOLD);
    assert_eq!(PptxModel::lane(&mark), Lane::Convergent);
    let align = PptxOp::AlignParagraphs {
        story,
        from: None,
        to: None,
        alignment: Alignment::Center,
    };
    assert_eq!(PptxModel::lane(&align), Lane::Sequenced);
}

#[test]
fn a_mark_formats_its_characters_and_has_no_effect_on_jade() {
    let mut model = deck();
    let story = shape_story(&model, 0, 0);
    let op = mark_over(&model, story, 1, 3, id(1, 1), BOLD);
    // jade keeps the base formatting; marks are realised on export.
    assert_eq!(model.apply(&op, &at(1, 0, None)).unwrap(), vec![]);
    let bold = vec![BOLD];
    assert_eq!(
        formats(&model, story),
        [vec![], bold.clone(), bold.clone(), bold.clone(), vec![]]
    );
    assert!(model.max_lamport() >= 1001);
}

/// Typing at a mark's end takes its formatting, typing at its start does
/// not — whichever arrives first.
#[test]
fn typing_at_the_edges_follows_the_expand_rule() {
    let base = deck();
    let story = shape_story(&base, 0, 0);
    let c = chars(&base, story);
    let mark = mark_over(&base, story, 1, 2, id(1, 1), BOLD);
    let after = PptxOp::InsertText {
        story,
        parent: Some(c[2]),
        first: id(5, 2),
        text: "x".into(),
    };
    let before = PptxOp::InsertText {
        story,
        parent: Some(c[0]),
        first: id(7, 2),
        text: "y".into(),
    };
    let mut one = base.clone();
    for op in [&mark, &after, &before] {
        one.apply(op, &at(1, 0, None)).unwrap();
    }
    let mut two = base.clone();
    for op in [&before, &after, &mark] {
        two.apply(op, &at(1, 0, None)).unwrap();
    }
    assert_eq!(one, two);
    assert_eq!(one.text(story).as_deref(), Some("Tyitxle"));
    let bold = vec![BOLD];
    assert_eq!(
        formats(&one, story),
        [
            vec![],
            vec![],
            bold.clone(),
            bold.clone(),
            bold.clone(),
            vec![],
            vec![]
        ]
    );
}

#[test]
fn the_newest_mark_wins_per_attribute_and_attributes_are_independent() {
    let base = deck();
    let story = shape_story(&base, 0, 0);
    let red = CharFormat::Color {
        color: "#FF0000".into(),
    };
    let not_bold = CharFormat::Bold { value: false };
    let ops = [
        mark_over(&base, story, 0, 4, id(1, 1), BOLD),
        mark_over(&base, story, 2, 3, id(1, 2), not_bold.clone()),
        mark_over(&base, story, 3, 4, id(2, 1), red.clone()),
    ];
    let mut forward = base.clone();
    for op in &ops {
        forward.apply(op, &at(1, 0, None)).unwrap();
    }
    let mut backward = base.clone();
    for op in ops.iter().rev() {
        backward.apply(op, &at(1, 0, None)).unwrap();
    }
    assert_eq!(forward, backward);
    assert_eq!(
        formats(&forward, story),
        [
            vec![BOLD],
            vec![BOLD],
            vec![not_bold.clone()],
            vec![not_bold, red.clone()],
            vec![BOLD, red],
        ]
    );
}

#[test]
fn marks_jade_cannot_write_are_refused() {
    let mut model = deck();
    let story = shape_story(&model, 0, 0);
    let before = model.clone();
    let bad = [
        CharFormat::Size { hundredths: 99 },
        CharFormat::Size { hundredths: 400_001 },
        CharFormat::Color { color: "red".into() },
        CharFormat::Color {
            color: "#12345G".into(),
        },
        CharFormat::Font {
            typeface: " ".into(),
        },
        CharFormat::EastAsianFont {
            typeface: "a\u{1}".into(),
        },
    ];
    for (k, format) in bad.into_iter().enumerate() {
        let op = mark_over(&model, story, 0, 1, id(k as u64, 1), format);
        assert!(model.apply(&op, &at(1, 0, None)).is_err(), "{op:?}");
    }
    let good = [
        CharFormat::Size { hundredths: 100 },
        CharFormat::Size { hundredths: 400_000 },
        CharFormat::Italic { value: true },
        CharFormat::Underline { value: true },
        CharFormat::Font {
            typeface: "Arial".into(),
        },
        CharFormat::EastAsianFont {
            typeface: "Yu Gothic".into(),
        },
        CharFormat::Color {
            color: "#00ff7F".into(),
        },
    ];
    let mut accepted = before.clone();
    for (k, format) in good.into_iter().enumerate() {
        let op = mark_over(&accepted, story, 0, 1, id(10 + k as u64, 1), format);
        accepted.apply(&op, &at(1, 0, None)).unwrap();
    }
    assert_eq!(model, before, "refusals change nothing");

    // Anchors the story does not have, nothing covered, a reused id, and
    // speaker notes (jade formats shapes only).
    let c = chars(&model, story);
    let missing = PptxOp::Mark {
        story,
        id: id(30, 1),
        start: Boundary::Before(id(999, 9)),
        end: Boundary::End,
        format: BOLD,
    };
    let empty = PptxOp::Mark {
        story,
        id: id(31, 1),
        start: Boundary::Before(c[2]),
        end: Boundary::Before(c[2]),
        format: BOLD,
    };
    let notes = PptxOp::Mark {
        story: StoryId::Notes {
            slide: model.slides()[0].id,
        },
        id: id(32, 1),
        start: Boundary::Start,
        end: Boundary::End,
        format: BOLD,
    };
    for op in [missing, empty, notes] {
        assert!(model.apply(&op, &at(1, 0, None)).is_err(), "{op:?}");
    }
    let first = mark_over(&model, story, 0, 0, id(40, 1), BOLD);
    model.apply(&first, &at(1, 0, None)).unwrap();
    let reused = mark_over(&model, story, 1, 1, id(40, 1), BOLD);
    assert!(model.apply(&reused, &at(1, 0, None)).is_err());
}

/// A copy keeps its source's marks; a mark whose author had not seen the
/// duplication formats the copy too.
#[test]
fn duplication_copies_marks_and_unseen_marks_reach_the_copy() {
    let mut model = deck();
    let story = shape_story(&model, 0, 0);
    let slide = model.slides()[0].id;
    model
        .apply(
            &mark_over(&model, story, 0, 0, id(1, 1), BOLD),
            &at(1, 0, Some(1)),
        )
        .unwrap();
    model
        .apply(
            &PptxOp::DuplicateSlide {
                slide,
                id: id(10, 1),
            },
            &at(1, 1, Some(2)),
        )
        .unwrap();
    let copy = StoryId::Shape {
        slide: id(10, 1),
        shape: model.slides()[1].shapes[0].id,
    };
    assert_eq!(formats(&model, copy)[0], vec![BOLD]);
    let italic = CharFormat::Italic { value: true };
    let unseen = mark_over(&model, story, 1, 1, id(20, 2), italic.clone());
    model.apply(&unseen, &at(2, 1, Some(3))).unwrap();
    assert_eq!(formats(&model, copy)[1], vec![italic.clone()]);
    let seen = mark_over(&model, story, 2, 2, id(21, 2), italic.clone());
    model.apply(&seen, &at(2, 2, Some(4))).unwrap();
    assert_eq!(formats(&model, story)[2], vec![italic]);
    assert_eq!(formats(&model, copy)[2], vec![]);
}

/// What the materializer writes: maximal runs of one value, per paragraph
/// and attribute, by byte offsets of the model's text, on visible slides
/// and shapes only.
#[test]
fn formatting_lists_runs_per_paragraph_and_attribute() {
    let mut model = deck();
    let story = shape_story(&model, 0, 1); // "one\ntwo\nthree"
    let slide = model.slides()[0].clone();
    // "ne", "two", "th": across two paragraph marks.
    let op = mark_over(&model, story, 1, 7, id(1, 1), BOLD);
    model.apply(&op, &at(1, 0, None)).unwrap();
    let size = CharFormat::Size { hundredths: 2400 };
    let op = mark_over(&model, story, 0, 0, id(2, 1), size.clone());
    model.apply(&op, &at(1, 0, None)).unwrap();
    let shape = ShapeRef {
        slide: slide.id,
        shape: slide.shapes[1].id,
        number: slide.shapes[1].number,
    };
    let run = |paragraph, start, end, format: &CharFormat| FormatRun {
        slide: 0,
        shape,
        paragraph,
        start,
        end,
        format: format.clone(),
    };
    assert_eq!(
        model.formatting(),
        [
            run(0, 1, 3, &BOLD),
            run(0, 0, 1, &size),
            run(1, 0, 3, &BOLD),
            run(2, 0, 2, &BOLD),
        ]
    );
    // A deleted shape's marks are not written.
    model
        .apply(
            &PptxOp::DeleteShape {
                slide: slide.id,
                shape: shape.shape,
            },
            &at(1, 0, Some(1)),
        )
        .unwrap();
    assert_eq!(model.formatting(), []);
}

#[test]
fn marks_round_trip_through_json() {
    let mut model = deck();
    let story = shape_story(&model, 0, 0);
    let op = mark_over(&model, story, 0, 2, id(1, 1), BOLD);
    model.apply(&op, &at(1, 0, None)).unwrap();
    let json = serde_json::to_string(&model).unwrap();
    assert_eq!(serde_json::from_str::<PptxModel>(&json).unwrap(), model);
}

fn marks(model: &PptxModel, story: StoryId) -> Vec<Id> {
    model
        .paragraphs(story)
        .unwrap()
        .iter()
        .filter_map(|p| p.mark)
        .collect()
}

/// Alignment names paragraphs by the atom they begin after (`None`: the
/// first), so every paragraph — an empty last one included — can be named,
/// and resolves in log order: one effect per paragraph.
#[test]
fn alignment_resolves_paragraphs_in_log_order() {
    let mut model = deck();
    let story = shape_story(&model, 0, 1); // "one\ntwo\nthree"
    let slide = model.slides()[0].clone();
    let shape = ShapeRef {
        slide: slide.id,
        shape: slide.shapes[1].id,
        number: slide.shapes[1].number,
    };
    let [first_mark, second_mark] = marks(&model, story)[..] else {
        panic!()
    };
    let align = |from, to, alignment| PptxOp::AlignParagraphs {
        story,
        from,
        to,
        alignment,
    };
    let effect = |paragraph, alignment| PptxEffect::AlignParagraph {
        slide: 0,
        shape,
        paragraph,
        alignment,
    };
    assert_eq!(
        model
            .apply(
                &align(Some(first_mark), Some(second_mark), Alignment::Right),
                &at(1, 0, Some(1))
            )
            .unwrap(),
        [effect(1, Alignment::Right), effect(2, Alignment::Right)]
    );
    // A split committed before it lands inside the range: covered.
    let c = chars(&model, story);
    model
        .apply(
            &PptxOp::SplitParagraph {
                story,
                parent: Some(c[3]),
                id: id(1, 2),
            },
            &at(2, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(model.text(story).as_deref(), Some("one\nt\nwo\nthree"));
    assert_eq!(
        model
            .apply(
                &align(None, Some(second_mark), Alignment::Center),
                &at(1, 0, Some(3))
            )
            .unwrap(),
        [
            effect(0, Alignment::Center),
            effect(1, Alignment::Center),
            effect(2, Alignment::Center),
            effect(3, Alignment::Center)
        ]
    );
    // Joined: the paragraph that began after the joined mark is now part
    // of the one before.
    model
        .apply(
            &PptxOp::JoinParagraph {
                story,
                mark: first_mark,
            },
            &at(2, 0, Some(4)),
        )
        .unwrap();
    assert_eq!(model.text(story).as_deref(), Some("onet\nwo\nthree"));
    assert_eq!(
        model
            .apply(
                &align(Some(first_mark), Some(first_mark), Alignment::Justify),
                &at(1, 0, Some(5))
            )
            .unwrap(),
        [effect(0, Alignment::Justify)]
    );
    // Reversed ranges do nothing; unknown atoms and notes are refused.
    assert_eq!(
        model
            .apply(
                &align(Some(second_mark), None, Alignment::Left),
                &at(1, 0, Some(6))
            )
            .unwrap(),
        []
    );
    assert!(model
        .apply(
            &align(Some(id(99, 9)), None, Alignment::Left),
            &at(1, 0, Some(7))
        )
        .is_err());
    let notes = PptxOp::AlignParagraphs {
        story: StoryId::Notes { slide: slide.id },
        from: None,
        to: None,
        alignment: Alignment::Left,
    };
    assert!(model.apply(&notes, &at(1, 0, Some(8))).is_err());
}

/// An alignment whose author had not seen a duplication aligns the copy
/// too, like any other text edit.
#[test]
fn alignment_reaches_unseen_copies() {
    let mut model = deck();
    let story = shape_story(&model, 0, 1);
    let slide = model.slides()[0].id;
    model
        .apply(
            &PptxOp::DuplicateSlide {
                slide,
                id: id(10, 1),
            },
            &at(1, 0, Some(1)),
        )
        .unwrap();
    let effects = model
        .apply(
            &PptxOp::AlignParagraphs {
                story,
                from: None,
                to: None,
                alignment: Alignment::Distributed,
            },
            &at(2, 0, Some(2)),
        )
        .unwrap();
    let slides: Vec<usize> = effects
        .iter()
        .map(|e| match e {
            PptxEffect::AlignParagraph { slide, .. } => *slide,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(slides, [0, 1]);
}
