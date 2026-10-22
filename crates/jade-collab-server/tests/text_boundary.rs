//! A committed text operation must still export and reopen with the same text.
use jade_collab_core::id::Id;
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::{Document, Sequencer, SubmitOutcome};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx, StoryId};

fn docx() -> Sequencer<DocxModel, JadeDocx> {
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx"),
    )
    .unwrap();
    let jade = JadeDocx::load(&bytes).unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    Sequencer::with_materializer(model, jade)
}
#[test]
fn xml_forbidden_docx_text_is_refused_before_it_can_poison_a_committed_file() {
    for tracked in [false, true] {
        for ch in ['\0', '\u{1}', '\u{b}', '\u{c}', '\u{fffe}', '\u{ffff}'] {
            let mut seq = docx();
            let welcome = seq.join();
            let before = seq.document().clone();
            let file = seq.materializer().export(seq.document()).unwrap();
            let first = Id::new(before.max_lamport() + 1, welcome.replica);
            let text = format!("before{ch}after");
            let op = if tracked {
                DocxOp::TrackInsertText {
                    story: StoryId::Body,
                    parent: None,
                    first,
                    text,
                    date: None,
                }
            } else {
                DocxOp::InsertText {
                    story: StoryId::Body,
                    parent: None,
                    first,
                    text,
                }
            };
            let result = seq.submit(
                welcome.replica,
                Submission {
                    client_seq: 1,
                    base_seq: 0,
                    op,
                },
            );
            assert!(
                matches!(result, SubmitOutcome::Rejected(_)),
                "XML-forbidden U+{:04X} committed (tracked={tracked}): {result:?}",
                ch as u32
            );
            assert_eq!(seq.document(), &before);
            assert_eq!(seq.materializer().export(seq.document()).unwrap(), file);
            assert_eq!(seq.seq(), 0);
            assert_eq!(seq.processed_client_seq(welcome.replica), 1);
            assert!(seq.faults().is_empty());
        }
    }
}
#[test]
fn admitted_docx_text_preserves_unicode_entities_tabs_and_line_breaks_on_reopen() {
    for text in [
        "日本🙂e\u{301} & < > \" '",
        "a\tb",
        "a\nb",
        "a\rb",
        "a\r\nb",
    ] {
        let mut seq = docx();
        let welcome = seq.join();
        let first = Id::new(seq.document().max_lamport() + 1, welcome.replica);
        let result = seq.submit(
            welcome.replica,
            Submission {
                client_seq: 1,
                base_seq: 0,
                op: DocxOp::InsertText {
                    story: StoryId::Body,
                    parent: None,
                    first,
                    text: text.into(),
                },
            },
        );
        assert!(
            matches!(result, SubmitOutcome::Committed(_)),
            "valid text refused: {text:?}: {result:?}"
        );
        assert!(seq.faults().is_empty(), "{text:?}: {:?}", seq.faults());
        let file = seq.materializer().export(seq.document()).unwrap();
        let opened = JadeDocx::load(&file).unwrap();
        let loaded = DocxModel::from_word_document(opened.word()).unwrap();
        assert_eq!(
            loaded.paragraph_texts(),
            seq.document().paragraph_texts(),
            "{text:?}"
        );
    }
}

