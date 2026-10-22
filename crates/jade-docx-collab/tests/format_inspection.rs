mod common;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Materializer};
use jade_docx_collab::{DocxModel, DocxOp, Expand, FormatChange, JadeDocx};

fn styled() -> (DocxModel, JadeDocx) {
    let mut package = jade_opc::OpcPackage::open(&common::fixture("jade-core/hello.docx")).unwrap();
    package.replace("word/styles.xml", br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
      <w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri"/><w:sz w:val="22"/><w:color w:val="112233"/></w:rPr></w:rPrDefault></w:docDefaults>
      <w:style w:type="paragraph" w:styleId="Parent"><w:rPr><w:b/><w:i/></w:rPr></w:style>
      <w:style w:type="paragraph" w:styleId="BodyX"><w:basedOn w:val="Parent"/><w:rPr><w:i w:val="0"/><w:sz w:val="28"/><w:rFonts w:ascii="Georgia"/></w:rPr></w:style>
      <w:style w:type="character" w:styleId="Em"><w:rPr><w:i/><w:u w:val="double"/><w:color w:val="AABBCC"/></w:rPr></w:style>
    </w:styles>"#.to_vec());
    package.replace("word/document.xml", r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
      <w:p><w:pPr><w:pStyle w:val="BodyX"/><w:jc w:val="right"/></w:pPr><w:r><w:rPr><w:rStyle w:val="Em"/><w:b w:val="0"/><w:highlight w:val="yellow"/></w:rPr><w:t>日🙂</w:t></w:r><w:r><w:t>tail</w:t></w:r></w:p>
      <w:p><w:pPr><w:pStyle w:val="BodyX"/></w:pPr></w:p>
    </w:body></w:document>"#.as_bytes().to_vec());
    let jade = JadeDocx::load(&package.write().unwrap()).unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    (model, jade)
}

#[test]
fn native_inspection_resolves_defaults_paragraph_character_and_direct_styles() {
    let (model, jade) = styled();
    let formats = jade.inspect_formatting(&model).unwrap();
    assert_eq!(formats.paragraphs.len(), 2);
    let p = &formats.paragraphs[0];
    assert_eq!(p.style_id.as_deref(), Some("BodyX"));
    assert_eq!(p.alignment.as_deref(), Some("right"));
    assert_eq!(p.runs.len(), 2);
    let first = &p.runs[0];
    assert_eq!((first.start, first.end), (0, 7));
    assert!(!first.style.bold);
    assert!(first.style.italic);
    assert!(first.style.underline);
    assert_eq!(first.style.underline_style.as_deref(), Some("double"));
    assert_eq!(first.style.font_size, Some(14.0));
    assert_eq!(first.style.font_family.as_deref(), Some("Georgia"));
    assert_eq!(first.style.font_color.as_deref(), Some("#AABBCC"));
    assert_eq!(first.style.highlight.as_deref(), Some("yellow"));
    let tail = &p.runs[1];
    assert_eq!((tail.start, tail.end), (7, 11));
    assert!(tail.style.bold);
    assert!(!tail.style.italic);
    assert_eq!(tail.style.font_color.as_deref(), Some("#112233"));
    assert!(formats.paragraphs[1].runs.is_empty());
}

#[test]
fn inspection_includes_marks_without_mutating_the_native_base_and_matches_export() {
    let (mut model, mut jade) = styled();
    let before = jade.export_base().unwrap();
    let p = &model.paragraphs()[0];
    let (start, end) = model
        .mark_anchors(p.story, p.chars[0], p.chars[1], Expand::End)
        .unwrap();
    let op = DocxOp::Mark {
        story: p.story,
        id: Id::new(model.max_lamport() + 1, ReplicaId(1)),
        start,
        end,
        change: FormatChange::Bold { value: true },
    };
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    };
    let effects = model.apply(&op, &ctx).unwrap();
    jade.materialize(&model, effects, &ctx).unwrap();
    let inspected = jade.inspect_formatting(&model).unwrap();
    assert!(inspected.paragraphs[0].runs[0].style.bold);
    assert_eq!(jade.export_base().unwrap(), before);
    let reopened = JadeDocx::load(&jade.export(&model).unwrap()).unwrap();
    let imported = DocxModel::from_word_document(reopened.word()).unwrap();
    assert_eq!(reopened.inspect_formatting(&imported).unwrap(), inspected);
}

#[test]
fn inspected_run_boundaries_are_validated_before_pairing_them_with_model_text() {
    let (model, jade) = styled();
    let inspected = jade.inspect_formatting(&model).unwrap();
    inspected.validate(&model).unwrap();
    let mut missing = inspected.clone();
    missing.paragraphs.pop();
    assert!(missing.validate(&model).is_err());
    let mut bad_utf8 = inspected.clone();
    bad_utf8.paragraphs[0].runs[0].end = 1;
    bad_utf8.paragraphs[0].runs[1].start = 1;
    assert!(bad_utf8.validate(&model).is_err());
    let mut uncovered = inspected.clone();
    uncovered.paragraphs[0].runs.pop();
    assert!(uncovered.validate(&model).is_err());
    let mut overlap = inspected;
    overlap.paragraphs[0].runs[1].start = 0;
    assert!(overlap.validate(&model).is_err());
}

#[test]
fn native_inspection_follows_model_order_for_cells_notes_and_text_boxes() {
    let (mut model, mut jade) = common::open("footnote-table-textbox-order.docx");
    for (index, p) in model.paragraphs().into_iter().enumerate() {
        let Some(first) = p.chars.first().copied() else {
            continue;
        };
        let (start, end) = model
            .mark_anchors(p.story, first, first, Expand::End)
            .unwrap();
        let op = DocxOp::Mark {
            story: p.story,
            id: Id::new(model.max_lamport() + 1, ReplicaId(1)),
            start,
            end,
            change: FormatChange::Bold {
                value: index % 2 == 0,
            },
        };
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: 0,
            seq: Some(index as u64 + 1),
        };
        let effects = model.apply(&op, &ctx).unwrap();
        jade.materialize(&model, effects, &ctx).unwrap();
    }
    let inspected = jade.inspect_formatting(&model).unwrap();
    inspected.validate(&model).unwrap();
    for (index, (p, format)) in model
        .paragraphs()
        .iter()
        .zip(&inspected.paragraphs)
        .enumerate()
    {
        if !p.text.is_empty() {
            assert_eq!(
                format.runs[0].style.bold,
                index % 2 == 0,
                "paragraph {index}"
            );
        }
    }
    let reopened = JadeDocx::load(&jade.export(&model).unwrap()).unwrap();
    let reimported = DocxModel::from_word_document(reopened.word()).unwrap();
    assert_eq!(reopened.inspect_formatting(&reimported).unwrap(), inspected);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn inspected_formats_survive_network_schedules_and_native_export_reimports(
        steps in proptest::collection::vec((0usize..3, 0u8..7, proptest::num::u64::ANY), 1..30),
    ) {
        use jade_collab_sync::sim::Simulation;
        use jade_docx_collab::intent::{resolve, IntentOp};
        let (model, jade) = styled();
        let mut sim = Simulation::with_materializer(model, jade, 3);
        for (client, kind, bits) in steps {
            match kind {
                0..=2 => {
                    let view = sim.client(client).view().clone();
                    let paragraphs = view.paragraphs();
                    let p = bits as usize % paragraphs.len();
                    let len = paragraphs[p].text.len();
                    let intent = match kind {
                        0 => IntentOp::InsertText { paragraph: p, offset: len, text: ["日", "🙂", "x"][bits as usize % 3].into() },
                        1 if len > 0 => {
                            let changes = common::character_changes();
                            IntentOp::Format { start_paragraph: p, start_offset: 0, end_paragraph: p, end_offset: len, change: changes[bits as usize % changes.len()].clone() }
                        }
                        2 => IntentOp::FormatParagraphs { start_paragraph: p, end_paragraph: p,
                            change: FormatChange::Alignment { alignment: [jade_docx_collab::ParagraphAlignment::Left, jade_docx_collab::ParagraphAlignment::Center, jade_docx_collab::ParagraphAlignment::Right][bits as usize % 3] } },
                        _ => continue,
                    };
                    if let Ok(ops) = resolve(&view, &intent, &mut |n| sim.client_mut(client).reserve(n)) {
                        for op in ops { let _ = sim.local(client, op); }
                    }
                }
                3 => { sim.deliver_up(client); }
                4 => { sim.deliver_down(client).unwrap(); }
                5 => { sim.reconnect().unwrap(); }
                _ => {
                    let (checkpoint, tail) = sim.checkpoint_keeping(bits as usize % 3);
                    let checkpoint: jade_collab_sync::Checkpoint<DocxModel> = serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
                    let base = sim.server().materializer().export_base().unwrap();
                    let mat = JadeDocx::restore(&base, &checkpoint.document).unwrap();
                    let restored = jade_collab_sync::Sequencer::restore_with_tail(checkpoint, tail, mat).unwrap();
                    sim.restart(restored).unwrap();
                }
            }
            let server = sim.server();
            let inspected = server.materializer().inspect_formatting(server.document()).unwrap();
            inspected.validate(server.document()).unwrap();
            let reopened = JadeDocx::load(&server.materializer().export(server.document()).unwrap()).unwrap();
            let reimported = DocxModel::from_word_document(reopened.word()).unwrap();
            proptest::prop_assert_eq!(reopened.inspect_formatting(&reimported).unwrap(), inspected);
            proptest::prop_assert!(server.faults().is_empty());
        }
        sim.quiesce().unwrap();
        for client in 0..3 { proptest::prop_assert_eq!(sim.client(client).view(), sim.server().document()); }
    }
}
