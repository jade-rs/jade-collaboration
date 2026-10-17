//! The index-addressed intent API (migrated from jade-core's prototype
//! `CollaborativeOperation`): what an agent or an editor sends in terms of
//! paragraph indices and UTF-8 byte offsets, resolved against the replica's
//! current view into identifier-addressed operations.

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_docx_collab::intent::{lane_for_command, resolve, IntentError, IntentOp};
use jade_docx_collab::{DocxModel, DocxOp};
use jade_docx_headless_api::{HeadlessCommand, HeadlessDeleteDirection, HeadlessTableCellSelection};

fn ids() -> impl FnMut(u64) -> Id {
    let mut next = 1_000u64;
    move |n| {
        let id = Id::new(next, ReplicaId(7));
        next += n;
        id
    }
}

fn run(model: &mut DocxModel, op: IntentOp) -> Result<Vec<DocxOp>, IntentError> {
    let ops = resolve(model, &op, &mut ids())?;
    let ctx = ApplyContext {
        origin: ReplicaId(7),
        base_seq: 0,
        seq: None,
    };
    for op in &ops {
        model.apply(op, &ctx).expect("resolved ops apply to the view they came from");
    }
    Ok(ops)
}

#[test]
fn insert_text_at_a_byte_offset() {
    let mut model = DocxModel::from_paragraphs(&["Hello", "world"]);
    run(
        &mut model,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 5,
            text: "!".into(),
        },
    )
    .unwrap();
    run(
        &mut model,
        IntentOp::InsertText {
            paragraph: 0,
            offset: 0,
            text: ">".into(),
        },
    )
    .unwrap();
    run(
        &mut model,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 0,
            text: "<".into(),
        },
    )
    .unwrap();
    assert_eq!(model.paragraph_texts(), [">Hello", "<world!"]);
}

#[test]
fn rejects_offsets_inside_a_character_or_past_the_end() {
    let model = DocxModel::from_paragraphs(&["日本"]);
    for offset in [1, 7] {
        let err = resolve(
            &model,
            &IntentOp::InsertText {
                paragraph: 0,
                offset,
                text: "x".into(),
            },
            &mut ids(),
        )
        .unwrap_err();
        assert!(matches!(err, IntentError::BadOffset { .. }), "{err:?}");
    }
    assert!(matches!(
        resolve(
            &model,
            &IntentOp::InsertText { paragraph: 3, offset: 0, text: "x".into() },
            &mut ids()
        ),
        Err(IntentError::NoSuchParagraph(3))
    ));
}

#[test]
fn delete_forward_and_backward_by_characters() {
    let mut model = DocxModel::from_paragraphs(&["abcdef"]);
    run(
        &mut model,
        IntentOp::DeleteText {
            paragraph: 0,
            offset: 1,
            direction: HeadlessDeleteDirection::Forward,
            count: 2,
        },
    )
    .unwrap();
    assert_eq!(model.paragraph_texts(), ["adef"]);
    run(
        &mut model,
        IntentOp::DeleteText {
            paragraph: 0,
            offset: 4,
            direction: HeadlessDeleteDirection::Backward,
            count: 1,
        },
    )
    .unwrap();
    assert_eq!(model.paragraph_texts(), ["ade"]);
}

#[test]
fn deleting_across_a_paragraph_end_joins_paragraphs() {
    let mut model = DocxModel::from_paragraphs(&["ab", "cd"]);
    let ops = run(
        &mut model,
        IntentOp::DeleteText {
            paragraph: 0,
            offset: 1,
            direction: HeadlessDeleteDirection::Forward,
            count: 2,
        },
    )
    .unwrap();
    assert_eq!(model.paragraph_texts(), ["acd"]);
    assert!(ops.iter().any(|op| DocxModel::lane(op) == Lane::Sequenced));
    assert!(ops.iter().any(|op| DocxModel::lane(op) == Lane::Convergent));
}

#[test]
fn replace_range_across_paragraphs() {
    let mut model = DocxModel::from_paragraphs(&["Hello", "big", "world"]);
    run(
        &mut model,
        IntentOp::ReplaceRange {
            start_paragraph: 0,
            start_offset: 4,
            end_paragraph: 2,
            end_offset: 1,
            text: "o, W".into(),
        },
    )
    .unwrap();
    assert_eq!(model.paragraph_texts(), ["Hello, World"]);
}

#[test]
fn paragraph_breaks_split_at_the_caret() {
    let mut model = DocxModel::from_paragraphs(&["HelloWorld"]);
    run(
        &mut model,
        IntentOp::InsertParagraphBreak {
            paragraph: 0,
            offset: 5,
            count: 2,
        },
    )
    .unwrap();
    assert_eq!(model.paragraph_texts(), ["Hello", "", "World"]);
}

#[test]
fn intent_ops_use_the_prototype_wire_format() {
    let op: IntentOp = serde_json::from_str(
        r#"{"type":"delete_text","paragraph":0,"offset":6,"direction":"forward","count":2}"#,
    )
    .unwrap();
    assert_eq!(
        op,
        IntentOp::DeleteText {
            paragraph: 0,
            offset: 6,
            direction: HeadlessDeleteDirection::Forward,
            count: 2
        }
    );
    let op: IntentOp = serde_json::from_str(r#"{"type":"insert_paragraph_break","paragraph":1,"offset":0}"#).unwrap();
    assert_eq!(op, IntentOp::InsertParagraphBreak { paragraph: 1, offset: 0, count: 1 });
}

#[test]
fn headless_commands_project_into_intents_or_name_their_lane() {
    let text = HeadlessCommand::InsertText {
        paragraph: 0,
        offset: 3,
        text: "abc".into(),
    };
    assert_eq!(
        IntentOp::try_from(text).unwrap(),
        IntentOp::InsertText {
            paragraph: 0,
            offset: 3,
            text: "abc".into()
        }
    );
    let merge = HeadlessCommand::MergeTableCells {
        table_order_index: 0,
        selected_cells: vec![HeadlessTableCellSelection {
            row: 0,
            column: 0,
            column_span: 1,
        }],
    };
    assert_eq!(lane_for_command(&merge), Lane::Sequenced);
    let err = IntentOp::try_from(merge).unwrap_err();
    assert_eq!(err.command_name, "merge_table_cells");
    let bold = HeadlessCommand::SetBold {
        start_paragraph: 0,
        start_offset: 0,
        end_paragraph: 0,
        end_offset: 1,
        value: true,
    };
    assert_eq!(lane_for_command(&bold), Lane::Sequenced);
    assert_eq!(lane_for_command(&HeadlessCommand::InsertText { paragraph: 0, offset: 0, text: "a".into() }), Lane::Convergent);
}
