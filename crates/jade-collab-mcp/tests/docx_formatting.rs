mod common;

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::marks::{self, Mark};
use jade_collab_core::rga::Rga;
use jade_collab_mcp::read_docx;
use jade_collab_sync::{ApplyContext, Document};
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{Atom, DocxModel, DocxOp, Expand, FormatChange, JadeDocx, MarkKey, StoryId};
use proptest::prelude::*;
use serde_json::json;

fn apply(model: &mut DocxModel, op: &DocxOp) {
    model
        .apply(
            op,
            &ApplyContext {
                origin: ReplicaId(1),
                base_seq: 0,
                seq: None,
            },
        )
        .unwrap();
}

fn format(model: &mut DocxModel, start: usize, end: usize, change: FormatChange) {
    let mut next = model.max_lamport() + 1;
    let ops = resolve(
        model,
        &IntentOp::Format {
            start_paragraph: 0,
            start_offset: start,
            end_paragraph: 0,
            end_offset: end,
            change,
        },
        &mut |n| {
            let first = Id::new(next, ReplicaId(1));
            next += n;
            first
        },
    )
    .unwrap();
    for op in ops {
        apply(model, &op);
    }
}

#[test]
fn reads_explicit_false_overlaps_clears_and_utf8_spans_without_claiming_the_base_style() {
    let mut model = DocxModel::from_paragraphs(&["a日🙂b", "tail"]);
    format(&mut model, 0, 9, FormatChange::Bold { value: true });
    format(&mut model, 1, 8, FormatChange::Italic { value: true });
    format(
        &mut model,
        4,
        8,
        FormatChange::Highlight {
            color: "yellow".into(),
        },
    );
    format(&mut model, 4, 8, FormatChange::Bold { value: false });
    format(&mut model, 1, 4, FormatChange::ClearFormatting);
    let read = read_docx(&model, 0, 1);
    assert_eq!(read["formatting_scope"], "collaborative_overrides");
    assert_eq!(
        read["paragraphs"][0]["formatting"],
        json!([
            {"start":0,"end":1,"change":{"kind":"bold","value":true}},
            {"start":4,"end":8,"change":{"kind":"bold","value":false}},
            {"start":8,"end":9,"change":{"kind":"bold","value":true}},
            {"start":4,"end":8,"change":{"kind":"italic","value":true}},
            {"start":4,"end":8,"change":{"kind":"highlight","color":"yellow"}},
        ])
    );
    assert_eq!(read["next"], 1);
    assert_eq!(
        read_docx(&model, 1, 1)["paragraphs"][0]["formatting"],
        json!([])
    );
    assert_eq!(
        read_docx(&model, usize::MAX, usize::MAX)["paragraphs"],
        json!([])
    );
}

#[test]
fn formatting_is_attached_to_the_right_body_cell_note_and_text_box_paragraph() {
    let jade = JadeDocx::load(&common::fixture(
        "public/fixtures/footnote-table-textbox-order.docx",
    ))
    .unwrap();
    let mut model = DocxModel::from_word_document(jade.word()).unwrap();
    for (i, paragraph) in model.paragraphs().into_iter().enumerate() {
        let Some(first) = paragraph.chars.first().copied() else {
            continue;
        };
        let (start, end) = model
            .mark_anchors(paragraph.story, first, first, Expand::End)
            .unwrap();
        let op = DocxOp::Mark {
            story: paragraph.story,
            id: Id::new(model.max_lamport() + 1, ReplicaId(1)),
            start,
            end,
            change: FormatChange::Bold { value: i % 2 == 0 },
        };
        apply(&mut model, &op);
    }
    for start in 0..model.paragraphs().len() {
        let read = read_docx(&model, start, 1);
        let p = &read["paragraphs"][0];
        let text = p["text"].as_str().unwrap();
        let expected = text
            .chars()
            .next()
            .map(|c| {
                json!([
                    {"start":0,"end":c.len_utf8(),"change":{"kind":"bold","value":start % 2 == 0}}
                ])
            })
            .unwrap_or_else(|| json!([]));
        assert_eq!(p["formatting"], expected, "paragraph {start}");
    }
}

fn attribute(bits: u64) -> FormatChange {
    match bits % 10 {
        0 => FormatChange::Bold { value: false },
        1 => FormatChange::Bold { value: true },
        2 => FormatChange::Italic {
            value: bits & 16 != 0,
        },
        3 => FormatChange::Underline {
            value: bits & 16 != 0,
            style: Some("double".into()),
        },
        4 => FormatChange::Strike {
            value: bits & 16 != 0,
            double: true,
        },
        5 => FormatChange::FontSize {
            half_points: 20 + (bits % 10) as u32,
        },
        6 => FormatChange::FontFamily {
            family: "Yu Gothic".into(),
        },
        7 => FormatChange::FontColor {
            color: "#00AA00".into(),
        },
        8 => FormatChange::Highlight {
            color: "yellow".into(),
        },
        _ => FormatChange::ClearFormatting,
    }
}

