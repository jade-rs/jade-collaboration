//! Random sequences of tool calls — valid, invalid and malformed — never
//! break the server, and every session it holds reads what a replica
//! outside MCP reads once both have caught up. Each `apply_edits` is also
//! replayed on that replica's model, one edit at a time against the state
//! the previous ones left: the statuses and the resulting document must
//! match.

mod common;

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::{statuses, Agent, Hosted, DOCX, XLSX};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_mcp::{read_docx, read_xlsx};
use jade_collab_server::Client;
use jade_collab_sync::{ApplyContext, Document};
use jade_docx_collab::{Anchor, DocxModel, DocxOp, StoryId};
use jade_xlsx_collab::XlsxModel;
use proptest::prelude::*;
use serde_json::{json, Value};
use tokio::runtime::Runtime;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    })
}

#[derive(Clone, Debug)]
enum Step {
    Join {
        xlsx: bool,
    },
    Leave {
        session: usize,
    },
    Read {
        session: usize,
        start: u64,
        limit: u64,
    },
    /// Each edit comes in a docx and an xlsx form; the session's format
    /// picks one, except where `foreign` sends the other format's.
    Apply {
        session: usize,
        edits: Vec<(Value, Value, bool)>,
    },
    Wait {
        session: usize,
        after: u64,
    },
    /// Arguments, a tool name or a message the server must refuse.
    Bad(Value),
    Raw(String),
}

fn text() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["", "a", "日本", "x y", "\u{1F600}", "\n"]).prop_map(String::from)
}

/// Most paragraphs are short or empty, so offsets favour the start.
fn offset() -> impl Strategy<Value = usize> {
    prop_oneof![2 => Just(0usize), 1 => 0..7usize]
}

fn docx_edit() -> impl Strategy<Value = Value> {
    let paragraph = || 0..8usize;
    prop_oneof![
        (paragraph(), offset(), text()).prop_map(|(p, o, t)| json!({ "type": "insert_text", "paragraph": p, "offset": o, "text": t })),
        (paragraph(), offset(), any::<bool>(), 0..4usize).prop_map(|(p, o, forward, n)| json!({
            "type": "delete_text", "paragraph": p, "offset": o,
            "direction": if forward { "forward" } else { "backward" }, "count": n
        })),
        (paragraph(), offset(), 0..3usize).prop_map(|(p, o, n)| json!({ "type": "insert_paragraph_break", "paragraph": p, "offset": o, "count": n })),
        (paragraph(), offset(), paragraph(), offset(), text()).prop_map(|(sp, so, ep, eo, t)| json!({
            "type": "replace_range", "start_paragraph": sp, "start_offset": so,
            "end_paragraph": ep, "end_offset": eo, "text": t
        })),
    ]
}

fn xlsx_edit() -> impl Strategy<Value = Value> {
    let reference = (
        prop::sample::select(vec!["A", "B", "C", "E", "F", "H", "ZZ"]),
        0..12u32,
    )
        .prop_map(|(c, r)| format!("{c}{r}"));
    let value = prop_oneof![
        (-5i32..100).prop_map(|n| json!({ "kind": "number", "value": n })),
        text().prop_map(|t| json!({ "kind": "text", "value": t })),
        any::<bool>().prop_map(|b| json!({ "kind": "bool", "value": b })),
        Just(json!({ "kind": "formula", "value": "=B2+B3" })),
        Just(json!({ "kind": "blank" })),
    ];
    let sheet = prop::sample::select(vec!["Operations", "Operations", "Missing"]);
    prop_oneof![
        3 => (sheet.clone(), reference, value).prop_map(|(s, r, v)| json!({ "type": "set_cell", "sheet": s, "reference": r, "value": v })),
        1 => (sheet.clone(), 0..12u32).prop_map(|(s, r)| json!({ "type": "insert_row", "sheet": s, "before": r })),
        1 => (sheet.clone(), 0..12u32).prop_map(|(s, r)| json!({ "type": "delete_row", "sheet": s, "row": r })),
        1 => (sheet.clone(), prop::sample::select(vec!["A", "D", "G", "K"])).prop_map(|(s, c)| json!({ "type": "insert_column", "sheet": s, "before": c })),
        1 => (sheet, prop::sample::select(vec!["A", "D", "G", "K"])).prop_map(|(s, c)| json!({ "type": "delete_column", "sheet": s, "column": c })),
    ]
}

fn edit() -> impl Strategy<Value = (Value, Value, bool)> {
    let junk = prop::sample::select(vec![json!({ "type": "explode" }), json!("insert_text")]);
    (
        prop_oneof![9 => docx_edit(), 1 => junk.clone()],
        prop_oneof![9 => xlsx_edit(), 1 => junk],
        prop::bool::weighted(0.1),
    )
}

