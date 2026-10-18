//! The model on its own: slides as an ordered list of ids, every text body
//! (shapes and speaker notes) an RGA of characters and paragraph marks,
//! geometry as per-attribute last-writer-wins registers, and the lane each
//! operation travels.

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_pptx_collab::{
    Geometry, GeometryEdit, PptxEffect, PptxModel, PptxOp, ShapeKind, ShapeRef, SlideView, StoryId,
    TextTarget,
};
use proptest::prelude::*;

fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: None,
    }
}

fn at(origin: u64, base_seq: u64, seq: Option<u64>) -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(origin),
        base_seq,
        seq,
    }
}

fn id(n: u64) -> Id {
    Id::new(1000 + n, ReplicaId(1))
}

fn deck() -> PptxModel {
    PptxModel::from_texts(&[&["Title", "Body\nsecond"], &["Only"]])
}

fn story(slide: &SlideView, shape: usize) -> StoryId {
    StoryId::Shape {
        slide: slide.id,
        shape: slide.shapes[shape].id,
    }
}

fn target(slide: &SlideView, shape: usize) -> TextTarget {
    let s = &slide.shapes[shape];
    TextTarget::Shape(ShapeRef {
        slide: slide.id,
        shape: s.id,
        number: s.number,
    })
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

fn ids(model: &PptxModel) -> Vec<Id> {
    model.slides().iter().map(|s| s.id).collect()
}

#[test]
fn builds_slides_shapes_and_paragraphs() {
    let model = deck();
    let slides = model.slides();
    assert_eq!(slides.len(), 2);
    assert_eq!(texts(&model), [vec!["Title", "Body\nsecond"], vec!["Only"]]);
    let shape = &slides[0].shapes[1];
    assert_eq!(shape.kind, ShapeKind::Shape);
    assert_eq!(slides[0].shapes[0].number, 2);
    assert_eq!(shape.number, 3);
    assert!(shape.editable && shape.deletable);
    assert!(shape.geometry.is_some());
    let paragraphs = model.paragraphs(story(&slides[0], 1)).unwrap();
    assert_eq!(paragraphs.len(), 2);
    assert_eq!(paragraphs[0].text, "Body");
    assert!(paragraphs[0].mark.is_some());
    assert!(
        paragraphs[1].mark.is_none(),
        "a text body's last paragraph has no mark atom"
    );
    assert_eq!(slides[0].notes, "");
    assert!(model.max_lamport() > 0);
}

#[test]
fn inserting_text_reports_slide_shape_paragraph_and_byte_offset() {
    let mut model = deck();
    let slides = model.slides();
    let e = model.paragraphs(story(&slides[1], 0)).unwrap()[0].chars[1];
    let effect = model
        .apply(
            &PptxOp::InsertText {
                story: story(&slides[1], 0),
                parent: Some(e),
                first: id(1),
                text: "日本".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model)[1], ["On日本ly"]);
    assert_eq!(
        effect,
        vec![PptxEffect::InsertText {
            slide: 1,
            target: target(&slides[1], 0),
            paragraph: 0,
            offset: 2,
            text: "日本".into(),
        }]
    );
    // The next insertion counts the two three-byte characters.
    let hon = id(2);
    let effect = model
        .apply(
            &PptxOp::InsertText {
                story: story(&slides[1], 0),
                parent: Some(hon),
                first: id(3),
                text: "!".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert!(matches!(
        effect[..],
        [PptxEffect::InsertText { offset: 8, .. }]
    ));
}

#[test]
fn text_must_be_plain_characters_jade_accepts() {
    let mut model = deck();
    let slides = model.slides();
    for bad in ["a\nb", "\u{1}", "", "\u{FFFF}"] {
        let before = model.clone();
        assert!(
            model
                .apply(
                    &PptxOp::InsertText {
                        story: story(&slides[0], 0),
                        parent: None,
                        first: id(1),
                        text: bad.into(),
                    },
                    &ctx(),
                )
                .is_err(),
            "{bad:?} must be refused"
        );
        assert_eq!(model, before);
    }
}

#[test]
fn deleting_reports_ranges_from_the_end() {
    let mut model = PptxModel::from_texts(&[&["abcdef\ngh"]]);
    let slides = model.slides();
    let p = model.paragraphs(story(&slides[0], 0)).unwrap();
    let ids = vec![p[0].chars[1], p[0].chars[2], p[0].chars[4], p[1].chars[0]];
    let effect = model
        .apply(
            &PptxOp::DeleteText {
                story: story(&slides[0], 0),
                ids,
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model), [vec!["adf\nh"]]);
    let t = target(&slides[0], 0);
    assert_eq!(
        effect,
        vec![
            PptxEffect::DeleteRange {
                slide: 0,
                target: t,
                paragraph: 1,
                start: 0,
                end: 1
            },
            PptxEffect::DeleteRange {
                slide: 0,
                target: t,
                paragraph: 0,
                start: 4,
                end: 5
            },
            PptxEffect::DeleteRange {
                slide: 0,
                target: t,
                paragraph: 0,
                start: 1,
                end: 3
            },
        ]
    );
    // Deleting again is harmless.
    let a = model.paragraphs(story(&slides[0], 0)).unwrap()[0].chars[0];
    let op = PptxOp::DeleteText {
        story: story(&slides[0], 0),
        ids: vec![a],
    };
    model.apply(&op, &ctx()).unwrap();
    assert_eq!(model.apply(&op, &ctx()).unwrap(), vec![]);
}

#[test]
fn convergent_deletes_cannot_remove_paragraph_marks() {
    let mut model = deck();
    let slides = model.slides();
    let mark = model.paragraphs(story(&slides[0], 1)).unwrap()[0]
        .mark
        .unwrap();
    assert!(model
        .apply(
            &PptxOp::DeleteText {
                story: story(&slides[0], 1),
                ids: vec![mark],
            },
            &ctx()
        )
        .is_err());
}

#[test]
fn split_and_join() {
    let mut model = PptxModel::from_texts(&[&["Hello world"]]);
    let slides = model.slides();
    let s = story(&slides[0], 0);
    let space = model.paragraphs(s).unwrap()[0].chars[5];
    let effect = model
        .apply(
            &PptxOp::SplitParagraph {
                story: s,
                parent: Some(space),
                id: id(1),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model), [vec!["Hello \nworld"]]);
    assert_eq!(
        effect,
        vec![PptxEffect::SplitParagraph {
            slide: 0,
            target: target(&slides[0], 0),
            paragraph: 0,
            offset: 6
        }]
    );
    let effect = model
        .apply(
            &PptxOp::JoinParagraph {
                story: s,
                mark: id(1),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model), [vec!["Hello world"]]);
    assert_eq!(
        effect,
        vec![PptxEffect::JoinParagraph {
            slide: 0,
            target: target(&slides[0], 0),
            paragraph: 0,
            offset: 6
        }]
    );
    assert!(
        model
            .apply(
                &PptxOp::JoinParagraph {
                    story: s,
                    mark: id(1)
                },
                &ctx()
            )
            .is_err(),
        "already joined"
    );
}

#[test]
fn speaker_notes_are_a_story_of_their_own() {
    let mut model = deck();
    let slides = model.slides();
    let notes = StoryId::Notes {
        slide: slides[1].id,
    };
    assert_eq!(model.paragraphs(notes).unwrap().len(), 1);
    let effect = model
        .apply(
            &PptxOp::InsertText {
                story: notes,
                parent: None,
                first: id(1),
                text: "say this".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![PptxEffect::InsertText {
            slide: 1,
            target: TextTarget::Notes,
            paragraph: 0,
            offset: 0,
            text: "say this".into()
        }]
    );
    model
        .apply(
            &PptxOp::SplitParagraph {
                story: notes,
                parent: Some(id(3)),
                id: id(20),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(model.slides()[1].notes, "say\n this");
    assert_eq!(model.text(notes).as_deref(), Some("say\n this"));
}

#[test]
fn lanes() {
    let model = deck();
    let slides = model.slides();
    let s = story(&slides[0], 0);
    let (slide, shape) = (slides[0].id, slides[0].shapes[0].id);
    let convergent = [
        PptxOp::InsertText {
            story: s,
            parent: None,
            first: id(1),
            text: "a".into(),
        },
        PptxOp::DeleteText {
            story: s,
            ids: vec![],
        },
        PptxOp::SplitParagraph {
            story: s,
            parent: None,
            id: id(1),
        },
        PptxOp::SetGeometry {
            slide,
            shape,
            stamp: id(1),
            edit: GeometryEdit::default(),
        },
    ];
    for op in &convergent {
        assert_eq!(PptxModel::lane(op), Lane::Convergent, "{op:?}");
    }
    let sequenced = [
        PptxOp::JoinParagraph {
            story: s,
            mark: id(1),
        },
        PptxOp::InsertSlide {
            id: id(1),
            after: None,
        },
        PptxOp::DeleteSlide { slide },
        PptxOp::MoveSlide { slide, after: None },
        PptxOp::DuplicateSlide { slide, id: id(1) },
        PptxOp::InsertTextBox {
            slide,
            id: id(1),
            geometry: Geometry {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                rotation: 0,
            },
        },
        PptxOp::DeleteShape { slide, shape },
    ];
    for op in &sequenced {
        assert_eq!(PptxModel::lane(op), Lane::Sequenced, "{op:?}");
    }
}

#[test]
fn slides_are_inserted_moved_and_deleted_by_id() {
    let mut model = deck();
    let [a, b] = ids(&model)[..] else { panic!() };
    let effect = model
        .apply(
            &PptxOp::InsertSlide {
                id: id(1),
                after: Some(a),
            },
            &at(1, 0, Some(1)),
        )
        .unwrap();
    assert_eq!(effect, vec![PptxEffect::InsertSlide { at: 1 }]);
    assert_eq!(ids(&model), [a, id(1), b]);
    let effect = model
        .apply(
            &PptxOp::InsertSlide {
                id: id(2),
                after: None,
            },
            &at(1, 1, Some(2)),
        )
        .unwrap();
    assert_eq!(effect, vec![PptxEffect::InsertSlide { at: 0 }]);
    assert_eq!(ids(&model), [id(2), a, id(1), b]);

    // Move `a` behind `b`: jade's final index is 3.
    let effect = model
        .apply(
            &PptxOp::MoveSlide {
                slide: a,
                after: Some(b),
            },
            &at(1, 2, Some(3)),
        )
        .unwrap();
    assert_eq!(effect, vec![PptxEffect::MoveSlide { from: 1, to: 3 }]);
    assert_eq!(ids(&model), [id(2), id(1), b, a]);
    let effect = model
        .apply(
            &PptxOp::MoveSlide {
                slide: a,
                after: None,
            },
            &at(1, 3, Some(4)),
        )
        .unwrap();
    assert_eq!(effect, vec![PptxEffect::MoveSlide { from: 3, to: 0 }]);
    assert_eq!(ids(&model), [a, id(2), id(1), b]);

    let effect = model
        .apply(&PptxOp::DeleteSlide { slide: id(1) }, &at(1, 4, Some(5)))
        .unwrap();
    assert_eq!(effect, vec![PptxEffect::DeleteSlide { at: 2 }]);
    assert_eq!(ids(&model), [a, id(2), b]);
    // A deleted slide cannot be an anchor, moved or deleted again.
    for op in [
        PptxOp::InsertSlide {
            id: id(9),
            after: Some(id(1)),
        },
        PptxOp::MoveSlide {
            slide: id(1),
            after: None,
        },
        PptxOp::MoveSlide {
            slide: a,
            after: Some(id(1)),
        },
        PptxOp::DeleteSlide { slide: id(1) },
        PptxOp::InsertSlide { id: a, after: None },
    ] {
        assert!(model.apply(&op, &at(1, 5, Some(6))).is_err(), "{op:?}");
    }
}

#[test]
fn the_last_slide_cannot_be_deleted() {
    let mut model = PptxModel::from_texts(&[&["a"], &["b"]]);
    let [a, b] = ids(&model)[..] else { panic!() };
    model
        .apply(&PptxOp::DeleteSlide { slide: a }, &at(1, 0, Some(1)))
        .unwrap();
    let before = model.clone();
    assert!(model
        .apply(&PptxOp::DeleteSlide { slide: b }, &at(2, 0, Some(2)))
        .is_err());
    assert_eq!(model, before);
}

#[test]
fn text_typed_on_a_concurrently_deleted_slide_lands_nowhere_visible() {
    let mut model = deck();
    let slides = model.slides();
    model
        .apply(
            &PptxOp::DeleteSlide {
                slide: slides[1].id,
            },
            &at(2, 0, Some(1)),
        )
        .unwrap();
    let effect = model
        .apply(
            &PptxOp::InsertText {
                story: story(&slides[1], 0),
                parent: None,
                first: id(1),
                text: "late".into(),
            },
            &at(1, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(effect, vec![]);
    assert_eq!(model.slides().len(), 1);
    assert_eq!(
        model.text(story(&slides[1], 0)).as_deref(),
        Some("lateOnly"),
        "kept in the tombstoned slide"
    );
}

#[test]
fn duplicating_a_slide_copies_its_text_notes_and_geometry() {
    let mut model = deck();
    let slides = model.slides();
    let notes = StoryId::Notes {
        slide: slides[0].id,
    };
    model
        .apply(
            &PptxOp::InsertText {
                story: notes,
                parent: None,
                first: id(1),
                text: "n".into(),
            },
            &at(1, 0, Some(1)),
        )
        .unwrap();
    let effect = model
        .apply(
            &PptxOp::DuplicateSlide {
                slide: slides[0].id,
                id: id(10),
            },
            &at(1, 1, Some(2)),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![PptxEffect::DuplicateSlide {
            at: 0,
            source: slides[0].id,
            copy: id(10)
        }]
    );
    let after = model.slides();
    assert_eq!(ids(&model), [slides[0].id, id(10), slides[1].id]);
    let (original, copy) = (&after[0], &after[1]);
    assert_eq!(copy.notes, "n");
    assert_eq!(copy.shapes.len(), original.shapes.len());
    for (a, b) in original.shapes.iter().zip(&copy.shapes) {
        assert_eq!(
            (a.id, a.number, &a.text, a.geometry),
            (b.id, b.number, &b.text, b.geometry)
        );
    }
    // The copy is independent once its author saw it.
    let effect = model
        .apply(
            &PptxOp::InsertText {
                story: story(copy, 0),
                parent: None,
                first: id(20),
                text: "copy ".into(),
            },
            &at(2, 2, Some(3)),
        )
        .unwrap();
    assert_eq!(effect.len(), 1);
    assert_eq!(texts(&model)[0][0], "Title");
    assert_eq!(texts(&model)[1][0], "copy Title");
}

/// An edit whose author had not seen a duplication lands in both copies —
/// the server and every replica agree on that whichever order they apply
/// the two in, so a remote convergent edit can still skip the rebuild.
#[test]
fn an_edit_concurrent_with_a_duplication_lands_in_both_copies() {
    let base = deck();
    let slides = base.slides();
    let s = story(&slides[0], 0);
    let edit = PptxOp::InsertText {
        story: s,
        parent: None,
        first: id(20),
        text: ">".into(),
    };
    let dup = PptxOp::DuplicateSlide {
        slide: slides[0].id,
        id: id(10),
    };

    // Server order: the duplication commits first (seq 1); the edit, made
    // by replica 2 at seq 0, commits second.
    let mut server = base.clone();
    server.apply(&dup, &at(1, 0, Some(1))).unwrap();
    let effect = server.apply(&edit, &at(2, 0, Some(2))).unwrap();
    assert_eq!(effect.len(), 2, "realised on both slides: {effect:?}");
    assert_eq!(texts(&server)[0][0], ">Title");
    assert_eq!(texts(&server)[1][0], ">Title");

    // Replica 1's view: its own duplication is still tentative when the
    // edit arrives and is applied straight onto the view.
    let mut view = base.clone();
    view.apply(&dup, &at(1, 0, None)).unwrap();
    view.apply(&edit, &at(2, 0, Some(1))).unwrap();
    assert_eq!(texts(&view), texts(&server));

    // The other order: the edit commits first and is copied.
    let mut other = base.clone();
    other.apply(&edit, &at(2, 0, Some(1))).unwrap();
    other.apply(&dup, &at(1, 0, Some(2))).unwrap();
    assert_eq!(texts(&other), texts(&server));
}

#[test]
fn an_edit_made_after_seeing_a_duplication_stays_in_its_slide() {
    let mut model = deck();
    let slides = model.slides();
    let s = story(&slides[0], 0);
    model
        .apply(
            &PptxOp::DuplicateSlide {
                slide: slides[0].id,
                id: id(10),
            },
            &at(1, 0, Some(1)),
        )
        .unwrap();
    // Replica 2 had seen seq 1.
    model
        .apply(
            &PptxOp::InsertText {
                story: s,
                parent: None,
                first: id(20),
                text: "a".into(),
            },
            &at(2, 1, Some(2)),
        )
        .unwrap();
    // The duplicating replica's own later edit, even before it saw seq 1.
    model
        .apply(
            &PptxOp::InsertText {
                story: s,
                parent: None,
                first: id(30),
                text: "b".into(),
            },
            &at(1, 0, Some(3)),
        )
        .unwrap();
    assert_eq!(texts(&model)[0][0], "baTitle");
    assert_eq!(texts(&model)[1][0], "Title");
}

#[test]
fn a_text_box_is_an_empty_editable_shape_on_top() {
    let mut model = deck();
    let slides = model.slides();
    let geometry = Geometry {
        x: 10,
        y: 20,
        width: 300,
        height: 400,
        rotation: 0,
    };
    let effect = model
        .apply(
            &PptxOp::InsertTextBox {
                slide: slides[1].id,
                id: id(1),
                geometry,
            },
            &at(1, 0, Some(1)),
        )
        .unwrap();
    let slide = &model.slides()[1];
    let shape = slide.shapes.last().unwrap();
    assert_eq!(shape.id, id(1));
    assert_eq!(shape.text.as_deref(), Some(""));
    assert_eq!(shape.geometry, Some(geometry));
    assert!(shape.editable && shape.deletable);
    assert!(
        shape.number > slides[1].shapes.iter().map(|s| s.number).max().unwrap(),
        "a fresh number"
    );
    assert_eq!(
        effect,
        vec![PptxEffect::InsertTextBox {
            slide: 1,
            shape: ShapeRef {
                slide: slide.id,
                shape: id(1),
                number: shape.number
            },
            geometry
        }]
    );
    // Same id again, an empty box and a deleted slide are refused.
    for op in [
        PptxOp::InsertTextBox {
            slide: slides[1].id,
            id: id(1),
            geometry,
        },
        PptxOp::InsertTextBox {
            slide: slides[1].id,
            id: id(2),
            geometry: Geometry {
                width: 0,
                ..geometry
            },
        },
        PptxOp::InsertTextBox {
            slide: id(77),
            id: id(3),
            geometry,
        },
    ] {
        assert!(model.apply(&op, &at(1, 1, Some(2))).is_err(), "{op:?}");
    }
}

#[test]
fn deleting_a_shape_tombstones_it() {
    let mut model = deck();
    let slides = model.slides();
    let effect = model
        .apply(
            &PptxOp::DeleteShape {
                slide: slides[0].id,
                shape: slides[0].shapes[0].id,
            },
            &at(2, 0, Some(1)),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![PptxEffect::DeleteShape {
            slide: 0,
            shape: ShapeRef {
                slide: slides[0].id,
                shape: slides[0].shapes[0].id,
                number: 2
            }
        }]
    );
    assert_eq!(texts(&model)[0], ["Body\nsecond"]);
    // A concurrent edit still applies, invisibly.
    let effect = model
        .apply(
            &PptxOp::InsertText {
                story: story(&slides[0], 0),
                parent: None,
                first: id(1),
                text: "x".into(),
            },
            &at(1, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(effect, vec![]);
    let effect = model
        .apply(
            &PptxOp::SetGeometry {
                slide: slides[0].id,
                shape: slides[0].shapes[0].id,
                stamp: id(2),
                edit: GeometryEdit {
                    x: Some(5),
                    ..GeometryEdit::default()
                },
            },
            &at(1, 0, Some(3)),
        )
        .unwrap();
    assert_eq!(effect, vec![]);
    assert!(model
        .apply(
            &PptxOp::DeleteShape {
                slide: slides[0].id,
                shape: slides[0].shapes[0].id,
            },
            &at(1, 0, Some(4)),
        )
        .is_err());
}

#[test]
fn geometry_attributes_are_independent_registers() {
    let mut model = deck();
    let slides = model.slides();
    let (slide, shape) = (slides[0].id, slides[0].shapes[0].id);
    let before = slides[0].shapes[0].geometry.unwrap();
    let set = |stamp: Id, edit: GeometryEdit| PptxOp::SetGeometry {
        slide,
        shape,
        stamp,
        edit,
    };
    let effect = model
        .apply(
            &set(
                id(5),
                GeometryEdit {
                    x: Some(7),
                    y: Some(8),
                    ..GeometryEdit::default()
                },
            ),
            &at(1, 0, Some(1)),
        )
        .unwrap();
    let moved = Geometry {
        x: 7,
        y: 8,
        ..before
    };
    assert_eq!(
        effect,
        vec![PptxEffect::SetTransform {
            slide: 0,
            shape: ShapeRef {
                slide,
                shape,
                number: 2
            },
            geometry: moved
        }]
    );
    // An older stamp on a register already written loses; on one that was
    // not, it wins.
    let effect = model
        .apply(
            &set(
                id(3),
                GeometryEdit {
                    x: Some(1),
                    width: Some(50),
                    ..GeometryEdit::default()
                },
            ),
            &at(2, 0, Some(2)),
        )
        .unwrap();
    let resized = Geometry { width: 50, ..moved };
    assert_eq!(model.slides()[0].shapes[0].geometry, Some(resized));
    assert!(
        matches!(&effect[..], [PptxEffect::SetTransform { geometry, .. }] if *geometry == resized)
    );
    // Nothing newer: no effect.
    let effect = model
        .apply(
            &set(
                id(4),
                GeometryEdit {
                    x: Some(2),
                    ..GeometryEdit::default()
                },
            ),
            &at(2, 0, Some(3)),
        )
        .unwrap();
    assert_eq!(effect, vec![]);
}

#[test]
fn geometry_outside_powerpoints_range_is_refused() {
    let mut model = deck();
    let slides = model.slides();
    for edit in [
        GeometryEdit {
            x: Some(i64::from(i32::MAX) + 1),
            ..GeometryEdit::default()
        },
        GeometryEdit {
            width: Some(-1),
            ..GeometryEdit::default()
        },
        GeometryEdit {
            rotation: Some(i64::from(i32::MIN) - 1),
            ..GeometryEdit::default()
        },
    ] {
        let op = PptxOp::SetGeometry {
            slide: slides[0].id,
            shape: slides[0].shapes[0].id,
            stamp: id(1),
            edit,
        };
        assert!(model.apply(&op, &ctx()).is_err(), "{op:?}");
    }
}

#[test]
fn the_model_round_trips_through_json() {
    let mut model = deck();
    let slides = model.slides();
    model
        .apply(
            &PptxOp::DuplicateSlide {
                slide: slides[0].id,
                id: id(1),
            },
            &at(1, 0, Some(1)),
        )
        .unwrap();
    let json = serde_json::to_string(&model).unwrap();
    assert_eq!(serde_json::from_str::<PptxModel>(&json).unwrap(), model);
}

fn edit_strategy() -> impl Strategy<Value = GeometryEdit> {
    let value = || prop::option::of(0i64..1_000_000);
    (value(), value(), value(), value(), value()).prop_map(|(x, y, width, height, rotation)| {
        GeometryEdit {
            x,
            y,
            width,
            height,
            rotation,
        }
    })
}

fn attributes(edit: &GeometryEdit) -> [Option<i64>; 5] {
    [edit.x, edit.y, edit.width, edit.height, edit.rotation]
}

fn values(geometry: Geometry) -> [i64; 5] {
    [
        geometry.x,
        geometry.y,
        geometry.width,
        geometry.height,
        geometry.rotation,
    ]
}

proptest! {
    /// Two replicas edit one shape's geometry concurrently. Whatever order
    /// the writes arrive in, the result is the same; an attribute only one
    /// of them set keeps that value (a move and a resize both survive), and
    /// one both set takes the newer stamp's value.
    #[test]
    fn concurrent_geometry_edits_merge_per_attribute(
        a in edit_strategy(),
        b in edit_strategy(),
        stamp_a in 1u64..50,
        stamp_b in 1u64..50,
    ) {
        let base = deck();
        let slides = base.slides();
        let (slide, shape) = (slides[0].id, slides[0].shapes[0].id);
        let before = values(slides[0].shapes[0].geometry.unwrap());
        let sa = Id::new(1000 + stamp_a, ReplicaId(1));
        let sb = Id::new(1000 + stamp_b, ReplicaId(2));
        let op_a = PptxOp::SetGeometry { slide, shape, stamp: sa, edit: a.clone() };
        let op_b = PptxOp::SetGeometry { slide, shape, stamp: sb, edit: b.clone() };

        let mut ab = base.clone();
        ab.apply(&op_a, &at(1, 0, Some(1))).unwrap();
        ab.apply(&op_b, &at(2, 0, Some(2))).unwrap();
        let mut ba = base.clone();
        ba.apply(&op_b, &at(2, 0, Some(1))).unwrap();
        ba.apply(&op_a, &at(1, 0, Some(2))).unwrap();
        prop_assert_eq!(&ab, &ba);

        let result = values(ab.slides()[0].shapes[0].geometry.unwrap());
        for k in 0..5 {
            let expected = match (attributes(&a)[k], attributes(&b)[k]) {
                (None, None) => before[k],
                (Some(v), None) => v,
                (None, Some(v)) => v,
                (Some(va), Some(vb)) => if sa > sb { va } else { vb },
            };
            prop_assert_eq!(result[k], expected, "attribute {}", k);
        }
    }
}
