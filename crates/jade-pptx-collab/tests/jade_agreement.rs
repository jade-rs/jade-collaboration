//! The model and jade must describe the same deck — the same slides in the
//! same order, the same shapes, the same text in every editable text body,
//! the same speaker notes and geometry — on load, after every operation the
//! materializer realises, and after the file is exported and read back.

mod common;

use common::{assert_agree, assert_agree_with, fixtures, generate, open};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_pptx_collab::intent::{resolve, PptxIntent, Target};
use jade_pptx_collab::{Geometry, GeometryEdit, JadePptx, PptxModel, PptxOp, StoryId};
use proptest::prelude::*;

/// Every deck jade can open yields a model that agrees with it.
#[test]
fn every_fixture_loads_into_an_agreeing_model() {
    let mut paths: Vec<_> = std::fs::read_dir(fixtures())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pptx"))
        .collect();
    paths.sort();
    let mut checked = 0;
    for path in paths {
        let Ok(jade) = JadePptx::open(&std::fs::read(&path).unwrap()) else {
            continue;
        };
        let model = PptxModel::from_pptx(jade.bytes())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_agree(&model, &jade, &path.display().to_string());
        checked += 1;
    }
    assert!(checked >= 4, "only {checked} fixtures were checked");
}

#[test]
fn group_children_and_notes_are_loaded() {
    let (model, _) = open("grouped-deck.pptx");
    let slide = &model.slides()[0];
    let child = slide
        .shapes
        .iter()
        .find(|s| s.text.as_deref() == Some("Group text"))
        .expect("the grouped shape's text is a story");
    assert!(child.group.is_some());
    assert!(child.geometry.is_none() && !child.deletable);
    let (model, _) = open("sample-deck.pptx");
    assert!(model.slides()[2].notes.contains('\n'));
}

struct Session {
    model: PptxModel,
    jade: JadePptx,
    seq: u64,
    next: u64,
}

impl Session {
    fn open(name: &str) -> Self {
        let (model, jade) = open(name);
        Self {
            model,
            jade,
            seq: 0,
            next: 1_000_000,
        }
    }

    fn id(&mut self) -> Id {
        self.next += 1;
        Id::new(self.next, ReplicaId(1))
    }