fn step() -> impl Strategy<Value = Step> {
    let session = 0..4usize;
    prop_oneof![
        2 => any::<bool>().prop_map(|xlsx| Step::Join { xlsx }),
        1 => session.clone().prop_map(|session| Step::Leave { session }),
        2 => (session.clone(), 0..20u64, 1..20u64).prop_map(|(session, start, limit)| Step::Read { session, start, limit }),
        6 => (session.clone(), prop::collection::vec(edit(), 0..5)).prop_map(|(session, edits)| Step::Apply { session, edits }),
        1 => (session, 0..10u64).prop_map(|(session, after)| Step::Wait { session, after }),
        1 => prop::sample::select(vec![
            json!({ "name": "read_document", "arguments": { "session_id": "nope" } }),
            json!({ "name": "apply_edits", "arguments": { "session_id": "nope", "edits": [] } }),
            json!({ "name": "apply_edits", "arguments": { "edits": 3 } }),
            json!({ "name": "join_document", "arguments": { "url": "ws://127.0.0.1:1/documents/x.txt/ws" } }),
            json!({ "name": "wait_for_changes", "arguments": { "session_id": "nope", "after_seq": "0" } }),
            json!({ "name": "no_such_tool", "arguments": {} }),
            json!({ "arguments": {} }),
            json!(null),
        ]).prop_map(Step::Bad),
        1 => prop::sample::select(vec![
            "{".to_string(),
            "[]".to_string(),
            r#"{"jsonrpc":"2.0","id":"x","method":"nope"}"#.to_string(),
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#.to_string(),
        ]).prop_map(Step::Raw),
    ]
}

/// Applies `edits` to `model` the way the tool promises to: each resolved
/// against the state the previous ones left, an edit whose operation is
/// refused stopping there. Returns the statuses.
fn replay<D: Document>(
    model: &mut D,
    edits: &[Value],
    seq: u64,
    resolve: impl Fn(&D, Value, &mut dyn FnMut(u64) -> Id) -> Result<Vec<D::Op>, String>,
) -> Vec<String> {
    // Fresh ids are newer than anything in the document, as a replica's are.
    let mut next = model.max_lamport() + 1;
    let mut committed = seq;
    edits
        .iter()
        .map(|edit| {
            let mut ids = |n: u64| {
                let id = Id::new(next, ReplicaId(u64::MAX));
                next += n;
                id
            };
            let ops = match resolve(model, edit.clone(), &mut ids) {
                Ok(ops) => ops,
                Err(_) => return "invalid".to_string(),
            };
            for op in ops {
                committed += 1;
                let ctx = ApplyContext {
                    origin: ReplicaId(u64::MAX),
                    base_seq: seq,
                    seq: Some(committed),
                };
                if model.apply(&op, &ctx).is_err() {
                    return "rejected".to_string();
                }
            }
            "applied".to_string()
        })
        .collect()
}

fn replay_docx(model: &mut DocxModel, edits: &[Value], seq: u64) -> Vec<String> {
    replay(model, edits, seq, |view, edit, ids| {
        let intent = serde_json::from_value(edit).map_err(|e| e.to_string())?;
        jade_docx_collab::intent::resolve(view, &intent, ids).map_err(|e| e.to_string())
    })
}

fn replay_xlsx(model: &mut XlsxModel, edits: &[Value], seq: u64) -> Vec<String> {
    replay(model, edits, seq, |view, edit, ids| {
        let intent = serde_json::from_value(edit).map_err(|e| e.to_string())?;
        jade_xlsx_collab::intent::resolve(view, &intent, ids)
            .map(|op| vec![op])
            .map_err(|e| e.to_string())
    })
}

/// A read without the replica's position, to compare with a rendering.
fn content(mut read: Value) -> Value {
    let object = read.as_object_mut().unwrap();
    object.remove("seq");
    object.remove("pending");
    object.remove("snapshot");
    read
}

/// A session the agent believes it holds.
struct Joined {
    id: String,
    name: &'static str,
}

