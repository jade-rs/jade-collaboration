//! Compare paragraph-walk resolution with an independent raw-prefix scan.
//! The ignored variant sends these same real replica states to Lean as well.
use super::*;
use crate::{
    intent::{resolve, IntentOp},
    Atom, DocxOp, JadeDocx, NoteKind,
};
use jade_collab_core::id::ReplicaId;
use jade_collab_sync::{sim::Simulation, ApplyContext, Document, NoMaterializer, Sequencer};
use proptest::prelude::*;
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn boundaries(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect()
}

// This deliberately does not enumerate paragraph ends or repeatedly ask the
// RGA for visible character positions, as the implementation does.
fn prefix_reference(model: &DocxModel, cursor: &TextCursor) -> Option<(usize, usize)> {
    let story = model.story(cursor.story)?;
    let nodes: Vec<_> = story.flow.iter().collect();
    let find = |id| nodes.iter().position(|n| n.id == id);
    let left = resolve_ref(cursor.left, &find)?;
    let right = resolve_ref(cursor.right, &find)?;
    if matches!((left, right), (Some(l), Some(r)) if l >= r) {
        return None;
    }
    let cut = right.or_else(|| left.map(|l| l + 1)).unwrap_or(nodes.len());
    let (mut paragraph, mut offset) = (0, 0);
    for node in &nodes[..cut] {
        if node.deleted {
            continue;
        }
        match node.value {
            Atom::Char(c) => offset += c.len_utf8(),
            Atom::Mark => {
                paragraph += 1;
                offset = 0;
            }
            Atom::Anchor(_) => {}
        }
    }
    model
        .paragraphs()
        .iter()
        .enumerate()
        .filter(|(_, p)| p.story == cursor.story)
        .nth(paragraph)
        .map(|(surface, _)| (surface, offset))
}

fn resolve_ref<T>(
    reference: Option<T>,
    find: &impl Fn(T) -> Option<usize>,
) -> Option<Option<usize>> {
    match reference {
        None => Some(None),
        Some(id) => find(id).map(Some),
    }
}

fn apply(model: &mut DocxModel, op: DocxOp) {
    model
        .apply(
            &op,
            &ApplyContext {
                origin: ReplicaId(1),
                base_seq: 0,
                seq: Some(1),
            },
        )
        .unwrap();
}

#[test]
fn tombstoned_paragraph_marks_and_both_neighbours_keep_the_cursor_boundary() {
    let mut model = DocxModel::from_paragraphs(&["a😀", "日本", "z"]);
    let p = model.paragraphs();
    let saved = model.text_cursor(1, "日".len()).unwrap();
    apply(
        &mut model,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: p[1].chars.clone(),
        },
    );
    apply(
        &mut model,
        DocxOp::JoinParagraph {
            story: StoryId::Body,
            mark: p[0].mark.unwrap(),
        },
    );
    assert_eq!(model.resolve_text_cursor(&saved), Some((0, "a😀".len())));
    assert_eq!(
        model.resolve_text_cursor(&saved),
        prefix_reference(&model, &saved)
    );
}

#[test]
fn foreign_future_and_reversed_endpoints_are_hidden_without_changing_the_model() {
    let model = DocxModel::from_paragraphs(&["😀ab"]);
    let p = &model.paragraphs()[0];
    let before = serde_json::to_value(&model).unwrap();
    let unknown = Id::new(u64::MAX, ReplicaId(u64::MAX));
    for (left, right) in [
        (Some(unknown), Some(p.chars[1])),
        (Some(p.chars[0]), Some(unknown)),
        (Some(p.chars[2]), Some(p.chars[0])),
        (Some(p.chars[1]), Some(p.chars[1])),
    ] {
        let cursor = TextCursor {
            story: StoryId::Body,
            left,
            right,
        };
        assert_eq!(model.resolve_text_cursor(&cursor), None);
    }
    for offset in [1, 2, 3, usize::MAX] {
        assert!(model.text_cursor(0, offset).is_none());
    }
    assert!(model.text_cursor(usize::MAX, 0).is_none());
    assert_eq!(serde_json::to_value(&model).unwrap(), before);
}

#[test]
fn every_unicode_boundary_roundtrips_in_cells_notes_and_text_boxes() {
    let model = mixed_document();
    let before = serde_json::to_value(&model).unwrap();
    for (index, p) in model.paragraphs().iter().enumerate() {
        for offset in boundaries(&p.text) {
            let cursor = model.text_cursor(index, offset).unwrap();
            assert_eq!(
                model.resolve_text_cursor(&cursor),
                Some((index, offset)),
                "{p:?}"
            );
            if p.story != StoryId::Body && (cursor.left.is_some() || cursor.right.is_some()) {
                let mut foreign = cursor.clone();
                foreign.story = StoryId::Body;
                assert_eq!(model.resolve_text_cursor(&foreign), None);
            }
        }
    }
    assert_eq!(serde_json::to_value(&model).unwrap(), before);
}