fn xlsx() -> Sequencer<jade_xlsx_collab::XlsxModel, jade_xlsx_collab::JadeXlsx> {
    use jade_xlsx_collab::{JadeXlsx, LoadOptions, XlsxModel};
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core/public/fixtures/grid-operations-workbook.xlsx"),
    )
    .unwrap();
    let jade = JadeXlsx::open(&bytes).unwrap();
    let model = XlsxModel::from_workbook(jade.workbook(), LoadOptions::default()).unwrap();
    Sequencer::with_materializer(model, jade)
}
#[test]
fn invalid_xlsx_formulas_and_nonfinite_numbers_are_refused_without_model_or_file_changes() {
    use jade_xlsx_collab::{CellInput, XlsxOp};
    for value in [
        CellInput::Formula("A1&\"\0\"".into()),
        CellInput::Formula("".into()),
        CellInput::Formula("=".into()),
        CellInput::Formula(" \t ".into()),
        CellInput::Number(f64::NAN),
        CellInput::Number(f64::INFINITY),
        CellInput::Number(f64::NEG_INFINITY),
    ] {
        let mut seq = xlsx();
        let welcome = seq.join();
        let before = seq.document().clone();
        let file = seq.materializer().export().unwrap();
        let (sheet, row, column) = before.locate("Operations", "E6").unwrap();
        let stamp = Id::new(before.max_lamport() + 1, welcome.replica);
        let result = seq.submit(
            welcome.replica,
            Submission {
                client_seq: 1,
                base_seq: 0,
                op: XlsxOp::SetCell {
                    sheet,
                    row,
                    column,
                    stamp,
                    value: value.clone(),
                },
            },
        );
        assert!(
            matches!(result, SubmitOutcome::Rejected(_)),
            "unwritable cell committed: {value:?}: {result:?}"
        );
        assert_eq!(seq.document(), &before);
        assert_eq!(seq.materializer().export().unwrap(), file);
        assert_eq!(seq.seq(), 0);
        assert!(seq.faults().is_empty());
    }
}
#[test]
fn xlsx_cell_text_and_notes_preserve_controls_literal_escapes_and_unicode_on_reopen() {
    use jade_xlsx_collab::{CellInput, JadeXlsx, LoadOptions, XlsxModel, XlsxOp};
    for text in [
        "before\0after",
        "a\r\nb",
        "literal_x0000_ and _x005F_",
        "\u{fffe}\u{ffff}日本🙂",
    ] {
        let mut seq = xlsx();
        let welcome = seq.join();
        let (sheet, row, column) = seq.document().locate("Operations", "E6").unwrap();
        for note in [false, true] {
            let stamp = Id::new(seq.document().max_lamport() + 1, welcome.replica);
            let op = if note {
                XlsxOp::SetNote {
                    sheet,
                    row,
                    column,
                    stamp,
                    note: Some(text.into()),
                }
            } else {
                XlsxOp::SetCell {
                    sheet,
                    row,
                    column,
                    stamp,
                    value: CellInput::Text(text.into()),
                }
            };
            let result = seq.submit(
                welcome.replica,
                Submission {
                    client_seq: if note { 2 } else { 1 },
                    base_seq: seq.seq(),
                    op,
                },
            );
            assert!(
                matches!(result, SubmitOutcome::Committed(_)),
                "losslessly encodable text refused: {text:?} note={note}: {result:?}"
            );
            assert!(seq.faults().is_empty(), "{text:?}: {:?}", seq.faults());
            let opened = JadeXlsx::open(&seq.materializer().export().unwrap()).unwrap();
            let loaded =
                XlsxModel::from_workbook(opened.workbook(), LoadOptions::default()).unwrap();
            let (s, r, c) = loaded.locate("Operations", "E6").unwrap();
            assert_eq!(loaded.value(s, r, c), Some(&CellInput::Text(text.into())));
            if note {
                assert_eq!(loaded.note(s, r, c), Some(text));
            }
        }
    }
}

