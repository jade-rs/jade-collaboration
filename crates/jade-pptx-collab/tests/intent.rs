//! The index-addressed intent API: what an agent or the CLI sends — slide
//! numbers from 1, shape ids as the file states them, paragraph indices and
//! UTF-8 byte offsets — resolved against a replica's view into
//! identifier-addressed operations.

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_pptx_collab::intent::{resolve, Direction, IntentError, PptxIntent, Target};
use jade_pptx_collab::{PptxModel, PptxOp};

fn run(model: &mut PptxModel, intent: PptxIntent) -> Result<Vec<PptxOp>, IntentError> {
    // Like a replica's clock: always ahead of everything in the view.
    let mut next = model.max_lamport() + 1;
    let ops = resolve(model, &intent, &mut |n| {
        let id = Id::new(next, ReplicaId(7));
        next += n;
        id
    })?;
    let ctx = ApplyContext {
        origin: ReplicaId(7),
        base_seq: 0,
        seq: None,
    };
    for op in &ops {
        model
            .apply(op, &ctx)
            .unwrap_or_else(|e| panic!("{op:?} does not apply to its own view: {e}"));
    }
    Ok(ops)
}

fn texts(model: &PptxModel) -> Vec<Vec<String>> {
    model
        .slides()
        .iter()
        .map(|s| {
            s.shapes
                .iter()
                .map(|shape| shape.text.clone().unwrap_or_default())
                .collect()
        })
        .collect()
}

fn insert(slide: usize, target: Target, paragraph: usize, offset: usize, text: &str) -> PptxIntent {
    PptxIntent::InsertText {
        slide,
        target,
        paragraph,
        offset,
        text: text.into(),
    }
}

#[test]
fn insert_text_by_slide_number_shape_id_and_byte_offset() {
    let mut model = PptxModel::from_texts(&[&["Hello"], &["日本", "x\ny"]]);
    run(&mut model, insert(1, Target::Shape(2), 0, 5, "!")).unwrap();
    run(&mut model, insert(2, Target::Shape(2), 0, 3, "-")).unwrap();
    run(&mut model, insert(2, Target::Shape(3), 1, 0, "<")).unwrap();
    assert_eq!(texts(&model), [vec!["Hello!"], vec!["日-本", "x\n<y"]]);
}

#[test]
fn newlines_in_inserted_text_split_paragraphs() {
    let mut model = PptxModel::from_texts(&[&["ab"]]);
    let ops = run(&mut model, insert(1, Target::Shape(2), 0, 1, "1\n2\n")).unwrap();
    assert_eq!(texts(&model), [vec!["a1\n2\nb"]]);
    assert_eq!(
        ops.iter()
            .filter(|op| matches!(op, PptxOp::SplitParagraph { .. }))
            .count(),
        2
    );
}

#[test]
fn rejects_what_the_view_does_not_have() {
    let model = PptxModel::from_texts(&[&["日本"]]);
    let mut ids = |_| Id::new(1, ReplicaId(7));
    let cases = [
        (
            insert(0, Target::Shape(2), 0, 0, "x"),
            IntentError::NoSuchSlide(0),
        ),
        (
            insert(2, Target::Shape(2), 0, 0, "x"),
            IntentError::NoSuchSlide(2),
        ),
        (
            insert(1, Target::Shape(9), 0, 0, "x"),
            IntentError::NoSuchShape { slide: 1, shape: 9 },
        ),
        (
            insert(1, Target::Shape(2), 1, 0, "x"),
            IntentError::NoSuchParagraph(1),
        ),
        (
            insert(1, Target::Shape(2), 0, 1, "x"),
            IntentError::BadOffset {
                paragraph: 0,
                offset: 1,
            },
        ),
        (
            insert(1, Target::Shape(2), 0, 7, "x"),
            IntentError::BadOffset {
                paragraph: 0,
                offset: 7,
            },
        ),
    ];
    for (intent, error) in cases {
        assert_eq!(resolve(&model, &intent, &mut ids), Err(error), "{intent:?}");
    }
}

