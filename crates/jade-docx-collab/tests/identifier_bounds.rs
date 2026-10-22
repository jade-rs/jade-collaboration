use jade_collab_core::grid::Merge;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_docx_collab::{Anchor, DocxModel, DocxOp, NoteKind, StoryId};

fn id(n: u64) -> Id {
    Id::new(n, ReplicaId(1))
}
fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: None,
    }
}

#[test]
fn overflowing_text_and_story_ids_are_rejected_atomically() {
    for op in [
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: id(u64::MAX),
            text: "ab".into(),
        },
        DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: None,
            first: id(u64::MAX),
            text: "ab".into(),
            date: None,
        },
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: None,
            id: id(u64::MAX),
            text: "x".into(),
        },
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: None,
            id: id(u64::MAX),
            text: "x".into(),
            width_px: 100,
            height_px: 100,
        },
    ] {
        let mut model = DocxModel::from_paragraphs(&["hello"]);
        let before = model.clone();
        assert!(model.apply(&op, &ctx()).is_err(), "{op:?}");
        assert_eq!(model, before, "{op:?}");
    }
}

#[test]
fn overflowing_merge_and_unmerge_story_ids_leave_the_table_unchanged() {
    let mut model = DocxModel::from_paragraphs(&["hello"]);
    model
        .apply(
            &DocxOp::InsertTable {
                anchor: Anchor::Start,
                id: id(100),
                rows: vec![id(101), id(102), id(103)],
                columns: vec![id(110), id(111)],
            },
            &ctx(),
        )
        .unwrap();
    let merge = Merge {
        top: id(101),
        bottom: id(103),
        left: id(110),
        right: id(111),
    };
    let before = model.clone();
    assert!(model
        .apply(
            &DocxOp::MergeCells {
                table: id(100),
                merge,
                first: id(u64::MAX)
            },
            &ctx()
        )
        .is_err());
    assert_eq!(model, before);
    model
        .apply(
            &DocxOp::MergeCells {
                table: id(100),
                merge,
                first: id(200),
            },
            &ctx(),
        )
        .unwrap();
    let before = model.clone();
    assert!(model
        .apply(
            &DocxOp::UnmergeCells {
                table: id(100),
                top: id(101),
                left: id(110),
                first: id(u64::MAX)
            },
            &ctx()
        )
        .is_err());
    assert_eq!(model, before);
}

#[test]
fn a_checked_replacement_returns_no_partial_edit_when_its_second_allocation_fails() {
    use jade_collab_core::id::{Clock, IdError};
    use jade_docx_collab::intent::{try_resolve, IntentError, IntentOp};
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    model
        .apply(&DocxOp::SetTracking { on: true }, &ctx())
        .unwrap();
    let before = model.clone();
    let mut clock = Clock::new(ReplicaId(1));
    clock.observe_lamport(u64::MAX - 1);
    let error = try_resolve(
        &model,
        &IntentOp::ReplaceRange {
            start_paragraph: 0,
            start_offset: 0,
            end_paragraph: 0,
            end_offset: 1,
            text: "xy".into(),
        },
        &mut |n| clock.try_reserve(n),
    )
    .unwrap_err();
    assert_eq!(error, IntentError::Identifiers(IdError::Exhausted));
    assert_eq!(
        clock.now(),
        u64::MAX,
        "the deletion reserved the last id before typing failed"
    );
    assert_eq!(model, before);
}

#[test]
fn an_unbounded_delete_count_deletes_only_the_remaining_characters() {
    use jade_collab_core::id::Clock;
    use jade_docx_collab::intent::{try_resolve, IntentOp};
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let intent: IntentOp = serde_json::from_value(serde_json::json!({
        "type": "delete_text", "paragraph": 0, "offset": 1, "direction": "forward", "count": usize::MAX,
    })).unwrap();
    let mut clock = Clock::new(ReplicaId(1));
    for op in try_resolve(&model, &intent, &mut |n| clock.try_reserve(n)).unwrap() {
        model.apply(&op, &ctx()).unwrap();
    }
    assert_eq!(model.paragraph_texts(), ["a"]);
}

#[test]
fn an_empty_text_box_can_use_the_last_id_without_needing_a_character_id() {
    let mut model = DocxModel::from_paragraphs(&["hello"]);
    model
        .apply(
            &DocxOp::InsertTextBox {
                story: StoryId::Body,
                parent: None,
                id: id(u64::MAX),
                text: String::new(),
                width_px: 100,
                height_px: 100,
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(model.max_lamport(), u64::MAX);
    assert_eq!(model.paragraph_texts(), ["hello", ""]);
}