#[test]
fn right_neighbour_affinity_and_story_tail_fallback_are_explicit() {
    let mut model = DocxModel::from_paragraphs(&["ab"]);
    let middle = model.text_cursor(0, 1).unwrap();
    let tail = model.text_cursor(0, 2).unwrap();
    let p = &model.paragraphs()[0];
    let a = p.chars[0];
    let b = p.chars[1];
    let first = Id::new(model.max_lamport() + 1, ReplicaId(1));
    apply(
        &mut model,
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: Some(a),
            first,
            text: "😀".into(),
        },
    );
    assert_eq!(model.paragraph_texts(), vec!["a😀b"]);
    assert_eq!(model.resolve_text_cursor(&middle), Some((0, 5)));
    let first = Id::new(model.max_lamport() + 1, ReplicaId(1));
    apply(
        &mut model,
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: Some(b),
            first,
            text: "日".into(),
        },
    );
    assert_eq!(model.paragraph_texts(), vec!["a😀b日"]);
    assert_eq!(model.resolve_text_cursor(&tail), Some((0, 6)));
}

#[test]
fn optimistic_future_endpoints_wait_for_delivery_then_reappear() {
    let mut sim = Simulation::new(DocxModel::from_paragraphs(&["a😀"]), 3);
    let first = sim.client_mut(0).reserve(1);
    sim.local(
        0,
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first,
            text: "日".into(),
        },
    )
    .unwrap();
    let cursor = sim.client(0).view().text_cursor(0, "日".len()).unwrap();
    assert_eq!(sim.client(1).view().resolve_text_cursor(&cursor), None);
    assert!(sim.deliver_up(0));
    assert_eq!(sim.client(1).view().resolve_text_cursor(&cursor), None);
    assert!(sim.deliver_down(1).unwrap());
    assert_eq!(
        sim.client(1).view().resolve_text_cursor(&cursor),
        Some((0, "日".len()))
    );
    sim.quiesce().unwrap();
    let restored: DocxModel =
        serde_json::from_value(serde_json::to_value(sim.server().document()).unwrap()).unwrap();
    assert_eq!(restored.resolve_text_cursor(&cursor), Some((0, "日".len())));
}

fn mixed_document() -> DocxModel {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../jade-core/public/fixtures/footnote-table-textbox-order.docx"
    ))
    .unwrap();
    let jade = JadeDocx::load(&bytes).unwrap();
    DocxModel::from_word_document(jade.word()).unwrap()
}

#[derive(Clone, Debug)]
enum Step {
    Structure(usize, u8),
    Capture(usize, usize, usize),
    Edit(usize, u8, usize, usize, usize, String),
    Up(usize),
    Down(usize),
    Reconnect,
    Restart,
}
fn history() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            3 => (0..3usize, any::<usize>(), any::<usize>()).prop_map(|(c,p,o)| Step::Capture(c,p,o)),
            5 => (0..3usize,0..4u8,any::<usize>(),any::<usize>(),any::<usize>(),"[a-z日本😀\u{301}]{0,3}").prop_map(|(c,k,p,a,b,t)| Step::Edit(c,k,p,a,b,t)),
            2 => (0..3usize).prop_map(Step::Up),
            2 => (0..3usize).prop_map(Step::Down),
            2 => (0..3usize,0..6u8).prop_map(|(c,k)| Step::Structure(c,k)),
            1 => Just(Step::Reconnect), 1 => Just(Step::Restart),
        ],
        1..75,
    )
}

fn id_word(id: Id) -> String {
    ((u128::from(id.lamport) << 64) | u128::from(id.replica.0)).to_string()
}
fn endpoint(id: Option<Id>) -> String {
    id.map(id_word).unwrap_or_else(|| "-".into())
}