fn check_projection(model: &DocxModel, flow: &Rga<Atom>, history: &[Mark<MarkKey, FormatChange>]) {
    use std::collections::BTreeMap;
    let mut text = vec![String::new()];
    let mut ids = Vec::new();
    for (id, atom) in flow.visible() {
        match atom {
            Atom::Char(c) => {
                text.last_mut().unwrap().push(*c);
                ids.push(id);
            }
            Atom::Mark => text.push(String::new()),
            _ => panic!("unexpected anchor"),
        }
    }
    let paragraphs = model.paragraphs();
    assert_eq!(model.paragraph_texts(), text);
    assert_eq!(
        paragraphs
            .iter()
            .flat_map(|p| p.chars.clone())
            .collect::<Vec<_>>(),
        ids
    );
    let read = read_docx(model, 0, usize::MAX);
    assert_eq!(read["formatting_scope"], "collaborative_overrides");
    let order = flow.ids();
    for (paragraph, entry) in paragraphs
        .iter()
        .zip(read["paragraphs"].as_array().unwrap())
    {
        let mut expected = BTreeMap::new();
        for id in &paragraph.chars {
            for key in MarkKey::ALL {
                if let Some((_, Some(change))) = marks::spec::effective(&order, history, *id, &key)
                {
                    expected.insert((*id, key), change);
                }
            }
        }
        let mut actual = BTreeMap::new();
        let mut previous: Option<(MarkKey, usize, FormatChange)> = None;
        for span in entry["formatting"].as_array().unwrap() {
            let start = span["start"].as_u64().unwrap() as usize;
            let end = span["end"].as_u64().unwrap() as usize;
            assert!(start < end && end <= paragraph.text.len());
            assert!(paragraph.text.is_char_boundary(start) && paragraph.text.is_char_boundary(end));
            let change: FormatChange = serde_json::from_value(span["change"].clone()).unwrap();
            let key = change.mark_key().unwrap();
            if let Some((prior_key, prior_end, prior_value)) = previous {
                assert!(
                    (prior_key, prior_end) <= (key, start),
                    "sorted disjoint runs per attribute"
                );
                assert!(
                    prior_key != key || prior_end != start || prior_value != change,
                    "adjacent equal runs must coalesce"
                );
            }
            for ((offset, _), id) in paragraph.text.char_indices().zip(&paragraph.chars) {
                if start <= offset && offset < end {
                    assert!(actual.insert((*id, key), change.clone()).is_none());
                }
            }
            previous = Some((key, end, change));
        }
        assert_eq!(actual, expected);
    }
    // Pages carry only their own paragraphs' spans, preserving their exact
    // representation and order, including pages with no formatting.
    for start in (0..paragraphs.len()).step_by(3) {
        let page = read_docx(model, start, 3);
        assert_eq!(
            page["paragraphs"],
            json!(read["paragraphs"].as_array().unwrap()[start..(start + 3).min(paragraphs.len())])
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn formatting_reads_match_the_mark_spec_through_text_structure_and_checkpoint_histories(
        steps in prop::collection::vec((0u8..8, any::<u64>(), any::<u64>()), 1..55),
    ) {
        let mut model = DocxModel::from_paragraphs(&["a日🙂b", "xy", ""]);
        let mut flow = Rga::new();
        let mut parent = None;
        for paragraph in model.paragraphs() {
            for (c, id) in paragraph.text.chars().zip(paragraph.chars) {
                flow.insert(id, parent, Atom::Char(c)).unwrap(); parent = Some(id);
            }
            if let Some(id) = paragraph.mark { flow.insert(id, parent, Atom::Mark).unwrap(); parent = Some(id); }
        }
        let mut history: Vec<Mark<MarkKey, FormatChange>> = Vec::new();
        for (kind, a, b) in steps {
            if kind == 7 { model = serde_json::from_value(serde_json::to_value(&model).unwrap()).unwrap(); check_projection(&model, &flow, &history); continue; }
            let id = Id::new(model.max_lamport() + 1, ReplicaId(1));
            let all = flow.ids();
            let chars: Vec<_> = flow.visible().filter_map(|(id, value)| matches!(value, Atom::Char(_)).then_some(id)).collect();
            let op = match kind {
                0 => DocxOp::InsertText { story: StoryId::Body,
                    parent: (a & 1 == 1 && !all.is_empty()).then(|| all[a as usize % all.len()]), first: id,
                    text: ["x", "日", "🙂"][b as usize % 3].into() },
                1 if !chars.is_empty() => DocxOp::DeleteText { story: StoryId::Body, ids: vec![chars[a as usize % chars.len()]] },
                2 => DocxOp::SplitParagraph { story: StoryId::Body,
                    parent: (a & 1 == 1 && !all.is_empty()).then(|| all[a as usize % all.len()]), id },
                3 => {
                    let marks: Vec<_> = model.paragraphs().iter().filter_map(|p| p.mark).collect();
                    if marks.is_empty() { continue; }
                    DocxOp::JoinParagraph { story: StoryId::Body, mark: marks[a as usize % marks.len()] }
                }
                4..=6 if !chars.is_empty() => {
                    let (first, last) = (a as usize % chars.len(), b as usize % chars.len());
                    let (start, end) = marks::anchors(&flow, chars[first.min(last)], chars[first.max(last)], Expand::End).unwrap();
                    DocxOp::Mark { story: StoryId::Body, id, start, end, change: attribute(b) }
                }
                _ => continue,
            };
            apply(&mut model, &op);
            match op {
                DocxOp::InsertText { first, parent, text, .. } => { flow.insert_run(first, parent, text.chars().map(Atom::Char)).unwrap(); }
                DocxOp::SplitParagraph { id, parent, .. } => { flow.insert(id, parent, Atom::Mark).unwrap(); }
                DocxOp::JoinParagraph { mark, .. } => { flow.delete(mark).unwrap(); }
                DocxOp::DeleteText { ids, .. } => for id in ids { flow.delete(id).unwrap(); },
                DocxOp::Mark { id, start, end, change, .. } => {
                    if change == FormatChange::ClearFormatting {
                        for key in MarkKey::ALL.into_iter().filter(|k| *k != MarkKey::Highlight) { history.push(Mark::new(id, key, start, end, None)); }
                    } else { history.push(Mark::new(id, change.mark_key().unwrap(), start, end, Some(change))); }
                }
                _ => unreachable!(),
            }
            check_projection(&model, &flow, &history);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn format_reads_follow_peers_and_keep_their_pinned_utf8_positions_and_values() {
    use common::{statuses, Agent, Hosted, DOCX};
    let hosted = Hosted::start().await;
    let (mut reader, mut writer) = (Agent::new(), Agent::new());
    let session = reader.join(&hosted.url(DOCX)).await;
    let editor = writer.join(&hosted.url(DOCX)).await;
    let initial = reader.read_all(&session).await;
    assert_eq!(statuses(&writer.apply(&editor, json!([
        {"type":"format","start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":5,"change":{"kind":"bold","value":false}}
    ])).await), ["applied"]);
    let seq = hosted.server.seq(DOCX).await.unwrap();
    reader
        .tool(
            "wait_for_changes",
            json!({"session_id":session,"after_seq":seq-1}),
        )
        .await;
    assert_eq!(
        reader
            .tool(
                "read_document",
                json!({"session_id":session,"snapshot":initial["snapshot"],"limit":100_000})
            )
            .await,
        initial
    );
    let pinned = reader.read_all(&session).await;
    assert_eq!(
        pinned["paragraphs"][0]["formatting"],
        json!([
            {"start":0,"end":5,"change":{"kind":"bold","value":false}}
        ])
    );
    // The reported explicit false is the property actually written to Word.
    let jade = JadeDocx::load(&hosted.server.export(DOCX).await.unwrap()).unwrap();
    let paragraph = jade.word().paragraph(0).unwrap();
    let run = paragraph
        .runs
        .iter()
        .find(|run| !run.text.is_empty())
        .unwrap();
    assert_eq!(run.properties.as_ref().and_then(|p| p.bold), Some(false));

    assert_eq!(
        statuses(
            &writer
                .apply(
                    &editor,
                    json!([
                        {"type":"insert_text","paragraph":0,"offset":0,"text":"日"}
                    ])
                )
                .await
        ),
        ["applied"]
    );
    let seq = hosted.server.seq(DOCX).await.unwrap();
    reader
        .tool(
            "wait_for_changes",
            json!({"session_id":session,"after_seq":seq-1}),
        )
        .await;
    assert_eq!(
        reader
            .tool(
                "read_document",
                json!({"session_id":session,"snapshot":pinned["snapshot"],"limit":100_000})
            )
            .await,
        pinned
    );
    let current = reader.read_all(&session).await;
    assert_eq!(
        current["paragraphs"][0]["formatting"],
        json!([
            {"start":3,"end":8,"change":{"kind":"bold","value":false}}
        ])
    );
    assert_eq!(statuses(&writer.apply(&editor, json!([
        {"type":"format","start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":8,"change":{"kind":"clear_formatting"}}
    ])).await), ["applied"]);
    let seq = hosted.server.seq(DOCX).await.unwrap();
    reader
        .tool(
            "wait_for_changes",
            json!({"session_id":session,"after_seq":seq-1}),
        )
        .await;
    assert_eq!(
        reader.read_all(&session).await["paragraphs"][0]["formatting"],
        json!([])
    );
}