use proptest::prelude::*;
const VALID_TEXT: &[&str] = &[
    "日本🙂e\u{301}",
    "a&<b>\"'",
    "\t\n\r",
    "\u{7f}\u{85}",
    "\u{fdd0}\u{1fffe}",
    "literal_x0000_",
];
const BAD_XML: &[&str] = &["a\0b", "\u{b}", "\u{fffe}", "\u{ffff}"];
fn docx_history(steps: &[(u8, usize)]) {
    let mut seq = docx();
    let welcome = seq.join();
    let replica = welcome.replica;
    let mut expected = seq.document().paragraph_texts();
    let mut ticket = 0;
    let mut commits = 0;
    for &(action, arg) in steps {
        if action == 3 {
            seq.compact_keeping(3);
            let checkpoint = seq.checkpoint();
            let tail = seq.log().to_vec();
            let file = seq.materializer().export_base().unwrap();
            let jade = JadeDocx::restore(&file, &checkpoint.document).unwrap();
            seq = Sequencer::restore_with_tail(checkpoint, tail, jade).unwrap();
        } else {
            let valid = VALID_TEXT[arg % VALID_TEXT.len()];
            let text = if action == 1 {
                BAD_XML[arg % BAD_XML.len()]
            } else {
                valid
            };
            let first = Id::new(seq.document().max_lamport() + 1, replica);
            let op = if action >= 4 {
                DocxOp::TrackInsertText {
                    story: StoryId::Body,
                    parent: None,
                    first,
                    text: text.into(),
                    date: (action == 5).then(|| "bad\0date".into()),
                }
            } else {
                DocxOp::InsertText {
                    story: StoryId::Body,
                    parent: None,
                    first,
                    text: text.into(),
                }
            };
            let before = seq.checkpoint();
            let file = seq.materializer().export(seq.document()).unwrap();
            if action != 2 || ticket == 0 {
                ticket += 1;
            }
            let duplicate = action == 2 && before.processed[&replica] == ticket;
            let result = seq.submit(
                replica,
                Submission {
                    client_seq: ticket,
                    base_seq: seq.seq(),
                    op,
                },
            );
            if duplicate {
                assert!(matches!(result, SubmitOutcome::Duplicate));
                assert_eq!(seq.checkpoint(), before);
            } else if action == 1 || action == 5 {
                assert!(matches!(result, SubmitOutcome::Rejected(_)));
                assert_eq!(seq.document(), &before.document);
                assert_eq!(seq.seq(), before.seq);
                assert_eq!(seq.materializer().export(seq.document()).unwrap(), file);
            } else {
                assert!(
                    matches!(result, SubmitOutcome::Committed(_)),
                    "{text:?}: {result:?}"
                );
                expected[0].insert_str(0, text);
                commits += 1;
            }
        }
        assert_eq!(seq.seq(), commits);
        assert_eq!(seq.processed_client_seq(replica), ticket);
        assert_eq!(seq.document().paragraph_texts(), expected);
        assert!(seq.faults().is_empty(), "{:?}", seq.faults());
        let file = seq.materializer().export(seq.document()).unwrap();
        let opened = JadeDocx::load(&file).unwrap();
        let loaded = DocxModel::from_word_document(opened.word()).unwrap();
        assert_eq!(loaded.paragraph_texts(), expected);
    }
}
fn xlsx_history(steps: &[(u8, usize)]) {
    use jade_xlsx_collab::{CellInput, JadeXlsx, LoadOptions, XlsxModel, XlsxOp};
    let mut seq = xlsx();
    let welcome = seq.join();
    let replica = welcome.replica;
    let (sheet, row, column) = seq.document().locate("Operations", "E6").unwrap();
    let mut value = seq.document().value(sheet, row, column).cloned();
    let mut note = seq.document().note(sheet, row, column).map(str::to_string);
    let mut ticket = 0;
    let mut commits = 0;
    for &(action, arg) in steps {
        if action == 3 {
            seq.compact_keeping(3);
            let checkpoint = seq.checkpoint();
            let tail = seq.log().to_vec();
            let jade = JadeXlsx::open(&seq.materializer().export().unwrap()).unwrap();
            seq = Sequencer::restore_with_tail(checkpoint, tail, jade).unwrap();
        } else {
            let valid = VALID_TEXT[arg % VALID_TEXT.len()];
            let text = if arg % 2 == 0 {
                BAD_XML[arg % BAD_XML.len()]
            } else {
                valid
            };
            let stamp = Id::new(seq.document().max_lamport() + 1, replica);
            let op = match action {
                1 => XlsxOp::SetCell {
                    sheet,
                    row,
                    column,
                    stamp,
                    value: CellInput::Formula(format!("A1&\"{}\"", BAD_XML[arg % BAD_XML.len()])),
                },
                4 => XlsxOp::SetNote {
                    sheet,
                    row,
                    column,
                    stamp,
                    note: Some(text.into()),
                },
                5 => XlsxOp::SetCell {
                    sheet,
                    row,
                    column,
                    stamp,
                    value: CellInput::Formula("1+1".into()),
                },
                _ => XlsxOp::SetCell {
                    sheet,
                    row,
                    column,
                    stamp,
                    value: CellInput::Text(text.into()),
                },
            };
            let before = seq.checkpoint();
            let file = seq.materializer().export().unwrap();
            if action != 2 || ticket == 0 {
                ticket += 1;
            }
            let duplicate = action == 2 && before.processed[&replica] == ticket;
            let result = seq.submit(
                replica,
                Submission {
                    client_seq: ticket,
                    base_seq: seq.seq(),
                    op,
                },
            );
            if duplicate {
                assert!(matches!(result, SubmitOutcome::Duplicate));
                assert_eq!(seq.checkpoint(), before);
            } else if action == 1 {
                assert!(matches!(result, SubmitOutcome::Rejected(_)));
                assert_eq!(seq.document(), &before.document);
                assert_eq!(seq.seq(), before.seq);
                assert_eq!(seq.materializer().export().unwrap(), file);
            } else {
                assert!(
                    matches!(result, SubmitOutcome::Committed(_)),
                    "{text:?}: {result:?}"
                );
                if action == 4 {
                    note = Some(text.into());
                } else {
                    value = Some(if action == 5 {
                        CellInput::Formula("1+1".into())
                    } else {
                        CellInput::Text(text.into())
                    });
                }
                commits += 1;
            }
        }
        assert_eq!(seq.seq(), commits);
        assert_eq!(seq.processed_client_seq(replica), ticket);
        assert_eq!(seq.document().value(sheet, row, column), value.as_ref());
        assert_eq!(seq.document().note(sheet, row, column), note.as_deref());
        assert!(seq.faults().is_empty(), "{:?}", seq.faults());
        let opened = JadeXlsx::open(&seq.materializer().export().unwrap()).unwrap();
        let loaded = XlsxModel::from_workbook(opened.workbook(), LoadOptions::default()).unwrap();
        let (s, r, c) = loaded.locate("Operations", "E6").unwrap();
        assert_eq!(loaded.value(s, r, c), value.as_ref());
        assert_eq!(loaded.note(s, r, c), note.as_deref());
    }
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn stateful_text_admission_retries_and_checkpoint_restore_preserve_native_file_content(steps in prop::collection::vec((0u8..6,0usize..12),1..24)) {
        docx_history(&steps);xlsx_history(&steps);
    }
}

