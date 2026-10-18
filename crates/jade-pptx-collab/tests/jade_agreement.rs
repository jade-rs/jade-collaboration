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
        mark: s
            .model
            .paragraphs(StoryId::Shape {
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
    assert_eq!(
        s.model.slides()[0].shapes.last().unwrap().text.as_deref(),
        Some("t")
    );
    assert_eq!(
        s.model.slides()[1].shapes.last().unwrap().text.as_deref(),
        Some("two")
    );
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
    #![proptest_config(ProptestConfig::with_cases(40))]

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

/// jade can refuse a structural change for a reason the model does not
/// track — here a slide whose shape tree lacks the group headers jade needs
/// to add a shape. The materializer says so before the operation is
/// applied, so the sequencer rejects it and the file never falls behind.
#[test]
fn jade_refuses_what_the_model_cannot_see() {
    use jade_collab_sync::protocol::Submission;
    use jade_collab_sync::{Sequencer, SubmitOutcome};

    let bytes = common::fixture("sample-deck.pptx");
    let mut package = jade_opc::OpcPackage::open_preserving(&bytes).unwrap();
    let part = "ppt/slides/slide1.xml";
    let xml = package.required_part_str(part).unwrap();
    assert!(xml.contains("<p:grpSpPr/>"));
    package.replace(part, xml.replace("<p:grpSpPr/>", "").into_bytes());
    let bytes = package.write().unwrap();

    let jade = JadePptx::open(&bytes).unwrap();
    let model = PptxModel::from_pptx(&bytes).unwrap();
    let op = PptxOp::InsertTextBox {
        slide: model.slides()[0].id,
        id: Id::new(1_000_000, ReplicaId(1)),
        geometry: Geometry {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
            rotation: 0,
        },
    };
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    };
    assert!(
        model.clone().apply(&op, &ctx).is_ok(),
        "the model alone would take it"
    );
    assert!(jade.admit(&model, &op).is_err());

    let mut server = Sequencer::with_materializer(model, jade);
    let replica = server.join().replica;
    let outcome = server.submit(
        replica,
        Submission {
            client_seq: 1,
            base_seq: 0,
            op,
        },
    );
    assert!(matches!(outcome, SubmitOutcome::Rejected(_)), "{outcome:?}");
    assert!(server.faults().is_empty());
    assert_agree(
        server.document(),
        server.materializer(),
        "after the refusal",
    );
}

/// A server restored from storage has the model (from JSON) and the file,
/// but not the materializer's memory of which file id jade gave each text
/// box made in the session. `JadePptx::restore` rebuilds it from the two, so
/// later edits still land on the right shape.
#[test]
fn a_restored_materializer_finds_text_boxes_whose_ids_jade_reused() {
    let mut s = Session::open("sample-deck.pptx");
    // Delete the slide's highest-numbered deletable shape, so jade gives the
    // next text box that number again while the model numbers it afresh.
    let slide = s.model.slides()[0].clone();
    let doomed = slide
        .shapes
        .iter()
        .filter(|shape| shape.deletable)
        .max_by_key(|shape| shape.number)
        .unwrap()
        .clone();
    s.step(PptxOp::DeleteShape {
        slide: slide.id,
        shape: doomed.id,
    });
    s.intent(PptxIntent::InsertTextBox {
        slide: 1,
        x: 0,
        y: 0,
        width: 1_000_000,
        height: 400_000,
        text: "box".into(),
    });
    let boxed = s.model.slides()[0].shapes.last().unwrap().clone();

    let model: PptxModel = serde_json::from_str(&serde_json::to_string(&s.model).unwrap()).unwrap();
    let mut restored = JadePptx::restore(&s.jade.export().unwrap(), &model).unwrap();
    assert_agree(&model, &restored, "restored");
    let mut model = model;
    let story = StoryId::Shape {
        slide: slide.id,
        shape: boxed.id,
    };
    let end = model.paragraphs(story).unwrap()[0].chars.last().copied();
    let op = PptxOp::InsertText {
        story,
        parent: end,
        first: Id::new(9_000_000, ReplicaId(2)),
        text: "ed".into(),
    };
    let ctx = ApplyContext {
        origin: ReplicaId(2),
        base_seq: s.seq,
        seq: Some(s.seq + 1),
    };
    let effect = model.apply(&op, &ctx).unwrap();
    restored.materialize(&model, effect, &ctx).unwrap();
    assert_agree(&model, &restored, "after editing the restored text box");
}

/// A deck whose second slide is hidden (`show="0"`), as PowerPoint writes it.
fn deck_with_hidden_slide() -> Vec<u8> {
    let bytes = common::fixture("sample-deck.pptx");
    let mut package = jade_opc::OpcPackage::open_preserving(&bytes).unwrap();
    let part = "ppt/slides/slide2.xml";
    let xml = package.required_part_str(part).unwrap();
    package.replace(part, xml.replacen("<p:sld ", "<p:sld show=\"0\" ", 1).into_bytes());
    package.write().unwrap()
}

/// Hidden slides are read from the file, and showing, hiding, duplicating
/// and inserting next to them are realised by jade as the model says.
#[test]
fn hidden_slides_are_loaded_and_realised() {
    let bytes = deck_with_hidden_slide();
    let jade = JadePptx::open(&bytes).unwrap();
    let model = PptxModel::from_pptx(&bytes).unwrap();
    assert_eq!(
        model.slides().iter().map(|s| s.hidden).collect::<Vec<_>>(),
        [false, true, false]
    );
    assert_agree(&model, &jade, "on load");
    let mut s = Session {
        model,
        jade,
        seq: 0,
        next: 1_000_000,
    };
    // A slide inserted in front of the hidden one, and a copy of it.
    s.intent(PptxIntent::InsertSlide { at: 2 });
    s.intent(PptxIntent::DuplicateSlide { slide: 3 });
    assert_eq!(
        s.model.slides().iter().map(|s| s.hidden).collect::<Vec<_>>(),
        [false, false, true, true, false]
    );
    s.intent(PptxIntent::SetSlideHidden {
        slide: 3,
        hidden: false,
    });
    s.intent(PptxIntent::SetSlideHidden {
        slide: 1,
        hidden: true,
    });
    assert_eq!(
        s.model.slides().iter().map(|s| s.hidden).collect::<Vec<_>>(),
        [true, false, false, true, false]
    );
    let reopened = JadePptx::open(&s.jade.export().unwrap()).unwrap();
    assert_agree_with(&s.model, &s.jade, &reopened, "after export");
}