#[test]
fn delete_forward_and_backward_by_characters_joining_across_paragraphs() {
    let mut model = PptxModel::from_texts(&[&["abc\ndef"]]);
    run(
        &mut model,
        PptxIntent::DeleteText {
            slide: 1,
            target: Target::Shape(2),
            paragraph: 0,
            offset: 1,
            direction: Direction::Forward,
            count: 1,
        },
    )
    .unwrap();
    assert_eq!(texts(&model), [vec!["ac\ndef"]]);
    let ops = run(
        &mut model,
        PptxIntent::DeleteText {
            slide: 1,
            target: Target::Shape(2),
            paragraph: 1,
            offset: 1,
            direction: Direction::Backward,
            count: 2,
        },
    )
    .unwrap();
    assert_eq!(texts(&model), [vec!["acef"]]);
    assert!(ops
        .iter()
        .any(|op| matches!(op, PptxOp::JoinParagraph { .. })));
}

#[test]
fn replace_a_range_across_paragraphs() {
    let mut model = PptxModel::from_texts(&[&["one\ntwo\nthree"]]);
    run(
        &mut model,
        PptxIntent::ReplaceRange {
            slide: 1,
            target: Target::Shape(2),
            start_paragraph: 0,
            start_offset: 2,
            end_paragraph: 2,
            end_offset: 1,
            text: "+".into(),
        },
    )
    .unwrap();
    assert_eq!(texts(&model), [vec!["on+hree"]]);
    let mut ids = |_| Id::new(1, ReplicaId(7));
    assert_eq!(
        resolve(
            &model,
            &PptxIntent::ReplaceRange {
                slide: 1,
                target: Target::Shape(2),
                start_paragraph: 0,
                start_offset: 3,
                end_paragraph: 0,
                end_offset: 1,
                text: String::new(),
            },
            &mut ids
        ),
        Err(IntentError::InvertedRange)
    );
}

#[test]
fn paragraph_breaks_and_speaker_notes() {
    let mut model = PptxModel::from_texts(&[&["ab"]]);
    run(
        &mut model,
        PptxIntent::InsertParagraphBreak {
            slide: 1,
            target: Target::Shape(2),
            paragraph: 0,
            offset: 1,
            count: 2,
        },
    )
    .unwrap();
    assert_eq!(texts(&model), [vec!["a\n\nb"]]);
    run(&mut model, insert(1, Target::Notes, 0, 0, "speak\nslowly")).unwrap();
    assert_eq!(model.slides()[0].notes, "speak\nslowly");
}

#[test]
fn slides_by_number() {
    let mut model = PptxModel::from_texts(&[&["a"], &["b"], &["c"]]);
    run(&mut model, PptxIntent::InsertSlide { at: 2 }).unwrap();
    assert_eq!(texts(&model), [vec!["a"], vec![], vec!["b"], vec!["c"]]);
    run(&mut model, PptxIntent::MoveSlide { slide: 1, to: 4 }).unwrap();
    assert_eq!(texts(&model), [vec![], vec!["b"], vec!["c"], vec!["a"]]);
    run(&mut model, PptxIntent::DuplicateSlide { slide: 2 }).unwrap();
    assert_eq!(
        texts(&model),
        [vec![], vec!["b"], vec!["b"], vec!["c"], vec!["a"]]
    );
    run(&mut model, PptxIntent::DeleteSlide { slide: 1 }).unwrap();
    run(&mut model, PptxIntent::MoveSlide { slide: 4, to: 1 }).unwrap();
    assert_eq!(texts(&model), [vec!["a"], vec!["b"], vec!["b"], vec!["c"]]);
    run(&mut model, PptxIntent::InsertSlide { at: 5 }).unwrap();
    assert_eq!(model.slides().len(), 5);
    let mut ids = |_| Id::new(1, ReplicaId(7));
    for intent in [
        PptxIntent::InsertSlide { at: 0 },
        PptxIntent::InsertSlide { at: 7 },
        PptxIntent::MoveSlide { slide: 1, to: 6 },
        PptxIntent::DeleteSlide { slide: 6 },
    ] {
        assert!(resolve(&model, &intent, &mut ids).is_err(), "{intent:?}");
    }
}

