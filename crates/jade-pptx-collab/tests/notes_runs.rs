//! Notes typing must not leave an unbounded chain of redundant native runs.
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Materializer};
use jade_opc::{parse_relationships, rels_part_name, resolve_target, OpcPackage};
use jade_pptx_collab::{
    intent::{PptxIntent, Target},
    JadePptx, PptxModel,
};
use proptest::prelude::*;
use quick_xml::{events::Event, name::ResolveResult, NsReader};

fn fixture() -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core/public/fixtures/sample-deck.pptx"),
    )
    .unwrap()
}
fn notes_xml(jade: &JadePptx) -> String {
    let deck = jade_pptx_reader::read_deck(jade.bytes()).unwrap();
    let slide = &deck.slides[0].part_name;
    let package = OpcPackage::open(jade.bytes()).unwrap();
    let rels =
        parse_relationships(&package.required_part_str(&rels_part_name(slide)).unwrap()).unwrap();
    let note = rels.by_type_suffix("/notesSlide");
    package
        .required_part_str(&resolve_target(slide, &note[0].target))
        .unwrap()
}
fn runs(xml: &str) -> usize {
    let mut reader = NsReader::from_str(xml);
    let mut count = 0;
    loop {
        let (ns, event) = reader.read_resolved_event().unwrap();
        if let Event::Start(e) | Event::Empty(e) = &event {
            if matches!(ns,ResolveResult::Bound(ns) if ns.as_ref()==b"http://schemas.openxmlformats.org/drawingml/2006/main")
                && e.local_name().as_ref() == b"r"
            {
                count += 1;
            }
        }
        if matches!(event, Event::Eof) {
            break;
        }
    }
    count
}

#[test]
fn ordinary_notes_typing_keeps_native_plain_runs_bounded_across_restore() {
    let bytes = fixture();
    let mut model = PptxModel::from_pptx(&bytes).unwrap();
    let mut jade = JadePptx::open(&bytes).unwrap();
    let mut expected = model.slides()[0].notes.clone();
    let mut next = model.max_lamport() + 100;
    let mut seq = 0;
    for i in 0..128 {
        let text = format!("[{i}]字🙂");
        let intent = PptxIntent::InsertText {
            slide: 1,
            target: Target::Notes,
            paragraph: 0,
            offset: 0,
            text: text.clone(),
        };
        let ops = jade_pptx_collab::intent::resolve(&model, &intent, &mut |n| {
            let id = Id::new(next, ReplicaId(1));
            next += n;
            id
        })
        .unwrap();
        for op in ops {
            let ctx = ApplyContext {
                origin: ReplicaId(1),
                base_seq: seq,
                seq: Some(seq + 1),
            };
            let effect = model.apply(&op, &ctx).unwrap();
            jade.materialize(&model, effect, &ctx).unwrap();
            seq += 1;
        }
        expected.insert_str(0, &text);
        assert_eq!(
            jade_pptx_writer::notes_text(jade.bytes(), 0).unwrap().text,
            expected
        );
        if i == 63 {
            let bytes = jade.export(&model).unwrap();
            jade = JadePptx::restore(&bytes, &model).unwrap();
        }
    }
    let count = runs(&notes_xml(&jade));
    assert!(
        count <= 17,
        "plain native runs grew to {count} after 128 prepends"
    );
    let reopened = PptxModel::from_pptx(&jade.export(&model).unwrap()).unwrap();
    assert_eq!(reopened.slides()[0].notes, expected);
}

#[test]
fn checkpoint_replay_has_the_exact_same_native_bytes_as_uninterrupted_edits() {
    let mut live = Session::new(&fixture());
    for _ in 0..9 {
        live.prepend(1, "prefix");
    }
    let checkpoint = live.jade.export(&live.model).unwrap();
    let mut restored = Session {
        model: live.model.clone(),
        jade: JadePptx::restore(&checkpoint, &live.model).unwrap(),
        seq: live.seq,
        next: live.next,
    };
    assert_eq!(live.jade.bytes(), restored.jade.bytes());
    for _ in 0..16 {
        live.prepend(1, "suffix");
        restored.prepend(1, "suffix");
        assert!(
            live.jade.bytes() == restored.jade.bytes(),
            "checkpoint replay changed the native archive despite identical operations"
        );
    }
}

