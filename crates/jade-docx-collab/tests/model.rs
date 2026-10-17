//! The model on its own: paragraphs as RGA runs split by marks, tables
//! anchored to marks, and the lanes each operation travels.

use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_docx_collab::{Anchor, DocxEffect, DocxModel, DocxOp, StoryId};

fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: None,
    }
}

fn id(n: u64) -> Id {
    Id::new(n, ReplicaId(1))
}

fn texts(model: &DocxModel) -> Vec<String> {
    model.paragraph_texts()
}

#[test]
fn builds_paragraphs_from_text() {
    let model = DocxModel::from_paragraphs(&["Hello", "", "world"]);
    assert_eq!(texts(&model), ["Hello", "", "world"]);
    let paragraphs = model.paragraphs();
    assert!(paragraphs[0].mark.is_some());
    assert!(
        paragraphs[2].mark.is_none(),
        "the last paragraph has no mark atom"
    );
    assert!(model.max_lamport() > 0);
}

#[test]
fn inserting_text_reports_where_it_landed() {
    let mut model = DocxModel::from_paragraphs(&["Hello", "world"]);
    let o = model.paragraphs()[1].chars[0];
    let effect = model
        .apply(
            &DocxOp::InsertText {
                story: StoryId::Body,
                parent: Some(o),
                first: id(100),
                text: "-x".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model), ["Hello", "w-xorld"]);
    assert_eq!(
        effect,
        vec![DocxEffect::InsertText {
            paragraph: 1,
            offset: 1,
            text: "-x".into()
        }]
    );
}

#[test]
fn offsets_are_utf8_bytes() {
    let mut model = DocxModel::from_paragraphs(&["日本"]);
    let hon = model.paragraphs()[0].chars[1];
    let effect = model
        .apply(
            &DocxOp::InsertText {
                story: StoryId::Body,
                parent: Some(hon),
                first: id(100),
                text: "語".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![DocxEffect::InsertText {
            paragraph: 0,
            offset: 6,
            text: "語".into()
        }]
    );
}

#[test]
fn deleting_reports_ranges_from_the_end() {
    let mut model = DocxModel::from_paragraphs(&["abcdef", "gh"]);
    let p = model.paragraphs();
    let ids = vec![p[0].chars[1], p[0].chars[2], p[0].chars[4], p[1].chars[0]];
    let effect = model
        .apply(
            &DocxOp::DeleteText {
                story: StoryId::Body,
                ids,
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model), ["adf", "h"]);
    assert_eq!(
        effect,
        vec![
            DocxEffect::DeleteRange {
                paragraph: 1,
                start: 0,
                end: 1
            },
            DocxEffect::DeleteRange {
                paragraph: 0,
                start: 4,
                end: 5
            },
            DocxEffect::DeleteRange {
                paragraph: 0,
                start: 1,
                end: 3
            },
        ]
    );
}

#[test]
fn deleting_already_deleted_text_is_harmless() {
    let mut model = DocxModel::from_paragraphs(&["ab"]);
    let a = model.paragraphs()[0].chars[0];
    let op = DocxOp::DeleteText {
        story: StoryId::Body,
        ids: vec![a],
    };
    model.apply(&op, &ctx()).unwrap();
    assert_eq!(model.apply(&op, &ctx()).unwrap(), vec![]);
    assert_eq!(texts(&model), ["b"]);
}

#[test]
fn convergent_ops_cannot_delete_paragraph_marks() {
    let mut model = DocxModel::from_paragraphs(&["a", "b"]);
    let mark = model.paragraphs()[0].mark.unwrap();
    assert!(model
        .apply(
            &DocxOp::DeleteText {
                story: StoryId::Body,
                ids: vec![mark]
            },
            &ctx()
        )
        .is_err());
}

#[test]
fn split_and_join() {
    let mut model = DocxModel::from_paragraphs(&["Hello world"]);
    let space = model.paragraphs()[0].chars[5];
    let effect = model
        .apply(
            &DocxOp::SplitParagraph {
                story: StoryId::Body,
                parent: Some(space),
                id: id(100),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model), ["Hello ", "world"]);
    assert_eq!(
        effect,
        vec![DocxEffect::SplitParagraph {
            paragraph: 0,
            offset: 6
        }]
    );

    let effect = model
        .apply(
            &DocxOp::JoinParagraph {
                story: StoryId::Body,
                mark: id(100),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model), ["Hello world"]);
    assert_eq!(
        effect,
        vec![DocxEffect::JoinParagraph {
            paragraph: 0,
            offset: 6
        }]
    );
}

#[test]
fn lanes() {
    let story = StoryId::Body;
    assert_eq!(
        DocxModel::lane(&DocxOp::InsertText {
            story,
            parent: None,
            first: id(1),
            text: "a".into()
        }),
        Lane::Convergent
    );
    assert_eq!(
        DocxModel::lane(&DocxOp::SplitParagraph {
            story,
            parent: None,
            id: id(1)
        }),
        Lane::Convergent
    );
    assert_eq!(
        DocxModel::lane(&DocxOp::JoinParagraph { story, mark: id(1) }),
        Lane::Sequenced
    );
    assert_eq!(
        DocxModel::lane(&DocxOp::DeleteTable { table: id(1) }),
        Lane::Sequenced
    );
}

fn with_table() -> (DocxModel, Id) {
    let mut model = DocxModel::from_paragraphs(&["before", "after"]);
    let mark = model.paragraphs()[0].mark.unwrap();
    model
        .apply(
            &DocxOp::InsertTable {
                anchor: Anchor::After(mark),
                id: id(200),
                rows: vec![id(201), id(202)],
                columns: vec![id(211), id(212)],
            },
            &ctx(),
        )
        .unwrap();
    (model, id(200))
}

#[test]
fn a_table_sits_between_paragraphs_and_its_cells_are_paragraphs() {
    let (model, table) = with_table();
    assert_eq!(texts(&model), ["before", "", "", "", "", "after"]);
    let tables = model.tables();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].id, table);
    assert_eq!(tables[0].rows, vec![id(201), id(202)]);
    let cell = model.paragraphs()[2].story;
    assert_eq!(
        cell,
        StoryId::Cell {
            table,
            row: id(201),
            column: id(212)
        }
    );
}

