//! Text edits and formatting interleaved across replicas. A format op names
//! its range by character ids, so whatever happened to the text before it
//! committed, it lands on exactly the characters that were in its range at
//! its log position — on the server's jade document, whatever order the
//! messages travelled in.

mod common;

use std::collections::BTreeMap;

use common::{char_styles, generate, open};
use jade_collab_core::id::Id;
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{ApplyContext, Document};
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{DocxOp, FormatChange};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Step {
    Type { client: usize, choice: u64 },
    Format { client: usize, choice: u64 },
    Up { client: usize },
    Down { client: usize },
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            3 => (0..3usize, any::<u64>()).prop_map(|(client, choice)| Step::Type { client, choice }),
            2 => (0..3usize, any::<u64>()).prop_map(|(client, choice)| Step::Format { client, choice }),
            2 => (0..3usize).prop_map(|client| Step::Up { client }),
            2 => (0..3usize).prop_map(|client| Step::Down { client }),
        ],
        0..60,
    )
}

const CHANGES: [FormatChange; 5] = [
    FormatChange::Bold { value: true },
    FormatChange::Bold { value: false },
    FormatChange::Italic { value: true },
    FormatChange::Italic { value: false },
    FormatChange::ClearFormatting,
];

/// What a character must look like at the end: known, or left to jade
/// (text typed next to formatted text takes its neighbour's formatting, as
/// in Word; clearing restores whatever the style says).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Expected {
    bold: Option<bool>,
    italic: Option<bool>,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn formatting_lands_on_the_characters_in_range_at_its_log_position(
        fixture in prop::sample::select(vec!["jade-core/hello.docx", "table-cell-spacing.docx"]),
        steps in steps(),
    ) {
        let (model, jade) = open(fixture);
        let initial = char_styles(&model, &jade);
        let mut sim = Simulation::with_materializer(model.clone(), jade, 3);
        for step in &steps {
            match *step {
                Step::Type { client, choice } => {
                    // Typing, deleting, splitting and joining only.
                    let choice = choice / 22 * 22 + choice % 10;
                    let view = sim.client(client).view().clone();
                    if let Some(op) = generate(&view, choice, &mut |n| sim.client_mut(client).reserve(n)) {
                        let _ = sim.local(client, op);
                    }
                }
                Step::Format { client, choice } => {
                    let view = sim.client(client).view().clone();
                    let paragraphs = view.paragraph_texts();
                    let p = (choice as usize >> 8) % paragraphs.len();
                    let q = (p + (choice as usize >> 16) % 3).min(paragraphs.len() - 1);
                    let so = (choice as usize >> 24) % (paragraphs[p].len() + 1);
                    let eo = (choice as usize >> 32) % (paragraphs[q].len() + 1);
                    let intent = IntentOp::Format {
                        start_paragraph: p,
                        start_offset: so,
                        end_paragraph: q,
                        end_offset: eo,
                        change: CHANGES[choice as usize % CHANGES.len()].clone(),
                    };
                    // Offsets off a character boundary or ranges ending
                    // before they start are simply not made.
                    if let Ok(ops) = resolve(&view, &intent, &mut |n| sim.client_mut(client).reserve(n)) {
                        for op in ops {
                            let _ = sim.local(client, op);
                        }
                    }
                }
                Step::Up { client } => {
                    sim.deliver_up(client);
                }
                Step::Down { client } => {
                    sim.deliver_down(client).unwrap();
                }
            }
        }
        sim.quiesce().unwrap();
        let server = sim.server();
        prop_assert!(server.faults().is_empty(), "materialization faults: {:?}", server.faults());

        // Replay the log, resolving every format op where it committed.
        let mut replay = model;
        let mut expected: BTreeMap<Id, Expected> = initial
            .iter()
            .map(|(id, s)| (*id, Expected {
                bold: Some(s.style.bold.unwrap_or(false)),
                italic: Some(s.style.italic.unwrap_or(false)),
            }))
            .collect();
        for committed in server.log() {
            if let DocxOp::Format { story, start, end, change } = &committed.op {
                for c in replay.range_chars(*story, *start, *end).unwrap_or_default() {
                    let e = expected.entry(c).or_insert(Expected { bold: None, italic: None });
                    match change {
                        FormatChange::Bold { value } => e.bold = Some(*value),
                        FormatChange::Italic { value } => e.italic = Some(*value),
                        _ => *e = Expected { bold: None, italic: None },
                    }
                }
            }
            let ctx = ApplyContext {
                origin: committed.origin,
                base_seq: committed.base_seq,
                seq: Some(committed.seq),
            };
            replay.apply(&committed.op, &ctx).expect("a committed op replays");
        }
        prop_assert_eq!(&replay, server.document());

        let actual = char_styles(server.document(), server.materializer());
        for (c, style) in &actual {
            // Characters typed after the last format that covered them (or
            // never covered) follow Word's typing rules, not the model's.
            let Some(e) = expected.get(c) else { continue };
            if let Some(bold) = e.bold {
                prop_assert_eq!(style.style.bold.unwrap_or(false), bold, "bold of {}", c);
            }
            if let Some(italic) = e.italic {
                prop_assert_eq!(style.style.italic.unwrap_or(false), italic, "italic of {}", c);
            }
        }
    }
}
