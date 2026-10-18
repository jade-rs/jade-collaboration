//! Text edits and formatting interleaved across replicas. Character
//! formatting is a convergent mark: every replica applies it at once, it
//! commutes with every concurrent edit, and the server's file ends up
//! showing, on every character, the formatting the model gives it —
//! whatever order the messages travelled in. A clear is a mark plus a
//! sequenced clear of the file's own formatting, which lands on exactly the
//! characters in its range at its log position.

mod common;

use std::collections::BTreeMap;

use common::{assert_formatting_agrees, char_styles, formatted_char_styles, generate, open, shows};
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

fn changes() -> [FormatChange; 7] {
    [
        FormatChange::Bold { value: true },
        FormatChange::Bold { value: false },
        FormatChange::Italic { value: true },
        FormatChange::Italic { value: false },
        FormatChange::FontSize { half_points: 30 },
        FormatChange::Highlight {
            color: "green".into(),
        },
        FormatChange::ClearFormatting,
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn concurrent_formatting_converges_and_the_file_shows_the_model(
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
                    let changes = changes();
                    let intent = IntentOp::Format {
                        start_paragraph: p,
                        start_offset: so,
                        end_paragraph: q,
                        end_offset: eo,
                        change: changes[choice as usize % changes.len()].clone(),
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
            for client in 0..3 {
                prop_assert_eq!(sim.client(client).view(), &sim.client(client).rebuilt_view());
            }
        }
        sim.quiesce().unwrap();
        let server = sim.server();
        prop_assert!(server.faults().is_empty(), "materialization faults: {:?}", server.faults());
        for client in 0..3 {
            prop_assert_eq!(sim.client(client).view(), server.document());
            prop_assert_eq!(
                sim.client(client).view().character_formats(),
                server.document().character_formats()
            );
        }
        // The file shows the model's formatting.
        assert_formatting_agrees(server.document(), server.materializer(), "after the session");

        // Sequenced formatting lands on exactly the characters in its range
        // at its log position (a paragraph change on their whole
        // paragraphs): replaying the log, a character of the file that no
        // sequenced format reached and no mark covers keeps its bold.
        let mut replay = model;
        let mut cleared: BTreeMap<Id, ()> = BTreeMap::new();
        for committed in server.log() {
            if let DocxOp::Format { story, start, end, change } = &committed.op {
                let chars = replay.range_chars(*story, *start, *end).unwrap_or_default();
                if change.is_paragraph_level() {
                    for p in replay.paragraphs() {
                        let touched = p.chars.iter().any(|c| chars.contains(c))
                            || p.mark.is_some_and(|m| m == *start || m == *end);
                        if touched {
                            for c in p.chars {
                                cleared.insert(c, ());
                            }
                        }
                    }
                }
                for c in chars {
                    cleared.insert(c, ());
                }
            }
            // A join gives the second paragraph the first one's style.
            if let DocxOp::JoinParagraph { story, mark } = &committed.op {
                let paragraphs = replay.paragraphs();
                if let Some(at) = paragraphs.iter().position(|p| p.story == *story && p.mark == Some(*mark)) {
                    for p in &paragraphs[at..(at + 2).min(paragraphs.len())] {
                        for c in &p.chars {
                            cleared.insert(*c, ());
                        }
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
        let formats = server.document().character_formats();
        let actual = formatted_char_styles(server.document(), server.materializer());
        for (c, before) in &initial {
            let Some(after) = actual.get(c) else { continue };
            if cleared.contains_key(c) || formats.contains_key(c) {
                continue;
            }
            prop_assert!(
                shows(&FormatChange::Bold { value: before.style.bold.unwrap_or(false) }, after),
                "bold of {} changed without a mark or a clear", c
            );
        }
    }
}
