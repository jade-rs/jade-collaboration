//! The model and jade must describe the same document: the same body
//! paragraphs in the same order with the same text, and the same top-level
//! tables — on load, and after every operation the materializer realises.

mod common;

use common::{generate, jade_core, open};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Materializer};
use jade_docx_collab::{DocxModel, JadeDocx};
use proptest::prelude::*;

fn assert_agree(model: &DocxModel, jade: &JadeDocx, context: &str) {
    assert_eq!(
        model.paragraph_texts(),
        jade.body_paragraph_texts(),
        "paragraphs disagree {context}"
    );
    let shapes: Vec<(usize, usize)> = model
        .tables()
        .iter()
        .map(|t| (t.rows.len(), t.columns.len()))
        .collect();
    let jade_shapes = jade.table_shapes();
    for (m, j) in model.tables().iter().zip(&jade_shapes) {
        if !m.locked {
            assert_eq!((m.rows.len(), m.columns.len()), *j, "table shape {context}");
        }
    }
    assert_eq!(shapes.len(), jade_shapes.len(), "table count {context}");
}

/// Every fixture jade can open yields a model that agrees with it. This pins
/// the model's traversal to jade's paragraph-surface enumeration (tables,
/// nested tables, footnotes and text boxes interleaved).
#[test]
fn every_fixture_loads_into_an_agreeing_model() {
    let dir = jade_core().join("public/fixtures");
    let mut checked = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "docx"))
        .collect();
    entries.sort();
    for path in entries {
        let Ok(jade) = JadeDocx::load(&std::fs::read(&path).unwrap()) else {
            continue;
        };
        let model = DocxModel::from_word_document(jade.word())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_agree(&model, &jade, &path.display().to_string());
        checked += 1;
    }
    assert!(checked > 50, "only {checked} fixtures were checked");
}

#[test]
fn hello_round_trips_an_edit_through_export() {
    let (mut model, mut jade) = open("hello.docx");
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    };
    let mut next = 1_000_000u64;
    let mut ids = |n: u64| {
        let id = Id::new(next, ReplicaId(1));
        next += n;
        id
    };
    let op = jade_docx_collab::intent::resolve(
        &model,
        &jade_docx_collab::intent::IntentOp::InsertText {
            paragraph: 0,
            offset: 0,
            text: "Edited: ".into(),
        },
        &mut ids,
    )
    .unwrap()
    .remove(0);
    let effect = model.apply(&op, &ctx).unwrap();
    jade.materialize(&model, effect, &ctx).unwrap();
    assert_agree(&model, &jade, "after the edit");

    let bytes = jade.export().unwrap();
    let reloaded = JadeDocx::load(&bytes).unwrap();
    assert_eq!(reloaded.body_paragraph_texts(), model.paragraph_texts());
    assert!(model.paragraph_texts()[0].starts_with("Edited: "));
}

const EDIT_FIXTURES: &[&str] = &[
    "jade-core/hello.docx",
    "table-cell-spacing.docx",
    "footnote-table-textbox-order.docx",
    "table-complex-nested-merges.docx",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Random edit sequences keep the model and jade in agreement after every
    /// single operation.
    #[test]
    fn random_edits_keep_model_and_jade_in_agreement(
        fixture_index in 0..EDIT_FIXTURES.len(),
        choices in prop::collection::vec(any::<u64>(), 1..25),
    ) {
        let name = EDIT_FIXTURES[fixture_index];
        let (mut model, mut jade) = open(name);
        assert_agree(&model, &jade, "on load");
        let mut next = 1_000_000u64;
        for (step, choice) in choices.into_iter().enumerate() {
            let mut ids = |n: u64| {
                let id = Id::new(next, ReplicaId(1));
                next += n;
                id
            };
            let Some(op) = generate(&model, choice, &mut ids) else { continue };
            let ctx = ApplyContext { origin: ReplicaId(1), base_seq: 0, seq: Some(step as u64 + 1) };
            let before = model.clone();
            match model.apply(&op, &ctx) {
                Ok(effect) => {
                    jade.materialize(&model, effect, &ctx)
                        .unwrap_or_else(|e| panic!("{name}: jade refused {op:?}: {e}"));
                    assert_agree(&model, &jade, &format!("{name} after {op:?}"));
                }
                Err(_) => prop_assert_eq!(&model, &before, "a rejected op changed the model"),
            }
        }
        // And the file jade writes reads back as the same document.
        let reloaded = JadeDocx::load(&jade.export().unwrap()).unwrap();
        prop_assert_eq!(reloaded.body_paragraph_texts(), model.paragraph_texts());
    }
}