fn bad_fields<D>(model: &D, op: D::Op, fields: &[&str])
where
    D: Document + std::fmt::Debug + PartialEq,
    D::Op: serde::Serialize + serde::de::DeserializeOwned,
{
    use jade_collab_core::id::ReplicaId;
    use jade_collab_sync::ApplyContext;
    model.validate_submission(&op, ReplicaId(1)).unwrap();
    let wire = serde_json::to_value(op).unwrap();
    for field in fields {
        for text in BAD_XML {
            let mut forged = wire.clone();
            *forged.pointer_mut(field).unwrap() = serde_json::Value::from(*text);
            let op: D::Op = serde_json::from_value(forged).unwrap();
            assert!(
                model.validate_submission(&op, ReplicaId(1)).is_err(),
                "unvalidated field {field}: {op:?}"
            );
            let mut changed = model.clone();
            assert!(changed
                .apply(
                    &op,
                    &ApplyContext {
                        origin: ReplicaId(1),
                        seq: None,
                        base_seq: 0
                    }
                )
                .is_err());
            assert_eq!(&changed, model, "refusal changed model/clock for {field}");
        }
    }
}
#[test]
fn docx_metadata_comments_stories_and_format_strings_reject_xml_forbidden_scalars_atomically() {
    use jade_docx_collab::{CommentContent, FormatChange, NoteKind};
    let seq = docx();
    let model = seq.document();
    let story = StoryId::Body;
    let first = Id::new(model.max_lamport() + 1, jade_collab_core::id::ReplicaId(1));
    let p = &model.paragraphs()[0];
    let start = p.chars[0];
    let end = p.chars[1];
    let content = CommentContent {
        text: "comment".into(),
        author: Some("Ada".into()),
        initials: Some("A".into()),
        date: Some("2026-10-22T00:00:00Z".into()),
    };
    let cases = vec![
        (
            DocxOp::TrackInsertText {
                story,
                parent: None,
                first,
                text: "text".into(),
                date: Some("2026-10-22T00:00:00Z".into()),
            },
            vec!["/text", "/date"],
        ),
        (
            DocxOp::TrackDeleteText {
                story,
                ids: vec![start],
                revision: first,
                date: Some("2026-10-22T00:00:00Z".into()),
            },
            vec!["/date"],
        ),
        (
            DocxOp::AddComment {
                story,
                start,
                end,
                id: first,
                content: content.clone(),
            },
            vec![
                "/content/text",
                "/content/author",
                "/content/initials",
                "/content/date",
            ],
        ),
        (
            DocxOp::ReplyToComment {
                comment: start,
                id: first,
                content,
            },
            vec![
                "/content/text",
                "/content/author",
                "/content/initials",
                "/content/date",
            ],
        ),
        (
            DocxOp::UpdateComment {
                comment: start,
                text: "updated".into(),
            },
            vec!["/text"],
        ),
        (
            DocxOp::InsertNote {
                kind: NoteKind::Footnote,
                story,
                parent: None,
                id: first,
                text: "note".into(),
            },
            vec!["/text"],
        ),
        (
            DocxOp::InsertTextBox {
                story,
                parent: None,
                id: first,
                text: "box".into(),
                width_px: 100,
                height_px: 100,
            },
            vec!["/text"],
        ),
        (
            DocxOp::Format {
                story,
                start,
                end,
                change: FormatChange::FontFamily {
                    family: "Arial".into(),
                },
            },
            vec!["/change/family"],
        ),
        (
            DocxOp::Format {
                story,
                start,
                end,
                change: FormatChange::ParagraphStyle {
                    style_id: Some("BodyText".into()),
                },
            },
            vec!["/change/style_id"],
        ),
        (
            DocxOp::Mark {
                story,
                id: first,
                start: jade_docx_collab::Boundary::Before(start),
                end: jade_docx_collab::Boundary::After(end),
                change: FormatChange::FontFamily {
                    family: "Arial".into(),
                },
            },
            vec!["/change/family"],
        ),
        (
            DocxOp::SetAuthor {
                replica: first.replica,
                name: "Ada".into(),
            },
            vec!["/name"],
        ),
    ];
    for (op, fields) in cases {
        bad_fields(model, op, &fields);
    }
}
#[test]
fn xlsx_unescaped_names_formats_and_header_strings_reject_xml_forbidden_scalars_atomically() {
    use jade_xlsx_collab::{FormatChange, LineAxis, XlsxOp};
    let seq = xlsx();
    let model = seq.document();
    let (sheet, row, column) = model.locate("Operations", "E6").unwrap();
    let stamp = Id::new(model.max_lamport() + 1, jade_collab_core::id::ReplicaId(1));
    let change = FormatChange {
        font_name: Some("Arial".into()),
        number_format: Some("0.00".into()),
        ..Default::default()
    };
    let cases = vec![
        (
            XlsxOp::SetHeader {
                sheet,
                row,
                column,
                stamp,
                text: "header".into(),
            },
            vec!["/text"],
        ),
        (
            XlsxOp::AddSheet {
                id: stamp,
                name: "New".into(),
                after: None,
                rows: 2,
                columns: 2,
            },
            vec!["/name"],
        ),
        (
            XlsxOp::RenameSheet {
                sheet,
                name: "New".into(),
            },
            vec!["/name"],
        ),
        (
            XlsxOp::SetFormat {
                sheet,
                rows: vec![row],
                columns: vec![column],
                stamp,
                change: change.clone(),
            },
            vec!["/change/font_name", "/change/number_format"],
        ),
        (
            XlsxOp::SetLineFormat {
                sheet,
                axis: LineAxis::Rows,
                lines: vec![row],
                stamp,
                change,
            },
            vec!["/change/font_name", "/change/number_format"],
        ),
    ];
    for (op, fields) in cases {
        bad_fields(model, op, &fields);
    }
}
#[test]
fn pptx_keeps_its_native_character_policy_and_multiline_intents_round_trip() {
    use jade_collab_core::id::Clock;
    use jade_pptx_collab::{
        intent::{try_resolve, PptxIntent, Target},
        JadePptx, PptxModel, PptxOp, StoryId,
    };
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core/public/fixtures/sample-deck.pptx"),
    )
    .unwrap();
    let jade = JadePptx::open(&bytes).unwrap();
    let model = PptxModel::from_pptx(jade.bytes()).unwrap();
    let mut seq = Sequencer::with_materializer(model, jade);
    let welcome = seq.join();
    let slide = seq.document().slides()[0].id;
    let story = StoryId::Notes { slide };
    let before = seq.document().clone();
    let file = seq.materializer().export(seq.document()).unwrap();
    let mut ticket = 0;
    for text in BAD_XML.iter().copied().chain(["a\rb", "a\nb"]) {
        ticket += 1;
        let first = Id::new(seq.document().max_lamport() + 1, welcome.replica);
        let result = seq.submit(
            welcome.replica,
            Submission {
                client_seq: ticket,
                base_seq: 0,
                op: PptxOp::InsertText {
                    story,
                    parent: None,
                    first,
                    text: text.into(),
                },
            },
        );
        assert!(
            matches!(result, SubmitOutcome::Rejected(_)),
            "native-invalid PPTX text committed: {text:?}"
        );
        assert_eq!(seq.document(), &before);
        assert_eq!(seq.materializer().export(seq.document()).unwrap(), file);
    }
    let mut clock = Clock::new(welcome.replica);
    clock.observe_lamport(seq.document().max_lamport());
    let ops = try_resolve(
        seq.document(),
        &PptxIntent::InsertText {
            slide: 1,
            target: Target::Notes,
            paragraph: 0,
            offset: 0,
            text: "日本🙂e\u{301}\t<&>\nsecond".into(),
        },
        &mut |n| clock.try_reserve(n),
    )
    .unwrap();
    for op in ops {
        ticket += 1;
        let result = seq.submit(
            welcome.replica,
            Submission {
                client_seq: ticket,
                base_seq: seq.seq(),
                op,
            },
        );
        assert!(matches!(result, SubmitOutcome::Committed(_)));
    }
    assert!(seq.faults().is_empty());
    let loaded = PptxModel::from_pptx(&seq.materializer().export(seq.document()).unwrap()).unwrap();
    assert_eq!(loaded.slides()[0].notes, seq.document().slides()[0].notes);
}
