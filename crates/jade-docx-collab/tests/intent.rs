//! The index-addressed intent API (migrated from jade-core's prototype
//! `CollaborativeOperation`): what an agent or an editor sends in terms of
//! paragraph indices and UTF-8 byte offsets, resolved against the replica's
//! current view into identifier-addressed operations.

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_docx_collab::intent::{
    lane_for_command, resolve, IntentError, IntentOp, UnsupportedCommand,
};
use jade_docx_collab::{
    Anchor, DocxModel, DocxOp, FormatChange, ParagraphAlignment, ParagraphStory, StoryId,
};
use jade_docx_headless_api::{
    HeadlessCommand, HeadlessDeleteDirection, HeadlessTableCellSelection,
};

fn body(index: usize) -> ParagraphStory {
    ParagraphStory::Body { index }
}

fn ids() -> impl FnMut(u64) -> Id {
    let mut next = 1_000u64;
    move |n| {
        let id = Id::new(next, ReplicaId(7));
        next += n;
        id
    }
}

fn run(model: &mut DocxModel, op: IntentOp) -> Result<Vec<DocxOp>, IntentError> {
    // Like a replica's clock: always ahead of everything in the view.
    let mut next = model.max_lamport() + 1;
    let ops = resolve(model, &op, &mut |n| {
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
            .expect("resolved ops apply to the view they came from");
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
            &IntentOp::InsertText {
                paragraph: 3,
                offset: 0,
                text: "x".into()
            },
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
    let op: IntentOp =
        serde_json::from_str(r#"{"type":"insert_paragraph_break","paragraph":1,"offset":0}"#)
            .unwrap();
    assert_eq!(
        op,
        IntentOp::InsertParagraphBreak {
            paragraph: 1,
            offset: 0,
            count: 1
        }
    );
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
    assert_eq!(
        lane_for_command(&HeadlessCommand::InsertText {
            paragraph: 0,
            offset: 0,
            text: "a".into()
        }),
        Lane::Convergent
    );
}

// --- formatting and comments ------------------------------------------------

fn bold() -> FormatChange {
    FormatChange::Bold { value: true }
}

#[test]
fn format_resolves_to_the_characters_between_the_offsets() {
    let model = DocxModel::from_paragraphs(&["Hello", "日本語"]);
    let p = model.paragraphs();
    let ops = resolve(
        &model,
        &IntentOp::Format {
            start_paragraph: 0,
            start_offset: 1,
            end_paragraph: 1,
            end_offset: 6,
            change: bold(),
        },
        &mut ids(),
    )
    .unwrap();
    assert_eq!(
        ops,
        vec![DocxOp::Format {
            story: StoryId::Body,
            start: p[0].chars[1],
            end: p[1].chars[1],
            change: bold(),
        }]
    );
    // A collapsed range formats nothing.
    let ops = resolve(
        &model,
        &IntentOp::Format {
            start_paragraph: 0,
            start_offset: 2,
            end_paragraph: 0,
            end_offset: 2,
            change: bold(),
        },
        &mut ids(),
    )
    .unwrap();
    assert_eq!(ops, vec![]);
    for (start_offset, end_offset) in [(4, 1), (1, 9)] {
        assert!(resolve(
            &model,
            &IntentOp::Format {
                start_paragraph: 0,
                start_offset,
                end_paragraph: 0,
                end_offset,
                change: bold(),
            },
            &mut ids(),
        )
        .is_err());
    }
}

#[test]
fn format_across_a_table_formats_every_story_it_crosses() {
    let mut model = DocxModel::from_paragraphs(&["before", "after"]);
    let mark = model.paragraphs()[0].mark.unwrap();
    let id = |n| Id::new(n, ReplicaId(1));
    model
        .apply(
            &DocxOp::InsertTable {
                anchor: Anchor::After(mark),
                id: id(200),
                rows: vec![id(201)],
                columns: vec![id(211), id(212)],
            },
            &ApplyContext {
                origin: ReplicaId(1),
                base_seq: 0,
                seq: None,
            },
        )
        .unwrap();
    run(
        &mut model,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 0,
            text: "cell".into(),
        },
    )
    .unwrap();
    assert_eq!(model.paragraph_texts(), ["before", "cell", "", "after"]);
    let ops = run(
        &mut model,
        IntentOp::Format {
            start_paragraph: 0,
            start_offset: 3,
            end_paragraph: 3,
            end_offset: 2,
            change: bold(),
        },
    )
    .unwrap();
    let p = model.paragraphs();
    assert_eq!(
        ops,
        vec![
            DocxOp::Format {
                story: StoryId::Body,
                start: p[0].chars[3],
                end: p[3].chars[1],
                change: bold(),
            },
            DocxOp::Format {
                story: p[1].story,
                start: p[1].chars[0],
                end: p[1].chars[3],
                change: bold(),
            },
        ],
        "the empty cell has nothing to format"
    );
}

#[test]
fn paragraph_formatting_reaches_empty_paragraphs_through_their_marks() {
    let mut model = DocxModel::from_paragraphs(&["a", "", "c", ""]);
    let center = FormatChange::Alignment {
        alignment: ParagraphAlignment::Center,
    };
    let p = model.paragraphs();
    let ops = run(
        &mut model,
        IntentOp::FormatParagraphs {
            start_paragraph: 1,
            end_paragraph: 2,
            change: center.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        ops,
        vec![DocxOp::Format {
            story: StoryId::Body,
            start: p[1].mark.unwrap(),
            end: p[2].mark.unwrap(),
            change: center.clone(),
        }]
    );
    // The story's last paragraph has no mark: while it is empty there is
    // nothing to name it by.
    let err = resolve(
        &model,
        &IntentOp::FormatParagraphs {
            start_paragraph: 2,
            end_paragraph: 3,
            change: center,
        },
        &mut ids(),
    )
    .unwrap_err();
    assert_eq!(err, IntentError::EmptyParagraph(3));
}

#[test]
fn invalid_format_values_are_refused_before_submitting() {
    let model = DocxModel::from_paragraphs(&["abc"]);
    let err = resolve(
        &model,
        &IntentOp::Format {
            start_paragraph: 0,
            start_offset: 0,
            end_paragraph: 0,
            end_offset: 1,
            change: FormatChange::FontColor {
                color: "blue".into(),
            },
        },
        &mut ids(),
    )
    .unwrap_err();
    assert!(matches!(err, IntentError::InvalidFormat(_)), "{err:?}");
}

#[test]
fn comments_by_index_and_by_id() {
    let mut model = DocxModel::from_paragraphs(&["Hello world"]);
    let ops = run(
        &mut model,
        IntentOp::AddComment {
            start_paragraph: 0,
            start_offset: Some(6),
            end_paragraph: 0,
            end_offset: None,
            text: "which world?".into(),
            author: Some("Ann".into()),
            initials: None,
            date: None,
        },
    )
    .unwrap();
    let p = model.paragraphs();
    let DocxOp::AddComment {
        story,
        start,
        end,
        id,
        content,
    } = &ops[0]
    else {
        panic!("{ops:?}")
    };
    assert_eq!(
        (*story, *start, *end),
        (StoryId::Body, p[0].chars[6], p[0].chars[10])
    );
    assert_eq!(content.author.as_deref(), Some("Ann"));
    let comment = *id;
    assert_eq!(model.comments()[0].id, comment);

    // Comments are named by their model id as `lamport@rN`.
    let key = comment.to_string();
    run(
        &mut model,
        IntentOp::ReplyToComment {
            comment_id: key.clone(),
            text: "this one".into(),
            author: None,
            initials: None,
            date: None,
        },
    )
    .unwrap();
    run(
        &mut model,
        IntentOp::ResolveComment {
            comment_id: key.clone(),
            resolved: true,
        },
    )
    .unwrap();
    assert!(model.comments()[0].resolved);
    assert_eq!(model.comments()[1].parent, Some(comment));
    let ops = run(
        &mut model,
        IntentOp::UpdateComment {
            comment_id: key.clone(),
            text: "that world".into(),
        },
    )
    .unwrap();
    assert_eq!(
        ops,
        [DocxOp::UpdateComment {
            comment,
            text: "that world".into()
        }]
    );
    assert_eq!(model.comments()[0].content.text, "that world");
    run(&mut model, IntentOp::DeleteComment { comment_id: key }).unwrap();
    assert!(model.comments().is_empty());

    for bad in ["nope", "12@r9", "@r1", "1@"] {
        let err = resolve(
            &model,
            &IntentOp::DeleteComment {
                comment_id: bad.into(),
            },
            &mut ids(),
        )
        .unwrap_err();
        assert_eq!(err, IntentError::NoSuchComment(bad.into()));
    }
}

#[test]
fn a_comment_needs_text_in_one_story() {
    let model = DocxModel::from_paragraphs(&["abc"]);
    let add = |start_offset, end_offset| IntentOp::AddComment {
        start_paragraph: 0,
        start_offset: Some(start_offset),
        end_paragraph: 0,
        end_offset: Some(end_offset),
        text: "x".into(),
        author: None,
        initials: None,
        date: None,
    };
    assert_eq!(
        resolve(&model, &add(1, 1), &mut ids()).unwrap_err(),
        IntentError::EmptyRange
    );
    assert_eq!(
        resolve(&model, &add(2, 1), &mut ids()).unwrap_err(),
        IntentError::InvertedRange
    );
}

#[test]
fn formatting_and_comment_commands_project_into_intents() {
    let range = |change| IntentOp::Format {
        start_paragraph: 0,
        start_offset: 1,
        end_paragraph: 2,
        end_offset: 3,
        change,
    };
    let cases = [
        (
            HeadlessCommand::SetBold {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
                value: false,
            },
            range(FormatChange::Bold { value: false }),
        ),
        (
            HeadlessCommand::ApplyItalic {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
                enabled: true,
            },
            range(FormatChange::Italic { value: true }),
        ),
        (
            HeadlessCommand::ApplyUnderline {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
                kind: Some("double".into()),
                enabled: true,
            },
            range(FormatChange::Underline {
                value: true,
                style: Some("double".into()),
            }),
        ),
        (
            HeadlessCommand::ApplyStrike {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
                double: true,
                enabled: true,
            },
            range(FormatChange::Strike {
                value: true,
                double: true,
            }),
        ),
        (
            HeadlessCommand::SetFontSize {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
                font_size: 10.5,
            },
            range(FormatChange::FontSize { half_points: 21 }),
        ),
        (
            HeadlessCommand::SetFontFamily {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
                font_family: "Arial".into(),
            },
            range(FormatChange::FontFamily {
                family: "Arial".into(),
            }),
        ),
        (
            HeadlessCommand::SetFontColor {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
                color: "#FF0000".into(),
            },
            range(FormatChange::FontColor {
                color: "#FF0000".into(),
            }),
        ),
        (
            HeadlessCommand::SetHighlightColor {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
                color: "yellow".into(),
            },
            range(FormatChange::Highlight {
                color: "yellow".into(),
            }),
        ),
        (
            HeadlessCommand::ClearFormatting {
                start_paragraph: 0,
                start_offset: 1,
                end_paragraph: 2,
                end_offset: 3,
            },
            range(FormatChange::ClearFormatting),
        ),
        (
            HeadlessCommand::SetParagraphAlignment {
                start_paragraph: 1,
                end_paragraph: 4,
                alignment: "both".into(),
            },
            IntentOp::FormatParagraphs {
                start_paragraph: 1,
                end_paragraph: 4,
                change: FormatChange::Alignment {
                    alignment: ParagraphAlignment::Justify,
                },
            },
        ),
        (
            HeadlessCommand::SetParagraphStyle {
                start_paragraph: 1,
                end_paragraph: 1,
                style_id: Some("Heading1".into()),
            },
            IntentOp::FormatParagraphs {
                start_paragraph: 1,
                end_paragraph: 1,
                change: FormatChange::ParagraphStyle {
                    style_id: Some("Heading1".into()),
                },
            },
        ),
        (
            HeadlessCommand::AddComment {
                start_paragraph: 0,
                start_offset: None,
                end_paragraph: 1,
                end_offset: Some(2),
                text: "t".into(),
                author: Some("A".into()),
                initials: Some("a".into()),
                date: Some("2026-10-18T00:00:00Z".into()),
            },
            IntentOp::AddComment {
                start_paragraph: 0,
                start_offset: None,
                end_paragraph: 1,
                end_offset: Some(2),
                text: "t".into(),
                author: Some("A".into()),
                initials: Some("a".into()),
                date: Some("2026-10-18T00:00:00Z".into()),
            },
        ),
        (
            HeadlessCommand::ReplyToComment {
                comment_id: "0".into(),
                text: "r".into(),
                author: None,
                initials: None,
                date: None,
            },
            IntentOp::ReplyToComment {
                comment_id: "0".into(),
                text: "r".into(),
                author: None,
                initials: None,
                date: None,
            },
        ),
        (
            HeadlessCommand::ResolveComment {
                comment_id: "0".into(),
                resolved: true,
            },
            IntentOp::ResolveComment {
                comment_id: "0".into(),
                resolved: true,
            },
        ),
        (
            HeadlessCommand::DeleteComment {
                comment_id: "0".into(),
            },
            IntentOp::DeleteComment {
                comment_id: "0".into(),
            },
        ),
        (
            HeadlessCommand::UpdateComment {
                comment_id: "0".into(),
                text: "x".into(),
            },
            IntentOp::UpdateComment {
                comment_id: "0".into(),
                text: "x".into(),
            },
        ),
    ];
    for (command, intent) in cases {
        assert_eq!(lane_for_command(&command), Lane::Sequenced, "{command:?}");
        assert_eq!(IntentOp::try_from(command).unwrap(), intent);
    }
    // Values with no collaborative form stay unsupported, on their lane.
    for command in [
        HeadlessCommand::SetParagraphAlignment {
            start_paragraph: 0,
            end_paragraph: 0,
            alignment: "sideways".into(),
        },
        HeadlessCommand::SetFontSize {
            start_paragraph: 0,
            start_offset: 0,
            end_paragraph: 0,
            end_offset: 1,
            font_size: f32::NAN,
        },
    ] {
        let name = command.name();
        assert_eq!(
            IntentOp::try_from(command).unwrap_err(),
            UnsupportedCommand {
                command_name: name,
                lane: Lane::Sequenced
            }
        );
    }
}

#[test]
fn format_intents_have_a_wire_format() {
    let op: IntentOp = serde_json::from_str(
        r#"{"type":"format","start_paragraph":0,"start_offset":0,"end_paragraph":0,
            "end_offset":3,"change":{"kind":"bold","value":true}}"#,
    )
    .unwrap();
    assert_eq!(
        op,
        IntentOp::Format {
            start_paragraph: 0,
            start_offset: 0,
            end_paragraph: 0,
            end_offset: 3,
            change: bold(),
        }
    );
    let op: IntentOp = serde_json::from_str(
        r#"{"type":"add_comment","start_paragraph":2,"end_paragraph":2,"text":"?"}"#,
    )
    .unwrap();
    assert_eq!(
        op,
        IntentOp::AddComment {
            start_paragraph: 2,
            start_offset: None,
            end_paragraph: 2,
            end_offset: None,
            text: "?".into(),
            author: None,
            initials: None,
            date: None,
        }
    );
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

    /// Resolving a format intent and applying it reports exactly the bytes
    /// the intent named, in every paragraph it crosses.
    #[test]
    fn a_format_intent_formats_exactly_the_bytes_it_names(
        texts in proptest::collection::vec("[ab日 ]{0,5}", 1..5),
        a in proptest::prelude::any::<proptest::sample::Index>(),
        b in proptest::prelude::any::<proptest::sample::Index>(),
    ) {
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let mut model = DocxModel::from_paragraphs(&texts);
        // Every caret as (paragraph, byte offset), in document order.
        let carets: Vec<(usize, usize)> = model
            .paragraph_texts()
            .iter()
            .enumerate()
            .flat_map(|(p, t)| t.char_indices().map(|(i, _)| i).chain([t.len()]).map(move |o| (p, o)).collect::<Vec<_>>())
            .collect();
        let (a, b) = (a.index(carets.len()), b.index(carets.len()));
        let ((sp, so), (ep, eo)) = (carets[a.min(b)], carets[a.max(b)]);
        let ops = run(&mut model, IntentOp::Format {
            start_paragraph: sp, start_offset: so, end_paragraph: ep, end_offset: eo, change: bold(),
        }).unwrap();
        let ctx = ApplyContext { origin: ReplicaId(7), base_seq: 0, seq: None };
        let mut effects = Vec::new();
        for op in &ops {
            effects.extend(model.clone().apply(op, &ctx).unwrap());
        }
        let texts = model.paragraph_texts();
        let expected: Vec<jade_docx_collab::DocxEffect> = (sp..=ep)
            .filter_map(|p| {
                let start = if p == sp { so } else { 0 };
                let end = if p == ep { eo } else { texts[p].len() };
                (start < end).then_some(jade_docx_collab::DocxEffect::FormatText {
                    paragraph: body(p), start, end, change: bold(),
                })
            })
            .collect();
        proptest::prop_assert_eq!(effects, expected);
    }
}

#[test]
fn merge_and_unmerge_table_cells_by_position() {
    use jade_docx_collab::{Anchor, StoryId};
    let mut model = DocxModel::from_paragraphs(&["before", "after"]);
    let mark = model.paragraphs()[0].mark.unwrap();
    let ctx = ApplyContext {
        origin: ReplicaId(7),
        base_seq: 0,
        seq: None,
    };
    let first = Id::new(500, ReplicaId(7));
    model
        .apply(
            &DocxOp::InsertTable {
                anchor: Anchor::After(mark),
                id: first,
                rows: vec![first.offset(1), first.offset(2)],
                columns: vec![first.offset(3), first.offset(4)],
            },
            &ctx,
        )
        .unwrap();
    run(
        &mut model,
        serde_json::from_str(r#"{"type":"merge_table_cells","table":0,"top_row":0,"left_column":0,"bottom_row":1,"right_column":1}"#).unwrap(),
    )
    .unwrap();
    assert_eq!(model.tables()[0].merges, vec![(0, 0, 1, 1)]);
    assert!(run(
        &mut model,
        serde_json::from_str(r#"{"type":"merge_table_cells","table":3,"top_row":0,"left_column":0,"bottom_row":1,"right_column":1}"#).unwrap(),
    )
    .is_err());
    run(
        &mut model,
        serde_json::from_str(r#"{"type":"unmerge_table_cells","table":0,"row":0,"column":0}"#)
            .unwrap(),
    )
    .unwrap();
    assert!(model.tables()[0].merges.is_empty());
    assert!(model
        .paragraphs()
        .iter()
        .any(|p| matches!(p.story, StoryId::Split { .. })));
}