#[test]
fn text_boxes_geometry_and_deletion_by_shape_id() {
    let mut model = PptxModel::from_texts(&[&["a"]]);
    run(
        &mut model,
        PptxIntent::InsertTextBox {
            slide: 1,
            x: 100,
            y: 200,
            width: 300,
            height: 400,
            text: "new\nbox".into(),
        },
    )
    .unwrap();
    let slide = &model.slides()[0];
    let new = slide.shapes.last().unwrap();
    assert_eq!(new.text.as_deref(), Some("new\nbox"));
    let number = new.number;
    run(
        &mut model,
        PptxIntent::SetGeometry {
            slide: 1,
            shape: number,
            x: Some(5),
            y: None,
            width: None,
            height: Some(6),
            rotation: Some(5_400_000),
        },
    )
    .unwrap();
    let geometry = model.slides()[0].shapes.last().unwrap().geometry.unwrap();
    assert_eq!(
        (
            geometry.x,
            geometry.y,
            geometry.width,
            geometry.height,
            geometry.rotation
        ),
        (5, 200, 300, 6, 5_400_000)
    );
    run(&mut model, PptxIntent::DeleteShape { slide: 1, shape: 2 }).unwrap();
    assert_eq!(texts(&model), [vec!["new\nbox"]]);
}

