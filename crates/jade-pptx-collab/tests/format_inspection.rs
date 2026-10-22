mod common;

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Materializer};
use jade_pptx_collab::intent::{resolve, PptxIntent};
use jade_pptx_collab::{CharFormat, JadePptx, PptxModel};

#[test]
fn authoring_styles_resolve_inheritance_before_autofit_and_explicit_marks_override_it() {
    let mut package = jade_opc::OpcPackage::open(&common::fixture("inherited-deck.pptx")).unwrap();
    let master = package
        .required_part_str("ppt/slideMasters/slideMaster1.xml")
        .unwrap()
        .replace("<a:defRPr sz=\"3000\">", "<a:defRPr sz=\"3000\" b=\"1\">");
    package.replace("ppt/slideMasters/slideMaster1.xml", master.into_bytes());
    let bytes = package.write().unwrap();
    let (mut model, mut jade) = (
        PptxModel::from_pptx(&bytes).unwrap(),
        JadePptx::open(&bytes).unwrap(),
    );
    let base = jade.export_base();
    let initial = jade.inspect_formatting(&model).unwrap();
    initial.validate(&model).unwrap();
    let p = &initial.slides[0].shapes[0].paragraphs.as_ref().unwrap()[0];
    assert_eq!(
        p.runs[0].style.size_pt, 30.0,
        "authoring size before display autofit"
    );
    assert_eq!(p.runs[0].style.color, "#A020F0");
    assert_eq!(p.runs[0].style.font.as_deref(), Some("serif"));
    assert!(
        p.runs[0].style.bold,
        "master placeholder supplies true before the explicit false mark"
    );
    let shape = model.slides()[0].shapes[0].number;
    let mut next = model.max_lamport() + 1;
    for (seq, format) in [
        CharFormat::Bold { value: false },
        CharFormat::Size { hundredths: 1600 },
    ]
    .into_iter()
    .enumerate()
    {
        let ops = resolve(
            &model,
            &PptxIntent::FormatText {
                slide: 1,
                shape,
                start_paragraph: 0,
                start_offset: 0,
                end_paragraph: 0,
                end_offset: 1,
                format,
            },
            &mut |n| {
                let id = Id::new(next, ReplicaId(1));
                next += n;
                id
            },
        )
        .unwrap();
        for op in ops {
            let ctx = ApplyContext {
                origin: ReplicaId(1),
                base_seq: seq as u64,
                seq: Some(seq as u64 + 1),
            };
            let effects = model.apply(&op, &ctx).unwrap();
            jade.materialize(&model, effects, &ctx).unwrap();
        }
    }
    let captured = jade.inspect_formatting(&model).unwrap();
    let p = &captured.slides[0].shapes[0].paragraphs.as_ref().unwrap()[0];
    assert_eq!((p.runs[0].start, p.runs[0].end), (0, 1));
    assert!(!p.runs[0].style.bold);
    assert_eq!(p.runs[0].style.size_pt, 16.0);
    assert_eq!(p.runs[1].style.size_pt, 30.0);
    assert_eq!(
        jade.export_base(),
        base,
        "inspection and marks leave the native base unchanged"
    );
    let bytes = jade.export(&model).unwrap();
    let imported = PptxModel::from_pptx(&bytes).unwrap();
    assert_eq!(
        JadePptx::open(&bytes)
            .unwrap()
            .inspect_formatting(&imported)
            .unwrap(),
        captured
    );
}