#[derive(Default)]
struct Queries {
    input: String,
    expected: Vec<String>,
}
impl Queries {
    fn check(
        &mut self,
        model: &DocxModel,
        saved: &[TextCursor],
        ranges: &[(TextCursor, TextCursor)],
        lean: bool,
    ) {
        let before = serde_json::to_value(model).unwrap();
        for (anchor, focus) in ranges {
            if let (Some(a), Some(b)) = (
                model.resolve_text_cursor(anchor),
                model.resolve_text_cursor(focus),
            ) {
                assert!(
                    a <= b,
                    "a selected range reversed after delivery: {anchor:?} {focus:?} -> {a:?} {b:?}"
                );
            }
        }
        let paragraphs = model.paragraphs();
        for cursor in saved {
            let actual = model.resolve_text_cursor(cursor);
            assert_eq!(actual, prefix_reference(model, cursor), "cursor {cursor:?}");
            if let Some((p, offset)) = actual {
                assert_eq!(paragraphs[p].story, cursor.story);
                assert!(paragraphs[p].text.is_char_boundary(offset));
            }
            if lean {
                let visible = paragraphs.iter().any(|p| p.story == cursor.story);
                self.input.push_str(if visible { "s 1" } else { "s 0" });
                if let Some(story) = model.story(cursor.story) {
                    for node in story.flow.iter() {
                        let (kind, width) = match node.value {
                            Atom::Char(c) => ("c", c.len_utf8()),
                            Atom::Mark => ("m", 0),
                            Atom::Anchor(_) => ("a", 0),
                        };
                        self.input.push_str(&format!(
                            " {}:{kind}:{width}:{}",
                            id_word(node.id),
                            u8::from(!node.deleted)
                        ));
                    }
                }
                self.input.push_str(&format!(
                    "\nq {} {}\n",
                    endpoint(cursor.left),
                    endpoint(cursor.right)
                ));
                self.expected.push(match actual {
                    None => "none".into(),
                    Some((surface, offset)) => format!(
                        "{} {offset}",
                        paragraphs[..surface]
                            .iter()
                            .filter(|p| p.story == cursor.story)
                            .count()
                    ),
                });
            }
        }
        assert_eq!(
            serde_json::to_value(model).unwrap(),
            before,
            "presence must not consume IDs or change durable state"
        );
    }
    fn compare_lean(self) {
        let path = std::env::var("JADE_LEAN_CURSOR_ORACLE").expect("set JADE_LEAN_CURSOR_ORACLE");
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // Drain stdout while writing large histories: neither pipe may fill
        // while the other side waits for it to be consumed.
        let mut stdin = child.stdin.take().unwrap();
        let input = self.input;
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap().unwrap();
        assert!(
            output.status.success(),
            "cursor oracle failed: {}",
            output.status
        );
        let actual = String::from_utf8(output.stdout).unwrap();
        assert_eq!(actual.lines().collect::<Vec<_>>(), self.expected);
    }
}