#[test]
fn a_paragraph_followed_by_a_table_cannot_be_joined() {
    let (mut model, _) = with_table();
    let mark = model.paragraphs()[0].mark.unwrap();
    let err = model
        .apply(
            &DocxOp::JoinParagraph {
                story: StoryId::Body,
                mark,
            },
            &ctx(),
        )
        .unwrap_err();
    assert!(err.message().contains("table"), "{err}");
}

#[test]
fn a_split_keeps_the_table_after_the_second_half() {
    let (mut model, _) = with_table();
    let first = model.paragraphs()[0].chars[2];
    model
        .apply(
            &DocxOp::SplitParagraph {
                story: StoryId::Body,
                parent: Some(first),
                id: id(300),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(texts(&model), ["bef", "ore", "", "", "", "", "after"]);
}

#[test]
fn table_rows_map_to_jade_indices() {
    let (mut model, table) = with_table();
    let effect = model
        .apply(
            &DocxOp::Table {
                table,
                op: GridOp::InsertRow {
                    id: id(203),
                    after: Some(id(201)),
                },
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![DocxEffect::InsertTableRow {
            table: 0,
            row: 1,
            template: Some(0)
        }]
    );
    assert_eq!(texts(&model).len(), 8);
    let effect = model
        .apply(
            &DocxOp::Table {
                table,
                op: GridOp::DeleteColumn { id: id(211) },
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![DocxEffect::DeleteTableColumn {
            table: 0,
            column: 0
        }]
    );
    assert_eq!(texts(&model), ["before", "", "", "", "after"]);
}

#[test]
fn text_typed_into_a_concurrently_deleted_row_lands_nowhere_visible() {
    let (mut model, table) = with_table();
    model
        .apply(
            &DocxOp::Table {
                table,
                op: GridOp::DeleteRow { id: id(201) },
            },
            &ctx(),
        )
        .unwrap();
    let effect = model
        .apply(
            &DocxOp::InsertText {
                story: StoryId::Cell {
                    table,
                    row: id(201),
                    column: id(211),
                },
                parent: None,
                first: id(400),
                text: "lost".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(effect, vec![]);
    assert!(!texts(&model).iter().any(|t| t == "lost"));
}

#[test]
fn merged_cells_are_not_supported_yet() {
    let (mut model, table) = with_table();
    let err = model
        .apply(
            &DocxOp::Table {
                table,
                op: GridOp::Merge(Merge {
                    top: id(201),
                    left: id(211),
                    bottom: id(202),
                    right: id(211),
                }),
            },
            &ctx(),
        )
        .unwrap_err();
    assert!(err.message().contains("merge"), "{err}");
}

#[test]
fn a_new_table_lands_right_after_its_paragraph_before_older_ones() {
    let (mut model, _) = with_table();
    let mark = model.paragraphs()[0].mark.unwrap();
    model
        .apply(
            &DocxOp::InsertTable {
                anchor: Anchor::After(mark),
                id: id(500),
                rows: vec![id(501)],
                columns: vec![id(511)],
            },
            &ctx(),
        )
        .unwrap();
    let tables = model.tables();
    assert_eq!(tables[0].id, id(500));
    assert_eq!(tables[1].id, id(200));
}

#[test]
fn the_model_round_trips_through_json() {
    let (model, _) = with_table();
    let json = serde_json::to_string(&model).unwrap();
    let back: DocxModel = serde_json::from_str(&json).unwrap();
    assert_eq!(back, model);
}