struct Session {
    model: PptxModel,
    jade: JadePptx,
    seq: u64,
    next: u64,
}
impl Session {
    fn new(bytes: &[u8]) -> Self {
        let model = PptxModel::from_pptx(bytes).unwrap();
        let next = model.max_lamport() + 100;
        Self {
            model,
            jade: JadePptx::open(bytes).unwrap(),
            seq: 0,
            next,
        }
    }
    fn intent(&mut self, intent: PptxIntent) {
        let ops = jade_pptx_collab::intent::resolve(&self.model, &intent, &mut |n| {
            let id = Id::new(self.next, ReplicaId(1));
            self.next += n;
            id
        })
        .unwrap();
        for op in ops {
            if PptxModel::lane(&op) == jade_collab_sync::Lane::Sequenced {
                self.jade.admit(&self.model, &op).unwrap();
            }
            let ctx = ApplyContext {
                origin: ReplicaId(1),
                base_seq: self.seq,
                seq: Some(self.seq + 1),
            };
            let effect = self.model.apply(&op, &ctx).unwrap();
            self.jade.materialize(&self.model, effect, &ctx).unwrap();
            self.seq += 1;
        }
    }
    fn prepend(&mut self, slide: usize, text: &str) {
        self.intent(PptxIntent::InsertText {
            slide,
            target: Target::Notes,
            paragraph: 0,
            offset: 0,
            text: text.into(),
        });
    }
}
fn point(text: &str, character: usize) -> (usize, usize) {
    let byte = text
        .char_indices()
        .nth(character)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    let prefix = &text[..byte];
    (
        prefix.bytes().filter(|&b| b == b'\n').count(),
        prefix.rsplit('\n').next().unwrap().len(),
    )
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn stateful_notes_edits_restore_and_forks_match_an_independent_string(
        steps in prop::collection::vec((0u8..6,any::<usize>(),any::<usize>(),0usize..8),1..40),
    ) {
        let mut session=Session::new(&fixture());
        let mut live=Session::new(&fixture());
        let mut reference=session.model.slides()[0].notes.clone();
        for i in 0..24 { let text=format!("[{i}]字🙂"); session.prepend(1,&text); live.prepend(1,&text); reference.insert_str(0,&text); }
        for (kind,a,b,t) in steps {
            let length=reference.chars().count();
            let a=a%(length+1); let b=b%(length+1);
            match kind {
                0..=2 => {
                    let (a,b)=if kind==2 {(a.min(b),a.max(b))} else {(a,a)};
                    let choices=["", "字🙂", "a & < >", "e\u{301}", "\t", "\n", "fiمرحبا", "one\ntwo"];
                    let text=choices[t];
                    let (sp,so)=point(&reference,a); let (ep,eo)=point(&reference,b);
                    let intent=PptxIntent::ReplaceRange {slide:1,target:Target::Notes,start_paragraph:sp,start_offset:so,end_paragraph:ep,end_offset:eo,text:text.into()};
                    live.intent(intent.clone()); session.intent(intent);
                    let from=reference.char_indices().nth(a).map(|(i,_)|i).unwrap_or(reference.len());
                    let to=reference.char_indices().nth(b).map(|(i,_)|i).unwrap_or(reference.len());
                    reference.replace_range(from..to,text);
                }
                3 => {
                    let bytes=session.jade.export(&session.model).unwrap();
                    session.jade=JadePptx::restore(&bytes,&session.model).unwrap();
                    prop_assert_eq!(&PptxModel::from_pptx(&bytes).unwrap().slides()[0].notes,&reference);
                }
                4 => { session.jade=session.jade.fork(&session.model).unwrap(); }
                _ => {
                    let before=session.jade.bytes().to_vec();
                    let ctx=ApplyContext {origin:ReplicaId(1),base_seq:session.seq,seq:None};
                    let result=session.jade.materialize(&session.model,vec![jade_pptx_collab::PptxEffect::InsertText {
                        slide:0,target:jade_pptx_collab::TextTarget::Notes,paragraph:usize::MAX,offset:0,text:"refused".into(),
                    }],&ctx);
                    prop_assert!(result.is_err()); prop_assert_eq!(session.jade.bytes(),before);
                }
            }
            prop_assert_eq!(&session.model.slides()[0].notes,&reference);
            let native=jade_pptx_writer::notes_text(session.jade.bytes(),0).unwrap();
            prop_assert_eq!(&native.text,&reference);
            prop_assert!(session.jade.bytes()==live.jade.bytes(),"restore/fork changed native replay bytes");
        }
        let reopened=PptxModel::from_pptx(&session.jade.export(&session.model).unwrap()).unwrap();
        prop_assert_eq!(&reopened.slides()[0].notes,&reference);
    }
}

