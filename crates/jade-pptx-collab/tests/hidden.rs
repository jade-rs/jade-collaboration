//! Hidden slides: whether a slide is skipped in a slide show is one
//! last-writer-wins register per slide, on the convergent lane.

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_pptx_collab::{PptxEffect, PptxModel, PptxOp, StoryId};

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
    PptxModel::from_texts(&[&["Title"], &["Second"]])
}

fn hidden(model: &PptxModel) -> Vec<bool> {
    model.slides().iter().map(|s| s.hidden).collect()
}

#[test]
fn slides_start_shown_and_hiding_travels_the_convergent_lane() {
    let model = deck();
    assert_eq!(hidden(&model), [false, false]);
    let op = PptxOp::SetSlideHidden {
        slide: model.slides()[0].id,
        stamp: id(1, 1),
        hidden: true,
    };
    assert_eq!(PptxModel::lane(&op), Lane::Convergent);
}

#[test]
fn hiding_reports_the_slides_position_once_the_value_changes() {
    let mut model = deck();
    let slide = model.slides()[1].id;
    let hide = |stamp, hidden| PptxOp::SetSlideHidden {
        slide,
        stamp,
        hidden,
    };
    let effect = model.apply(&hide(id(1, 1), true), &at(1, 0, None)).unwrap();
    assert_eq!(
        effect,
        vec![PptxEffect::SetSlideHidden {
            slide: 1,
            hidden: true
        }]
    );
    assert_eq!(hidden(&model), [false, true]);
    // The same value again, or an older write: nothing for jade to do.
    assert_eq!(
        model.apply(&hide(id(2, 1), true), &at(1, 0, None)).unwrap(),
        vec![]
    );
    assert_eq!(
        model
            .apply(&hide(id(0, 2), false), &at(2, 0, None))
            .unwrap(),
        vec![]
    );
    assert_eq!(hidden(&model), [false, true]);
    assert!(model.max_lamport() >= 1002);
}

/// Two replicas set one slide's visibility concurrently: every order of
/// the two writes ends at the newer stamp's value.
#[test]
fn concurrent_writes_converge_on_the_newest() {
    let base = deck();
    let slide = base.slides()[0].id;
    let a = PptxOp::SetSlideHidden {
        slide,
        stamp: id(5, 1),
        hidden: true,
    };
    let b = PptxOp::SetSlideHidden {
        slide,
        stamp: id(5, 2),
        hidden: false,
    };
    let mut ab = base.clone();
    ab.apply(&a, &at(1, 0, Some(1))).unwrap();
    ab.apply(&b, &at(2, 0, Some(2))).unwrap();
    let mut ba = base.clone();
    ba.apply(&b, &at(2, 0, Some(1))).unwrap();
    ba.apply(&a, &at(1, 0, Some(2))).unwrap();
    assert_eq!(ab, ba);
    // (lamport 1005, replica 2) is the newer stamp.
    assert_eq!(hidden(&ab), [false, false]);
}

#[test]
fn hiding_a_deleted_slide_lands_invisibly() {
    let mut model = deck();
    let slide = model.slides()[0].id;
    model
        .apply(&PptxOp::DeleteSlide { slide }, &at(1, 0, Some(1)))
        .unwrap();
    let effect = model
        .apply(
            &PptxOp::SetSlideHidden {
                slide,
                stamp: id(1, 2),
                hidden: true,
            },
            &at(2, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(effect, vec![]);
    let missing = PptxOp::SetSlideHidden {
        slide: id(99, 9),
        stamp: id(2, 2),
        hidden: true,
    };
    assert!(model.apply(&missing, &at(2, 0, Some(3))).is_err());
}

/// A copy starts as hidden as its source; a write whose author had not seen
/// the duplication reaches both, like every other convergent edit.
#[test]
fn duplication_copies_visibility_and_unseen_writes_reach_the_copy() {
    let base = deck();
    let slide = base.slides()[0].id;
    let hide = PptxOp::SetSlideHidden {
        slide,
        stamp: id(1, 1),
        hidden: true,
    };
    let dup = PptxOp::DuplicateSlide {
        slide,
        id: id(10, 1),
    };
    let mut model = base.clone();
    model.apply(&hide, &at(1, 0, Some(1))).unwrap();
    model.apply(&dup, &at(1, 1, Some(2))).unwrap();
    assert_eq!(hidden(&model), [true, true, false]);

    // Replica 2 shows the slide without having seen the duplication.
    let show = PptxOp::SetSlideHidden {
        slide,
        stamp: id(3, 2),
        hidden: false,
    };
    let effect = model.apply(&show, &at(2, 1, Some(3))).unwrap();
    assert_eq!(
        effect,
        vec![
            PptxEffect::SetSlideHidden {
                slide: 0,
                hidden: false
            },
            PptxEffect::SetSlideHidden {
                slide: 1,
                hidden: false
            }
        ]
    );
    // A write made after seeing it stays on its slide.
    let hide_again = PptxOp::SetSlideHidden {
        slide,
        stamp: id(4, 2),
        hidden: true,
    };
    model.apply(&hide_again, &at(2, 2, Some(4))).unwrap();
    assert_eq!(hidden(&model), [true, false, false]);
    // Text is untouched by any of it.
    assert_eq!(
        model.text(StoryId::Notes { slide }).as_deref(),
        Some(""),
        "notes stay"
    );
}

#[test]
fn visibility_round_trips_through_json_and_older_models_read_as_shown() {
    let mut model = deck();
    let slide = model.slides()[0].id;
    model
        .apply(
            &PptxOp::SetSlideHidden {
                slide,
                stamp: id(1, 1),
                hidden: true,
            },
            &at(1, 0, Some(1)),
        )
        .unwrap();
    let json = serde_json::to_string(&model).unwrap();
    assert_eq!(serde_json::from_str::<PptxModel>(&json).unwrap(), model);

    // A checkpoint written before slides could be hidden has no register.
    let mut value: serde_json::Value = serde_json::to_value(deck()).unwrap();
    for pair in value["slides"].as_array_mut().unwrap() {
        pair[1].as_object_mut().unwrap().remove("hidden");
    }
    let old: PptxModel = serde_json::from_value(value).unwrap();
    assert_eq!(hidden(&old), [false, false]);
}
