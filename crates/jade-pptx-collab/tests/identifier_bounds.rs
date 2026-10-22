use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_pptx_collab::{PptxModel, PptxOp, StoryId};

#[test]
fn an_overflowing_text_run_is_rejected_atomically() {
    let mut model = PptxModel::from_texts(&[&["hello"]]);
    let slide = model.slides().remove(0);
    let story = StoryId::Shape {
        slide: slide.id,
        shape: slide.shapes[0].id,
    };
    let before = model.clone();
    let op = PptxOp::InsertText {
        story,
        parent: None,
        first: Id::new(u64::MAX, ReplicaId(1)),
        text: "ab".into(),
    };
    assert!(model
        .apply(
            &op,
            &ApplyContext {
                origin: ReplicaId(1),
                base_seq: 0,
                seq: None
            }
        )
        .is_err());
    assert_eq!(model, before);
}

#[test]
fn checked_multiline_typing_reports_exhaustion_without_a_partial_operation_list() {
    use jade_collab_core::id::{Clock, IdError};
    use jade_pptx_collab::intent::{try_resolve, IntentError, PptxIntent, Target};
    let model = PptxModel::from_texts(&[&["hello"]]);
    let before = model.clone();
    let shape = model.slides()[0].shapes[0].number;
    let mut clock = Clock::new(ReplicaId(1));
    clock.observe_lamport(u64::MAX - 1);
    let error = try_resolve(
        &model,
        &PptxIntent::InsertText {
            slide: 1,
            target: Target::Shape(shape),
            paragraph: 0,
            offset: 0,
            text: "a\nb".into(),
        },
        &mut |n| clock.try_reserve(n),
    )
    .unwrap_err();
    assert_eq!(error, IntentError::Identifiers(IdError::Exhausted));
    assert_eq!(model, before);
}

#[test]
fn an_unbounded_delete_count_deletes_only_the_remaining_characters() {
    use jade_collab_core::id::Clock;
    use jade_pptx_collab::intent::{try_resolve, PptxIntent};
    let mut model = PptxModel::from_texts(&[&["hello"]]);
    let shape = model.slides()[0].shapes[0].number;
    let intent: PptxIntent = serde_json::from_value(serde_json::json!({
        "type": "delete_text", "slide": 1, "target": {"shape": shape}, "paragraph": 0, "offset": 1,
        "direction": "forward", "count": usize::MAX,
    }))
    .unwrap();
    let mut clock = Clock::new(ReplicaId(1));
    for op in try_resolve(&model, &intent, &mut |n| clock.try_reserve(n)).unwrap() {
        model
            .apply(
                &op,
                &ApplyContext {
                    origin: ReplicaId(1),
                    base_seq: 0,
                    seq: None,
                },
            )
            .unwrap();
    }
    assert_eq!(model.slides()[0].shapes[0].text.as_deref(), Some("h"));
}