async fn run(steps: Vec<Step>) {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let docx: Client<DocxModel> = hosted.direct(DOCX).await;
    // A table after the first paragraph gives the edits cells to land in
    // and paragraphs they cannot join.
    let end = docx.view().paragraphs()[0].chars.last().copied();
    let mark = docx.reserve(1);
    docx.submit(DocxOp::SplitParagraph {
        story: StoryId::Body,
        parent: end,
        id: mark,
    })
    .unwrap();
    let first = docx.reserve(5);
    docx.submit(DocxOp::InsertTable {
        anchor: Anchor::After(mark),
        id: first,
        rows: vec![first.offset(1), first.offset(2)],
        columns: vec![first.offset(3), first.offset(4)],
    })
    .unwrap();
    docx.settle().await.unwrap();
    let xlsx: Client<XlsxModel> = hosted.direct(XLSX).await;
    let mut sessions: Vec<Joined> = Vec::new();
    let pick = |sessions: &[Joined], i: usize| -> Option<String> {
        (!sessions.is_empty()).then(|| sessions[i % sessions.len()].id.clone())
    };
    for step in steps {
        match step {
            Step::Join { xlsx } => {
                let name = if xlsx { XLSX } else { DOCX };
                let id = agent.join(&hosted.url(name)).await;
                sessions.push(Joined { id, name });
            }
            Step::Leave { session } => {
                if !sessions.is_empty() {
                    let gone = sessions.remove(session % sessions.len());
                    agent
                        .tool("leave_document", json!({ "session_id": gone.id }))
                        .await;
                    let (code, _) = agent
                        .rpc_error("read_document", json!({ "session_id": gone.id }))
                        .await;
                    assert_eq!(code, -32602);
                }
            }
            Step::Read {
                session,
                start,
                limit,
            } => {
                if let Some(id) = pick(&sessions, session) {
                    let read = agent
                        .tool(
                            "read_document",
                            json!({ "session_id": id, "start": start, "limit": limit }),
                        )
                        .await;
                    assert_eq!(read["start"], json!(start));
                }
            }
            Step::Apply { session, edits } => {
                if let Some(i) = (!sessions.is_empty()).then(|| session % sessions.len()) {
                    let Joined { id, name } = &sessions[i];
                    let edits: Vec<Value> = edits
                        .into_iter()
                        .map(|(d, x, foreign)| if (*name == DOCX) != foreign { d } else { x })
                        .collect();
                    // Bring the session and the independent replica to the
                    // server's position; they must read the same.
                    let seq = hosted.server.seq(name).await.unwrap();
                    agent.catch_up(id, seq).await;
                    let before = content(agent.read_all(id).await);
                    let (expected, after) = if *name == DOCX {
                        hosted.catch_up(DOCX, &docx).await;
                        let mut model = docx.view();
                        assert_eq!(before, read_docx(&model, 0, usize::MAX));
                        let expected = replay_docx(&mut model, &edits, seq);
                        (expected, read_docx(&model, 0, usize::MAX))
                    } else {
                        hosted.catch_up(XLSX, &xlsx).await;
                        let mut model = xlsx.view();
                        assert_eq!(before, read_xlsx(&model, 0, usize::MAX));
                        let expected = replay_xlsx(&mut model, &edits, seq);
                        (expected, read_xlsx(&model, 0, usize::MAX))
                    };
                    let applied = agent.apply(id, json!(edits)).await;
                    assert_eq!(statuses(&applied), expected, "{applied}");
                    assert_eq!(applied["pending"], json!(0), "{applied}");
                    if *name == DOCX {
                        assert_eq!(content(agent.read_all(id).await), after);
                    } else {
                        // The server may follow an edit with what jade did on
                        // its own (Excel naming a new table column): the
                        // session must read what the independent replica
                        // reads once both have the whole log.
                        let seq = hosted.server.seq(name).await.unwrap();
                        agent.catch_up(id, seq).await;
                        hosted.catch_up(XLSX, &xlsx).await;
                        assert_eq!(
                            content(agent.read_all(id).await),
                            read_xlsx(&xlsx.view(), 0, usize::MAX)
                        );
                        let _ = after;
                    }
                }
            }
            Step::Wait { session, after } => {
                if let Some(id) = pick(&sessions, session) {
                    let started = Instant::now();
                    let waited = agent
                        .tool(
                            "wait_for_changes",
                            json!({ "session_id": id, "after_seq": after, "timeout_ms": 20 }),
                        )
                        .await;
                    let seq = waited["seq"].as_u64().unwrap();
                    assert_eq!(waited["changed"], json!(seq > after), "{waited}");
                    if seq <= after {
                        assert!(
                            started.elapsed() >= Duration::from_millis(20),
                            "returned early"
                        );
                    }
                }
            }
            Step::Bad(params) => {
                let response = agent.request("tools/call", params).await;
                assert!(response["error"]["code"].is_i64(), "{response}");
            }
            Step::Raw(line) => {
                if let Some(response) = agent.send(&line).await {
                    assert!(response["error"]["code"].is_i64(), "{response}");
                }
            }
        }
    }

    // Every session left standing reads what an independent replica reads.
    for joined in &sessions {
        let seq = hosted.server.seq(joined.name).await.unwrap();
        agent.catch_up(&joined.id, seq).await;
        let read = agent.read_all(&joined.id).await;
        let mut expected = if joined.name == DOCX {
            hosted.catch_up(DOCX, &docx).await;
            read_docx(&docx.view(), 0, usize::MAX)
        } else {
            hosted.catch_up(XLSX, &xlsx).await;
            read_xlsx(&xlsx.view(), 0, usize::MAX)
        };
        expected["seq"] = json!(seq);
        expected["pending"] = json!(0);
        assert_eq!(common::without_snapshot(read), expected);
    }
    assert!(hosted.server.faults(DOCX).await.unwrap().is_empty());
    assert!(hosted.server.faults(XLSX).await.unwrap().is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn random_tool_calls_keep_sessions_consistent(
        steps in prop::collection::vec(step(), 1..25),
    ) {
        runtime().block_on(run(steps));
    }
}