    /// Applies `op` as the server would and checks agreement.
    fn step(&mut self, op: PptxOp) {
        self.seq += 1;
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: self.seq - 1,
            seq: Some(self.seq),
        };
        if PptxModel::lane(&op) == Lane::Sequenced {
            self.jade
                .admit(&self.model, &op)
                .unwrap_or_else(|e| panic!("jade refused {op:?}: {e}"));
        }
        let effect = self
            .model
            .apply(&op, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        self.jade
            .materialize(&self.model, effect, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        assert_agree(&self.model, &self.jade, &format!("after {op:?}"));
    }

    fn intent(&mut self, intent: PptxIntent) {
        let mut next = self.next;
        let ops = resolve(&self.model, &intent, &mut |n| {
            let id = Id::new(next + 1, ReplicaId(1));
            next += n;
            id
        })
        .unwrap_or_else(|e| panic!("{intent:?}: {e}"));
        self.next = next;
        for op in ops {
            self.step(op);
        }
    }
}

/// Slide structure, placeholders of a new slide, text boxes, geometry and
/// shape deletion end to end in jade, then through export.
#[test]
fn structure_is_realised_in_jade() {
    let mut s = Session::open("sample-deck.pptx");
    // A new slide gets the placeholders of its layout, and they take text.
    s.intent(PptxIntent::InsertSlide { at: 2 });
    let new = s.model.slides()[1].clone();
    assert!(!new.shapes.is_empty(), "the layout's placeholders");
    let number = new.shapes[0].number;
    s.intent(PptxIntent::InsertText {
        slide: 2,
        target: Target::Shape(number),
        paragraph: 0,
        offset: 0,
        text: "New title\nline 😀".into(),
    });
    s.intent(PptxIntent::InsertText {
        slide: 2,
        target: Target::Notes,
        paragraph: 0,
        offset: 0,
        text: "first notes".into(),
    });
    // Duplicate the slide with notes, then move the copy to the front.
    s.intent(PptxIntent::DuplicateSlide { slide: 4 });
    assert_eq!(s.model.slides()[4].notes, s.model.slides()[3].notes);
    s.intent(PptxIntent::MoveSlide { slide: 5, to: 1 });
    // A text box with text, moved and resized, on the copy.
    s.intent(PptxIntent::InsertTextBox {
        slide: 1,
        x: 914_400,
        y: 914_400,
        width: 2_000_000,
        height: 500_000,
        text: "box\ntext".into(),
    });
    let slide = s.model.slides()[0].clone();
    let shape = slide.shapes.last().unwrap().clone();
    let stamp = s.id();
    s.step(PptxOp::SetGeometry {
        slide: slide.id,
        shape: shape.id,
        stamp,
        edit: GeometryEdit {
            x: Some(100),
            width: Some(3_000_000),
            rotation: Some(2_700_000),
            ..GeometryEdit::default()
        },
    });
    // Typing into the box after it moved.
    s.step(PptxOp::JoinParagraph {
        story: StoryId::Shape {
            slide: slide.id,
            shape: shape.id,
        },
        mark: s.model.paragraphs(StoryId::Shape {
            slide: slide.id,
            shape: shape.id,
        })
        .unwrap()[0]
            .mark
            .unwrap(),
    });
    // Delete a loaded shape, the original slide and the first one.
    let target = s.model.slides()[3].clone();
    let deletable = target.shapes.iter().position(|s| s.deletable).unwrap();
    s.step(PptxOp::DeleteShape {
        slide: target.id,
        shape: target.shapes[deletable].id,
    });
    s.intent(PptxIntent::DeleteSlide { slide: 4 });
    s.intent(PptxIntent::DeleteSlide { slide: 2 });
    let reopened = JadePptx::open(&s.jade.export().unwrap()).unwrap();
    assert_agree_with(&s.model, &s.jade, &reopened, "after export");
}

#[test]
fn a_text_box_keeps_jades_shape_id_through_duplication() {
    let mut s = Session::open("inherited-deck.pptx");
    s.intent(PptxIntent::InsertTextBox {
        slide: 1,
        x: 0,
        y: 0,
        width: 100,
        height: 100,
        text: "t".into(),
    });
    s.intent(PptxIntent::DuplicateSlide { slide: 1 });
    let slide = s.model.slides()[1].clone();
    let box_shape = slide.shapes.last().unwrap();
    s.intent(PptxIntent::InsertText {
        slide: 2,
        target: Target::Shape(box_shape.number),
        paragraph: 0,
        offset: 1,
        text: "wo".into(),
    });
    assert_eq!(s.model.slides()[0].shapes.last().unwrap().text.as_deref(), Some("t"));
    assert_eq!(s.model.slides()[1].shapes.last().unwrap().text.as_deref(), Some("two"));
}

/// A placeholder on a new slide inherits its placement from the layout;
/// the model knows it (jade resolved it when the template was read), so
/// moving it writes the full transform.
#[test]
fn geometry_of_new_slides_comes_from_jade() {
    let mut s = Session::open("inherited-deck.pptx");
    s.intent(PptxIntent::InsertSlide { at: 1 });
    let shape = s.model.slides()[0].shapes[0].clone();
    let geometry: Geometry = shape.geometry.expect("inherited placement is resolved");
    let stamp = s.id();
    let slide = s.model.slides()[0].id;
    s.step(PptxOp::SetGeometry {
        slide,
        shape: shape.id,
        stamp,
        edit: GeometryEdit {
            y: Some(geometry.y + 10),
            ..GeometryEdit::default()
        },
    });
}

const EDIT_FIXTURES: &[&str] = &[
    "sample-deck.pptx",
    "inherited-deck.pptx",
    "grouped-deck.pptx",
    "themed-deck.pptx",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Random edit sequences keep the model and jade in agreement after
    /// every single operation, and the exported file reads back as the same
    /// deck.
    #[test]
    fn random_edits_keep_model_and_jade_in_agreement(
        fixture in prop::sample::select(EDIT_FIXTURES),
        choices in prop::collection::vec(any::<u64>(), 1..20),
    ) {
        let (mut model, mut jade) = open(fixture);
        let mut next = 1_000_000u64;
        for (step, choice) in choices.into_iter().enumerate() {
            let mut ids = |n: u64| {
                let id = Id::new(next, ReplicaId(1));
                next += n;
                id
            };
            let Some(op) = generate(&model, choice, &mut ids) else { continue };
            let seq = step as u64 + 1;
            let ctx = ApplyContext { origin: ReplicaId(1), base_seq: seq - 1, seq: Some(seq) };
            // As the sequencer does: jade may refuse a sequenced op first.
            if PptxModel::lane(&op) == Lane::Sequenced && jade.admit(&model, &op).is_err() {
                continue;
            }
            let before = model.clone();
            match model.apply(&op, &ctx) {
                Ok(effect) => {
                    jade.materialize(&model, effect, &ctx)
                        .unwrap_or_else(|e| panic!("{fixture}: jade refused {op:?}: {e}"));
                    assert_agree(&model, &jade, &format!("{fixture} after {op:?}"));
                }
                Err(_) => prop_assert_eq!(&model, &before, "a rejected op changed the model"),
            }
        }
        let reopened = JadePptx::open(&jade.export().unwrap()).unwrap();
        assert_agree_with(&model, &jade, &reopened, &format!("{fixture} after export"));
        // A model built from the exported file agrees with it too.
        let reloaded = PptxModel::from_pptx(reopened.bytes()).unwrap();
        assert_agree(&reloaded, &reopened, &format!("{fixture} reloaded"));
    }
}