fn fragmented_fixture(count: usize) -> Vec<u8> {
    let mut writer = jade_pptx_writer::EditSession::open(&fixture()).unwrap();
    writer.set_slide_notes(0, "seed").unwrap();
    let slide = jade_pptx_reader::read_deck(writer.bytes()).unwrap().slides[0]
        .part_name
        .clone();
    let mut package = OpcPackage::open_preserving(writer.bytes()).unwrap();
    let rels =
        parse_relationships(&package.required_part_str(&rels_part_name(&slide)).unwrap()).unwrap();
    let part = resolve_target(&slide, &rels.by_type_suffix("/notesSlide")[0].target);
    let mut xml = package.required_part_str(&part).unwrap();
    let start = xml.find("<jadeDraw:r ").unwrap();
    let end = start + xml[start..].find("</jadeDraw:r>").unwrap() + "</jadeDraw:r>".len();
    let runs:String=(0..count).map(|i|format!("<a:r xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:t>[{i}]字🙂</a:t></a:r>")).collect();
    xml.replace_range(start..end, &runs);
    package.replace(&part, xml.into_bytes());
    package.write().unwrap()
}
#[test]
fn moving_a_large_notes_story_compacts_at_its_current_ordinal() {
    let mut s = Session::new(&fragmented_fixture(128));
    s.prepend(2, "warm");
    s.intent(PptxIntent::MoveSlide { slide: 1, to: 2 });
    s.prepend(2, "after move");
    let deck = jade_pptx_reader::read_deck(s.jade.bytes()).unwrap();
    let package = OpcPackage::open(s.jade.bytes()).unwrap();
    let rels = parse_relationships(
        &package
            .required_part_str(&rels_part_name(&deck.slides[1].part_name))
            .unwrap(),
    )
    .unwrap();
    let xml = package
        .required_part_str(&resolve_target(
            &deck.slides[1].part_name,
            &rels.by_type_suffix("/notesSlide")[0].target,
        ))
        .unwrap();
    assert!(runs(&xml) <= 17);
    assert_eq!(
        jade_pptx_writer::notes_text(s.jade.bytes(), 1)
            .unwrap()
            .text,
        s.model.slides()[1].notes
    );
}
#[test]
fn a_noop_does_not_publish_a_staged_xml_compaction() {
    let bytes = fragmented_fixture(64);
    let model = PptxModel::from_pptx(&bytes).unwrap();
    let mut jade = JadePptx::open(&bytes).unwrap();
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: None,
    };
    jade.materialize(
        &model,
        vec![jade_pptx_collab::PptxEffect::InsertText {
            slide: 0,
            target: jade_pptx_collab::TextTarget::Notes,
            paragraph: 0,
            offset: 0,
            text: String::new(),
        }],
        &ctx,
    )
    .unwrap();
    assert_eq!(jade.bytes(), bytes);
    assert!(!jade.session().is_dirty());
    // A subsequent actual edit still normalizes the restored story.
    let next = model.max_lamport() + 100;
    let mut session = Session {
        model,
        jade,
        seq: 0,
        next,
    };
    session.prepend(1, "real edit");
    assert!(
        runs(&notes_xml(&session.jade)) <= 17,
        "the actual edit did not normalize the restored story"
    );
}