#[test]
fn unicode_soft_breaks_empty_paragraphs_and_cached_fields_keep_authoring_offsets() {
    let mut package = jade_opc::OpcPackage::open(&common::fixture("inherited-deck.pptx")).unwrap();
    let part = "ppt/slides/slide1.xml";
    let mut slide = package.required_part_str(part).unwrap();
    let start = slide.find("<p:txBody>").unwrap();
    let end = slide[start..].find("</p:txBody>").unwrap() + start + "</p:txBody>".len();
    slide.replace_range(start..end, r#"<p:txBody><a:bodyPr/><a:lstStyle/>
      <a:p><a:pPr algn="r" marL="25400" indent="-12700"><a:buChar char="•"/><a:spcBef><a:spcPts val="600"/></a:spcBef></a:pPr>
        <a:r><a:rPr b="0"/><a:t>日🙂</a:t></a:r><a:br/><a:r><a:rPr i="1"/><a:t>é</a:t></a:r>
      </a:p><a:p><a:pPr algn="ctr"/></a:p>
      <a:p><a:fld id="{00000000-0000-0000-0000-000000000001}" type="slidenum"><a:rPr u="sng"/><a:t>999</a:t></a:fld></a:p>
    </p:txBody>"#);
    package.replace(part, slide.into_bytes());
    let bytes = package.write().unwrap();
    let (model, jade) = (
        PptxModel::from_pptx(&bytes).unwrap(),
        JadePptx::open(&bytes).unwrap(),
    );
    let format = jade.inspect_formatting(&model).unwrap();
    format.validate(&model).unwrap();
    assert_eq!(
        model.slides()[0].shapes[0].text.as_deref(),
        Some("日🙂\né\n\n999")
    );
    let paragraphs = format.slides[0].shapes[0].paragraphs.as_ref().unwrap();
    assert_eq!(paragraphs.len(), 4);
    assert_eq!(
        (paragraphs[0].runs[0].start, paragraphs[0].runs[0].end),
        (0, 7)
    );
    assert!(!paragraphs[0].runs[0].style.bold);
    assert_eq!(paragraphs[0].style.alignment.as_deref(), Some("right"));
    assert_eq!(paragraphs[0].style.bullet.as_deref(), Some("character"));
    assert_eq!(paragraphs[0].style.character.as_deref(), Some("•"));
    assert_eq!(paragraphs[0].style.margin_left_pt, Some(2.0));
    assert_eq!(paragraphs[0].style.indent_pt, Some(-1.0));
    assert_eq!(
        paragraphs[0].style.space_before,
        Some(jade_pptx_collab::Spacing::Points(6.0))
    );
    assert_eq!(
        paragraphs[1].style, paragraphs[0].style,
        "soft break stays in the same native paragraph"
    );
    assert_eq!(
        (paragraphs[1].runs[0].start, paragraphs[1].runs[0].end),
        (0, 2)
    );
    assert!(paragraphs[1].runs[0].style.italic);
    assert!(paragraphs[2].runs.is_empty());
    assert_eq!(paragraphs[2].style.alignment.as_deref(), Some("center"));
    assert_eq!(
        paragraphs[3].runs[0].end, 3,
        "cached field text is not replaced by the slide's rendered number"
    );
    assert!(paragraphs[3].runs[0].style.underline);
    let mut bad = format.clone();
    bad.slides[0].shapes[0].paragraphs.as_mut().unwrap()[0].runs[0].end = 1;
    assert!(
        bad.validate(&model).is_err(),
        "mid-codepoint byte boundary is refused"
    );
}

fn check_overrides(model: &PptxModel, captured: &jade_pptx_collab::PptxFormatting) {
    for run in model.formatting() {
        let shape = captured.slides[run.slide]
            .shapes
            .iter()
            .find(|s| s.shape == run.shape.number)
            .unwrap();
        for native in &shape.paragraphs.as_ref().unwrap()[run.paragraph].runs {
            if native.start >= run.end || run.start >= native.end {
                continue;
            }
            let style = &native.style;
            match &run.format {
                CharFormat::Bold { value } => assert_eq!(style.bold, *value),
                CharFormat::Italic { value } => assert_eq!(style.italic, *value),
                CharFormat::Underline { value } => assert_eq!(style.underline, *value),
                CharFormat::Size { hundredths } => {
                    assert_eq!(style.size_pt, *hundredths as f32 / 100.0)
                }
                CharFormat::Font { typeface } => {
                    assert_eq!(style.font.as_deref(), Some(typeface.as_str()))
                }
                CharFormat::EastAsianFont { typeface } => {
                    assert_eq!(style.east_asian_font.as_deref(), Some(typeface.as_str()))
                }
                CharFormat::Color { color } => assert!(style.color.eq_ignore_ascii_case(color)),
            }
        }
    }
    for p in captured
        .slides
        .iter()
        .flat_map(|s| &s.shapes)
        .filter_map(|s| s.paragraphs.as_ref())
        .flatten()
    {
        for pair in p.runs.windows(2) {
            assert_ne!(
                pair[0].style, pair[1].style,
                "equal adjacent styles must be coalesced"
            );
        }
    }
}

#[test]
fn every_native_fixture_can_be_inspected_and_invalid_pairings_are_refused() {
    let mut checked = 0;
    for path in std::fs::read_dir(common::fixtures())
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pptx"))
    {
        let bytes = std::fs::read(&path).unwrap();
        let Ok(jade) = JadePptx::open(&bytes) else {
            continue;
        };
        let model = PptxModel::from_pptx(&bytes).unwrap();
        let format = jade
            .inspect_formatting(&model)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        format.validate(&model).unwrap();
        let mut missing = format.clone();
        missing.slides.pop();
        assert!(missing.validate(&model).is_err());
        checked += 1;
    }
    assert!(checked >= 4);
    let (model, jade) = common::open("inherited-deck.pptx");
    let format = jade.inspect_formatting(&model).unwrap();
    let mut bad = format.clone();
    bad.slides[0].shapes[0].shape += 100;
    assert!(bad.validate(&model).is_err());
    let mut bad = format.clone();
    bad.slides[0].shapes[0].paragraphs.as_mut().unwrap()[0].runs[0].end = 0;
    assert!(bad.validate(&model).is_err());
    let mut bad = format.clone();
    bad.slides[0].shapes[0].paragraphs = None;
    assert!(bad.validate(&model).is_err());
    let mut bad = format;
    bad.slides[0].shapes[0].paragraphs.as_mut().unwrap()[0].runs[0]
        .style
        .size_pt = f32::NAN;
    assert!(bad.validate(&model).is_err());
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    #[test]
    fn inspection_survives_edit_schedules_duplicates_and_checkpoint_restores(
        steps in proptest::collection::vec((0usize..3, 0u8..8, proptest::num::u64::ANY), 1..24),
        grouped in proptest::bool::ANY,
    ) {
        use jade_collab_sync::sim::Simulation;
        let (model,jade) = common::open(if grouped {"grouped-deck.pptx"} else {"inherited-deck.pptx"});
        let mut sim = Simulation::with_materializer(model, jade, 3);
        for (client,kind,bits) in steps {
            match kind {
                0..=3 => {
                    let view = sim.client(client).view().clone();
                    if let Some(op) = common::generate(&view, bits, &mut |n| sim.client_mut(client).reserve(n)) {let _ = sim.local(client,op);}
                }
                4 => {sim.deliver_up(client);}
                5 => {sim.deliver_down(client).unwrap();}
                6 => {sim.reconnect().unwrap();}
                _ => {
                    let (checkpoint, tail) = sim.checkpoint_keeping(bits as usize % 3);
                    let checkpoint: jade_collab_sync::Checkpoint<PptxModel> = serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
                    let jade = JadePptx::restore(&sim.server().materializer().export_base(),&checkpoint.document).unwrap();
                    sim.restart(jade_collab_sync::Sequencer::restore_with_tail(checkpoint,tail,jade).unwrap()).unwrap();
                }
            }
            let authority = sim.server();
            let model = authority.document();
            let jade = authority.materializer();
            let base = jade.export_base();
            let captured = jade.inspect_formatting(model).unwrap();
            captured.validate(model).unwrap();
            check_overrides(model,&captured);
            let captured: jade_pptx_collab::PptxFormatting = serde_json::from_value(serde_json::to_value(captured).unwrap()).unwrap();
            let export = jade.export(model).unwrap();
            let reimported = PptxModel::from_pptx(&export).unwrap();
            let native = JadePptx::open(&export).unwrap().inspect_formatting(&reimported).unwrap();
            proptest::prop_assert_eq!(native, captured);
            proptest::prop_assert_eq!(jade.export_base(), base);
            proptest::prop_assert!(authority.faults().is_empty());
        }
        sim.quiesce().unwrap();
        for client in 0..3 {proptest::prop_assert_eq!(sim.client(client).view(),sim.server().document());}
    }
}