#[test]
fn intents_have_a_stable_wire_format() {
    let intent: PptxIntent = serde_json::from_str(
        r#"{"type":"insert_text","slide":1,"target":{"shape":2},"paragraph":0,"offset":0,"text":"x"}"#,
    )
    .unwrap();
    assert_eq!(intent, insert(1, Target::Shape(2), 0, 0, "x"));
    let intent: PptxIntent = serde_json::from_str(
        r#"{"type":"delete_text","slide":1,"target":"notes","paragraph":0,"offset":0}"#,
    )
    .unwrap();
    assert_eq!(
        intent,
        PptxIntent::DeleteText {
            slide: 1,
            target: Target::Notes,
            paragraph: 0,
            offset: 0,
            direction: Direction::Forward,
            count: 1
        }
    );
    let intent: PptxIntent =
        serde_json::from_str(r#"{"type":"set_geometry","slide":2,"shape":4,"x":10}"#).unwrap();
    assert_eq!(
        intent,
        PptxIntent::SetGeometry {
            slide: 2,
            shape: 4,
            x: Some(10),
            y: None,
            width: None,
            height: None,
            rotation: None
        }
    );
    for json in [
        r#"{"type":"insert_slide","at":1}"#,
        r#"{"type":"move_slide","slide":1,"to":2}"#,
        r#"{"type":"duplicate_slide","slide":1}"#,
        r#"{"type":"delete_slide","slide":1}"#,
        r#"{"type":"insert_text_box","slide":1,"x":0,"y":0,"width":1,"height":1}"#,
        r#"{"type":"delete_shape","slide":1,"shape":2}"#,
        r#"{"type":"insert_paragraph_break","slide":1,"target":{"shape":2},"paragraph":0,"offset":0}"#,
        r#"{"type":"replace_range","slide":1,"target":"notes","start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":0,"text":""}"#,
        r#"{"type":"set_slide_hidden","slide":2,"hidden":true}"#,
        r#"{"type":"format_text","slide":1,"shape":2,"start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":1,"format":{"attribute":"bold","value":true}}"#,
        r#"{"type":"format_text","slide":1,"shape":2,"start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":1,"format":{"attribute":"size","hundredths":2400}}"#,
        r#"{"type":"align_paragraphs","slide":1,"shape":2,"start_paragraph":0,"end_paragraph":1,"alignment":"center"}"#,
    ] {
        let intent: PptxIntent =
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{json}: {e}"));
        let back = serde_json::to_string(&intent).unwrap();
        assert_eq!(serde_json::from_str::<PptxIntent>(&back).unwrap(), intent);
    }
}

#[test]
fn slides_are_hidden_and_shown_by_number() {
    let mut model = PptxModel::from_texts(&[&["a"], &["b"]]);
    let ops = run(
        &mut model,
        PptxIntent::SetSlideHidden {
            slide: 2,
            hidden: true,
        },
    )
    .unwrap();
    assert!(matches!(
        ops.as_slice(),
        [PptxOp::SetSlideHidden { hidden: true, .. }]
    ));
    assert_eq!(
        model.slides().iter().map(|s| s.hidden).collect::<Vec<_>>(),
        [false, true]
    );
    run(
        &mut model,
        PptxIntent::SetSlideHidden {
            slide: 2,
            hidden: false,
        },
    )
    .unwrap();
    assert!(!model.slides()[1].hidden, "a later write wins");
    assert_eq!(
        run(
            &mut model,
            PptxIntent::SetSlideHidden {
                slide: 3,
                hidden: true
            }
        ),
        Err(IntentError::NoSuchSlide(3))
    );
}

#[test]
fn formatting_by_shape_id_and_byte_offsets() {
    use jade_pptx_collab::{Alignment, CharFormat, StoryId};
    let mut model = PptxModel::from_texts(&[&["日本語", "a\nb\n"]]);
    let ops = run(
        &mut model,
        PptxIntent::FormatText {
            slide: 1,
            shape: 2,
            start_paragraph: 0,
            start_offset: 3,
            end_paragraph: 0,
            end_offset: 9,
            format: CharFormat::Bold { value: true },
        },
    )
    .unwrap();
    assert!(matches!(ops.as_slice(), [PptxOp::Mark { .. }]));
    let slide = &model.slides()[0];
    let story = StoryId::Shape {
        slide: slide.id,
        shape: slide.shapes[0].id,
    };
    let chars = model.paragraphs(story).unwrap()[0].chars.clone();
    let marked = model.char_formats(story).unwrap();
    assert!(!marked.contains_key(&chars[0]));
    assert_eq!(marked[&chars[1]], [CharFormat::Bold { value: true }]);
    assert_eq!(marked[&chars[2]], [CharFormat::Bold { value: true }]);

    // An empty range formats nothing; notes and bad values are refused.
    let empty = PptxIntent::FormatText {
        slide: 1,
        shape: 2,
        start_paragraph: 0,
        start_offset: 3,
        end_paragraph: 0,
        end_offset: 3,
        format: CharFormat::Bold { value: true },
    };
    assert_eq!(run(&mut model, empty), Err(IntentError::NothingToFormat));
    let bad = PptxIntent::FormatText {
        slide: 1,
        shape: 2,
        start_paragraph: 0,
        start_offset: 0,
        end_paragraph: 0,
        end_offset: 3,
        format: CharFormat::Color {
            color: "red".into(),
        },
    };
    assert!(matches!(
        run(&mut model, bad),
        Err(IntentError::InvalidFormat(_))
    ));

    // Alignment of every paragraph, the empty last one included.
    let ops = run(
        &mut model,
        PptxIntent::AlignParagraphs {
            slide: 1,
            shape: 3,
            start_paragraph: 1,
            end_paragraph: 2,
            alignment: Alignment::Right,
        },
    )
    .unwrap();
    let story = StoryId::Shape {
        slide: slide.id,
        shape: slide.shapes[1].id,
    };
    let marks: Vec<_> = model
        .paragraphs(story)
        .unwrap()
        .iter()
        .filter_map(|p| p.mark)
        .collect();
    assert_eq!(
        ops,
        [PptxOp::AlignParagraphs {
            story,
            from: Some(marks[0]),
            to: Some(marks[1]),
            alignment: Alignment::Right,
        }]
    );
    assert_eq!(
        run(
            &mut model,
            PptxIntent::AlignParagraphs {
                slide: 1,
                shape: 3,
                start_paragraph: 1,
                end_paragraph: 3,
                alignment: Alignment::Right,
            }
        ),
        Err(IntentError::NoSuchParagraph(3))
    );
}