#[test]
fn locked_notes_refusal_preserves_the_original_archive_even_when_compaction_is_due() {
    let bytes = fragmented_fixture(64);
    let deck = jade_pptx_reader::read_deck(&bytes).unwrap();
    let mut package = OpcPackage::open_preserving(&bytes).unwrap();
    let rels = parse_relationships(
        &package
            .required_part_str(&rels_part_name(&deck.slides[0].part_name))
            .unwrap(),
    )
    .unwrap();
    let part = resolve_target(
        &deck.slides[0].part_name,
        &rels.by_type_suffix("/notesSlide")[0].target,
    );
    let xml = package.required_part_str(&part).unwrap();
    assert!(xml.contains("<n:cNvSpPr/>"));
    package.replace(
        &part,
        xml.replacen(
            "<n:cNvSpPr/>",
            "<n:cNvSpPr><d:spLocks noTextEdit=\"1\"/></n:cNvSpPr>",
            1,
        )
        .into_bytes(),
    );
    let bytes = package.write().unwrap();
    assert!(jade_pptx_writer::notes_text(&bytes, 0).unwrap().locked);
    let model = PptxModel::from_pptx(&bytes).unwrap();
    let mut jade = JadePptx::open(&bytes).unwrap();
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: None,
    };
    assert!(jade
        .materialize(
            &model,
            vec![jade_pptx_collab::PptxEffect::InsertText {
                slide: 0,
                target: jade_pptx_collab::TextTarget::Notes,
                paragraph: 0,
                offset: 0,
                text: "refused".into()
            }],
            &ctx
        )
        .is_err());
    assert_eq!(jade.bytes(), bytes);
    assert!(!jade.session().is_dirty());
}

#[test]
fn note_compaction_and_typing_leave_every_other_package_part_exact() {
    let bytes = fragmented_fixture(128);
    let mut session = Session::new(&bytes);
    let deck = jade_pptx_reader::read_deck(&bytes).unwrap();
    let old = OpcPackage::open(&bytes).unwrap();
    let rels = parse_relationships(
        &old.required_part_str(&rels_part_name(&deck.slides[0].part_name))
            .unwrap(),
    )
    .unwrap();
    let note = resolve_target(
        &deck.slides[0].part_name,
        &rels.by_type_suffix("/notesSlide")[0].target,
    );
    session.prepend(1, "typed🙂");
    let new = OpcPackage::open(session.jade.bytes()).unwrap();
    assert_eq!(old.part_names(), new.part_names());
    for part in old.part_names() {
        if part != &note {
            assert_eq!(old.part(part), new.part(part), "untouched part {part}");
        }
    }
    assert!(runs(&notes_xml(&session.jade)) <= 17);
}