fn run(steps: Vec<Step>, mixed: bool, lean: bool) {
    let mut initial = if mixed {
        mixed_document()
    } else {
        DocxModel::from_paragraphs(&["a😀日", "", "e\u{301}本"])
    };
    // Exercise exact identities beyond JavaScript's safe integer range too.
    let large = Id::new(initial.max_lamport() + 1, ReplicaId(u64::MAX - 1));
    apply(
        &mut initial,
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: large,
            text: "🦀".into(),
        },
    );
    let mut sim = Simulation::new(initial, 3);
    let mut saved = Vec::new();
    let mut ranges = Vec::new();
    // Include empty paragraphs and independent stories before any random action.
    for (p, paragraph) in sim.server().document().paragraphs().iter().enumerate() {
        saved.push(
            sim.server()
                .document()
                .text_cursor(p, paragraph.text.len())
                .unwrap(),
        );
    }
    for (p, paragraph) in sim.server().document().paragraphs().iter().enumerate() {
        ranges.push((
            sim.server().document().text_cursor(p, 0).unwrap(),
            sim.server()
                .document()
                .text_cursor(p, paragraph.text.len())
                .unwrap(),
        ));
    }
    let body = &sim.server().document().paragraphs()[0];
    saved.push(sim.server().document().text_cursor(0, 0).unwrap());
    saved.push(TextCursor {
        story: StoryId::Body,
        left: body.chars.last().copied(),
        right: body.chars.first().copied(),
    });
    saved.push(TextCursor {
        story: StoryId::Body,
        left: Some(Id::new(u64::MAX, ReplicaId(u64::MAX))),
        right: body.chars.first().copied(),
    });
    let mut queries = Queries::default();
    for step in steps {
        match step {
            Step::Structure(c, k) => {
                let model = sim.client(c).view().clone();
                let op = match k {
                    0 | 1 => model.tables().first().and_then(|table| {
                        let intent = if k == 0 {
                            IntentOp::MergeTableCells {
                                table: 0,
                                top_row: 0,
                                left_column: 0,
                                bottom_row: table.rows.len() - 1,
                                right_column: table.columns.len() - 1,
                            }
                        } else {
                            IntentOp::UnmergeTableCells {
                                table: 0,
                                row: 0,
                                column: 0,
                            }
                        };
                        resolve(&model, &intent, &mut |n| sim.client_mut(c).reserve(n))
                            .ok()
                            .and_then(|mut ops| ops.pop())
                    }),
                    2 => model
                        .tables()
                        .first()
                        .filter(|t| t.rows.len() > 1)
                        .map(|t| DocxOp::Table {
                            table: t.id,
                            op: jade_collab_core::grid::GridOp::DeleteRow { id: t.rows[0] },
                        }),
                    3 => model
                        .tables()
                        .first()
                        .map(|t| DocxOp::DeleteTable { table: t.id }),
                    4 => model
                        .text_boxes
                        .keys()
                        .next()
                        .map(|id| DocxOp::DeleteTextBox { text_box: *id }),
                    _ => model
                        .footnotes
                        .iter()
                        .find(|(_, n)| !n.deleted)
                        .map(|(id, _)| DocxOp::DeleteNote {
                            kind: NoteKind::Footnote,
                            note: *id,
                        }),
                };
                if let Some(op) = op {
                    let _ = sim.local(c, op);
                }
            }
            Step::Capture(c, p, o) => {
                let model = sim.client(c).view();
                let paragraphs = model.paragraphs();
                let p = p % paragraphs.len();
                let offsets = boundaries(&paragraphs[p].text);
                let anchor = model.text_cursor(p, offsets[o % offsets.len()]).unwrap();
                let focus = model.text_cursor(p, paragraphs[p].text.len()).unwrap();
                ranges.push((anchor.clone(), focus.clone()));
                if ranges.len() > 24 {
                    ranges.remove(0);
                }
                saved.push(anchor);
                saved.push(focus);
                while saved.len() > 24 {
                    saved.remove(0);
                }
            }
            Step::Edit(c, k, p, a, b, text) => {
                let model = sim.client(c).view().clone();
                let paragraphs = model.paragraphs();
                let p = p % paragraphs.len();
                let offsets = boundaries(&paragraphs[p].text);
                let a = offsets[a % offsets.len()];
                let b = offsets[b % offsets.len()];
                let intent = match k {
                    0 => IntentOp::InsertText {
                        paragraph: p,
                        offset: a,
                        text,
                    },
                    1 => IntentOp::ReplaceRange {
                        start_paragraph: p,
                        start_offset: a.min(b),
                        end_paragraph: p,
                        end_offset: a.max(b),
                        text,
                    },
                    2 => IntentOp::InsertParagraphBreak {
                        paragraph: p,
                        offset: a,
                        count: 1,
                    },
                    _ => {
                        let next = paragraphs
                            .iter()
                            .enumerate()
                            .skip(p + 1)
                            .find(|(_, q)| q.story == paragraphs[p].story)
                            .map(|(i, _)| i)
                            .unwrap_or(p);
                        IntentOp::ReplaceRange {
                            start_paragraph: p,
                            start_offset: paragraphs[p].text.len(),
                            end_paragraph: next,
                            end_offset: 0,
                            text: String::new(),
                        }
                    }
                };
                if let Ok(ops) = resolve(&model, &intent, &mut |n| sim.client_mut(c).reserve(n)) {
                    for op in ops {
                        let _ = sim.local(c, op);
                    }
                }
            }
            Step::Up(c) => {
                sim.deliver_up(c);
            }
            Step::Down(c) => {
                sim.deliver_down(c).unwrap();
            }
            Step::Reconnect => sim.reconnect().unwrap(),
            Step::Restart => {
                let checkpoint = sim.checkpoint();
                let checkpoint =
                    serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
                sim.restart(Sequencer::restore(checkpoint, NoMaterializer))
                    .unwrap();
            }
        }
        queries.check(sim.server().document(), &saved, &ranges, lean);
        for c in 0..3 {
            queries.check(sim.client(c).view(), &saved, &ranges, lean);
        }
    }
    sim.quiesce().unwrap();
    queries.check(sim.server().document(), &saved, &ranges, lean);
    for c in 0..3 {
        queries.check(sim.client(c).view(), &saved, &ranges, lean);
        assert_eq!(sim.client(c).view(), sim.server().document());
        for cursor in &saved {
            assert_eq!(
                sim.client(c).view().resolve_text_cursor(cursor),
                sim.server().document().resolve_text_cursor(cursor)
            );
        }
    }
    if lean {
        queries.compare_lean();
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn stateful_cursors_follow_unicode_structure_delivery_and_restart(steps in history(), mixed in any::<bool>()) {
        run(steps, mixed, false);
    }
    #[test]
    #[ignore = "requires JADE_LEAN_CURSOR_ORACLE; see lean/README.md"]
    fn stateful_cursors_match_lean(steps in history(), mixed in any::<bool>()) {
        run(steps, mixed, true);
    }
}
