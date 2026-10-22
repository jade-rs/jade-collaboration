mod common;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_mcp::read_pptx;
use jade_collab_sync::{ApplyContext, Document};
use jade_pptx_collab::intent::{resolve, PptxIntent};
use jade_pptx_collab::{CharFormat, PptxModel};
use serde_json::json;

#[test]
fn slides_report_hidden_state_and_explicit_character_formatting_in_utf8() {
    let mut model = PptxModel::from_texts(&[&["a日🙂b"], &["other"]]);
    let shape = model.slides()[0].shapes[0].number;
    for intent in [
        PptxIntent::FormatText {
            slide: 1,
            shape,
            start_paragraph: 0,
            start_offset: 1,
            end_paragraph: 0,
            end_offset: 8,
            format: CharFormat::Bold { value: false },
        },
        PptxIntent::FormatText {
            slide: 1,
            shape,
            start_paragraph: 0,
            start_offset: 4,
            end_paragraph: 0,
            end_offset: 8,
            format: CharFormat::Italic { value: true },
        },
        PptxIntent::SetSlideHidden {
            slide: 1,
            hidden: true,
        },
    ] {
        let mut next = model.max_lamport() + 1;
        let ops = resolve(&model, &intent, &mut |n| {
            let first = Id::new(next, ReplicaId(1));
            next += n;
            first
        })
        .unwrap();
        for op in ops {
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
    }
    let read = read_pptx(&model, 0, 1);
    assert_eq!(read["formatting_scope"], "collaborative_overrides");
    assert_eq!(read["slides"][0]["hidden"], true);
    assert_eq!(read["slides"][0]["shapes"][0]["aspect_locked"], false);
    assert_eq!(read["slides"][0]["shapes"][0]["group"], json!(null));
    assert_eq!(
        read["slides"][0]["shapes"][0]["formatting"],
        json!([
            {"paragraph":0,"start":1,"end":8,"format":{"attribute":"bold","value":false}},
            {"paragraph":0,"start":4,"end":8,"format":{"attribute":"italic","value":true}}
        ])
    );
    assert_eq!(read["next"], 1);
    assert_eq!(read_pptx(&model, 1, 1)["slides"][0]["hidden"], false);
    assert_eq!(
        read_pptx(&model, 1, 1)["slides"][0]["shapes"][0]["formatting"],
        json!([])
    );
}

#[test]
fn loaded_group_children_name_their_parent_by_the_public_shape_number() {
    let model =
        PptxModel::from_pptx(&common::fixture("public/fixtures/grouped-deck.pptx")).unwrap();
    check_read(&model);
    let read = read_pptx(&model, 0, usize::MAX);
    let mut children = 0;
    for slide in read["slides"].as_array().unwrap() {
        let shapes = slide["shapes"].as_array().unwrap();
        for shape in shapes {
            if shape["group"].is_number() {
                let parent = shapes
                    .iter()
                    .find(|s| s["shape"] == shape["group"])
                    .unwrap();
                assert_eq!(parent["kind"], "group");
                children += 1;
            }
        }
    }
    assert!(children > 0);
}

fn check_read(model: &PptxModel) {
    use jade_pptx_collab::StoryId;
    use std::collections::BTreeMap;
    let read = read_pptx(model, 0, usize::MAX);
    assert_eq!(read["slide_count"], model.slides().len());
    for (index, (slide, shown)) in model
        .slides()
        .iter()
        .zip(read["slides"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(shown["slide"], index + 1);
        assert_eq!(shown["hidden"], slide.hidden);
        assert_eq!(
            shown["shapes"].as_array().unwrap().len(),
            slide.shapes.len()
        );
        for (shape, shown) in slide.shapes.iter().zip(shown["shapes"].as_array().unwrap()) {
            assert_eq!(shown["shape"], shape.number);
            assert_eq!(shown["aspect_locked"], shape.aspect_locked);
            let group = shape
                .group
                .and_then(|id| slide.shapes.iter().find(|s| s.id == id).map(|s| s.number));
            assert_eq!(shown["group"], json!(group));
            let story = StoryId::Shape {
                slide: slide.id,
                shape: shape.id,
            };
            let paragraphs = model.paragraphs(story).unwrap_or_default();
            let all = model.char_formats(story).unwrap_or_default();
            let mut expected = BTreeMap::new();
            for (p, paragraph) in paragraphs.iter().enumerate() {
                for id in &paragraph.chars {
                    for format in all.get(id).into_iter().flatten() {
                        expected.insert((p, *id, format.key()), format.clone());
                    }
                }
            }
            let mut actual = BTreeMap::new();
            let mut previous: Option<(usize, jade_pptx_collab::MarkKey, usize, CharFormat)> = None;
            for span in shown["formatting"].as_array().unwrap() {
                let p = span["paragraph"].as_u64().unwrap() as usize;
                let start = span["start"].as_u64().unwrap() as usize;
                let end = span["end"].as_u64().unwrap() as usize;
                let format: CharFormat = serde_json::from_value(span["format"].clone()).unwrap();
                let paragraph = &paragraphs[p];
                assert!(start < end && end <= paragraph.text.len());
                assert!(
                    paragraph.text.is_char_boundary(start) && paragraph.text.is_char_boundary(end)
                );
                if let Some((prior_p, prior_key, prior_end, prior_format)) = previous {
                    assert!((prior_p, prior_key, prior_end) <= (p, format.key(), start));
                    assert!(
                        prior_p != p
                            || prior_key != format.key()
                            || prior_end != start
                            || prior_format != format
                    );
                }
                for ((offset, _), id) in paragraph.text.char_indices().zip(&paragraph.chars) {
                    if start <= offset && offset < end {
                        assert!(actual
                            .insert((p, *id, format.key()), format.clone())
                            .is_none());
                    }
                }
                previous = Some((p, format.key(), end, format));
            }
            assert_eq!(actual, expected, "slide {index}, shape {}", shape.number);
        }
    }
    for start in (0..model.slides().len()).step_by(2) {
        assert_eq!(
            read_pptx(model, start, 2)["slides"],
            json!(read["slides"].as_array().unwrap()[start..(start + 2).min(model.slides().len())])
        );
    }
}

fn format(bits: u64) -> CharFormat {
    match bits % 7 {
        0 => CharFormat::Bold {
            value: bits & 16 != 0,
        },
        1 => CharFormat::Italic {
            value: bits & 16 != 0,
        },
        2 => CharFormat::Underline {
            value: bits & 16 != 0,
        },
        3 => CharFormat::Size { hundredths: 1250 },
        4 => CharFormat::Font {
            typeface: "Arial".into(),
        },
        5 => CharFormat::EastAsianFont {
            typeface: "Yu Gothic".into(),
        },
        _ => CharFormat::Color {
            color: "#00AA00".into(),
        },
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(48))]
    #[test]
    fn pptx_reads_keep_format_ownership_through_duplicates_moves_delivery_and_restarts(
        steps in proptest::collection::vec((0usize..3, 0u8..11, proptest::num::u64::ANY), 1..40),
    ) {
        use jade_collab_sync::{sim::Simulation, NoMaterializer, Sequencer};
        let mut sim = Simulation::new(PptxModel::from_texts(&[&["a日🙂b", "second"], &["other"]]), 3);
        for (client, kind, bits) in steps {
            match kind {
                7 => { sim.deliver_up(client); }
                8 => { sim.deliver_down(client).unwrap(); }
                9 => { sim.reconnect().unwrap(); }
                10 => {
                    let checkpoint = sim.checkpoint();
                    let checkpoint = serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
                    sim.restart(Sequencer::restore(checkpoint, NoMaterializer)).unwrap();
                }
                _ => {
                    let view = sim.client(client).view().clone();
                    let slides = view.slides();
                    let index = bits as usize % slides.len();
                    let slide = index + 1;
                    let text_shapes: Vec<_> = slides[index].shapes.iter().filter(|s| s.editable && s.text.as_ref().is_some_and(|s| !s.is_empty())).collect();
                    let intent = match kind {
                        0 | 1 if !text_shapes.is_empty() => {
                            let shape = text_shapes[(bits / 16) as usize % text_shapes.len()];
                            let end = shape.text.as_ref().unwrap().split('\n').next().unwrap().len();
                            if kind == 0 { PptxIntent::FormatText { slide, shape: shape.number, start_paragraph: 0, start_offset: 0, end_paragraph: 0, end_offset: end, format: format(bits / 64) } }
                            else { PptxIntent::InsertText { slide, target: jade_pptx_collab::intent::Target::Shape(shape.number), paragraph: 0, offset: end, text: "日🙂".into() } }
                        }
                        2 => PptxIntent::DuplicateSlide { slide },
                        3 => PptxIntent::MoveSlide { slide, to: (bits / 16) as usize % slides.len() + 1 },
                        4 => PptxIntent::SetSlideHidden { slide, hidden: bits & 16 != 0 },
                        5 => {
                            let Some(shape) = slides[index].shapes.iter().find(|s| s.deletable) else { continue; };
                            PptxIntent::DeleteShape { slide, shape: shape.number }
                        }
                        6 => PptxIntent::DeleteSlide { slide },
                        _ => continue,
                    };
                    if let Ok(ops) = jade_pptx_collab::intent::try_resolve(&view, &intent, &mut |n| sim.client_mut(client).try_reserve(n)) {
                        for op in ops { let _ = sim.local(client, op); }
                    }
                }
            }
            check_read(sim.server().document());
            for client in 0..3 { check_read(sim.client(client).view()); }
        }
        sim.quiesce().unwrap();
        for client in 0..3 { proptest::prop_assert_eq!(sim.client(client).view(), sim.server().document()); }
    }
}