#[test]
#[ignore = "requires JADE_NOTES_GROWN_PPTX; compares the old native notes path with the materializer"]
fn grown_notes_native_path_comparison() {
    use std::time::Instant;
    let input = std::env::var("JADE_NOTES_GROWN_PPTX").expect("set JADE_NOTES_GROWN_PPTX");
    let bytes = std::fs::read(input).unwrap();
    let mut native = jade_pptx_writer::EditSession::open(&bytes).unwrap();
    let mut optimized = Session::new(&bytes);
    let mut reference = optimized.model.slides()[0].notes.clone();
    let before_runs = runs(&notes_xml(&optimized.jade));
    let mut baseline_ms = Vec::new();
    let mut optimized_ms = Vec::new();
    for i in 0..32 {
        let text = format!("[bench{i}]字🙂");
        let begin = Instant::now();
        // The previous materializer read native text for position conversion,
        // read it again for the guarded input, then called the native writer.
        let position = jade_pptx_writer::notes_text(native.bytes(), 0).unwrap();
        assert_eq!(position.text, reference);
        let expected_text = jade_pptx_writer::notes_text(native.bytes(), 0)
            .unwrap()
            .text;
        native
            .apply_notes_input(
                0,
                &jade_pptx_writer::NotesInputEdit {
                    start: 0,
                    end: 0,
                    text: text.clone(),
                    expected_text,
                    soft_break: false,
                    history: None,
                },
            )
            .unwrap();
        baseline_ms.push(begin.elapsed().as_secs_f64() * 1000.0);

        let intent = PptxIntent::InsertText {
            slide: 1,
            target: Target::Notes,
            paragraph: 0,
            offset: 0,
            text: text.clone(),
        };
        let ops = jade_pptx_collab::intent::resolve(&optimized.model, &intent, &mut |n| {
            let id = Id::new(optimized.next, ReplicaId(1));
            optimized.next += n;
            id
        })
        .unwrap();
        assert_eq!(ops.len(), 1);
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: optimized.seq,
            seq: Some(optimized.seq + 1),
        };
        let effect = optimized.model.apply(&ops[0], &ctx).unwrap();
        let begin = Instant::now();
        optimized
            .jade
            .materialize(&optimized.model, effect, &ctx)
            .unwrap();
        optimized_ms.push(begin.elapsed().as_secs_f64() * 1000.0);
        optimized.seq += 1;
        reference.insert_str(0, &text);
        assert_eq!(
            jade_pptx_writer::notes_text(native.bytes(), 0)
                .unwrap()
                .text,
            reference
        );
        assert_eq!(
            jade_pptx_writer::notes_text(optimized.jade.bytes(), 0)
                .unwrap()
                .text,
            reference
        );
        assert!(runs(&notes_xml(&optimized.jade)) <= 17);
    }
    let old = OpcPackage::open(native.bytes()).unwrap();
    let new = OpcPackage::open(optimized.jade.bytes()).unwrap();
    let deck = jade_pptx_reader::read_deck(native.bytes()).unwrap();
    let rels = parse_relationships(
        &old.required_part_str(&rels_part_name(&deck.slides[0].part_name))
            .unwrap(),
    )
    .unwrap();
    let target = resolve_target(
        &deck.slides[0].part_name,
        &rels.by_type_suffix("/notesSlide")[0].target,
    );
    assert_eq!(old.part_names(), new.part_names());
    for part in old.part_names() {
        if part != &target {
            assert_eq!(old.part(part), new.part(part), "untouched {part}");
        }
    }
    baseline_ms.sort_by(f64::total_cmp);
    optimized_ms.sort_by(f64::total_cmp);
    let report = serde_json::json!({"samples":32,"before_runs":before_runs,"after_runs":runs(&notes_xml(&optimized.jade)),
        "baseline_native_ms":{"median":baseline_ms[15],"p95":baseline_ms[30],"total":baseline_ms.iter().sum::<f64>()},
        "optimized_materializer_ms":{"median":optimized_ms[15],"p95":optimized_ms[30],"total":optimized_ms.iter().sum::<f64>()},
        "baseline_native_retained_bytes":native.retained_archive_bytes(),"optimized_native_retained_bytes":optimized.jade.session().retained_archive_bytes(),
        "same_native_text":true,"all_other_parts_exact":true});
    println!("NOTES_COMPARISON {report}");
    if let Some(path) = std::env::var_os("JADE_NOTES_COMPARISON_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
