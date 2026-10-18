//! Random sequences of tool calls — valid, invalid and malformed — never
//! break the server, and every session it holds reads what a replica
//! outside MCP reads once both have caught up.

mod common;

use std::sync::OnceLock;

use common::{Agent, Hosted, DOCX, XLSX};
use jade_collab_mcp::{read_docx, read_xlsx};
use jade_docx_collab::DocxModel;
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
    Apply {
        session: usize,
        edits: Vec<Value>,
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

fn docx_edit() -> impl Strategy<Value = Value> {
    prop_oneof![
        (0..5usize, 0..8usize, text()).prop_map(|(p, o, t)| json!({ "type": "insert_text", "paragraph": p, "offset": o, "text": t })),
        (0..5usize, 0..8usize, any::<bool>(), 0..4usize).prop_map(|(p, o, forward, n)| json!({
            "type": "delete_text", "paragraph": p, "offset": o,
            "direction": if forward { "forward" } else { "backward" }, "count": n
        })),
        (0..5usize, 0..8usize, 0..3usize).prop_map(|(p, o, n)| json!({ "type": "insert_paragraph_break", "paragraph": p, "offset": o, "count": n })),
        (0..4usize, 0..6usize, 0..4usize, 0..6usize, text()).prop_map(|(sp, so, ep, eo, t)| json!({
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

fn edit() -> impl Strategy<Value = Value> {
    prop_oneof![
        4 => docx_edit(),
        4 => xlsx_edit(),
        1 => Just(json!({ "type": "explode" })),
        1 => Just(json!("insert_text")),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    let session = 0..4usize;
    prop_oneof![
        2 => any::<bool>().prop_map(|xlsx| Step::Join { xlsx }),
        1 => session.clone().prop_map(|session| Step::Leave { session }),
        2 => (session.clone(), 0..20u64, 1..20u64).prop_map(|(session, start, limit)| Step::Read { session, start, limit }),
        6 => (session.clone(), prop::collection::vec(edit(), 0..4)).prop_map(|(session, edits)| Step::Apply { session, edits }),
        1 => (session, 0..10u64).prop_map(|(session, after)| Step::Wait { session, after }),
        1 => prop::sample::select(vec![
            json!({ "name": "read_document", "arguments": { "session_id": "nope" } }),
            json!({ "name": "apply_edits", "arguments": { "session_id": "nope", "edits": [] } }),
            json!({ "name": "apply_edits", "arguments": { "edits": 3 } }),
            json!({ "name": "join_document", "arguments": { "url": "ws://127.0.0.1:1/documents/x.pptx/ws" } }),
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

/// A session the agent believes it holds.
struct Joined {
    id: String,
    name: &'static str,
}

async fn run(steps: Vec<Step>) {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
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
                if let Some(id) = pick(&sessions, session) {
                    let count = edits.len();
                    let applied = agent.apply(&id, Value::Array(edits)).await;
                    let results = applied["results"].as_array().unwrap();
                    assert_eq!(results.len(), count);
                    for result in results {
                        let status = result["status"].as_str().unwrap();
                        assert!(
                            ["applied", "rejected", "invalid"].contains(&status),
                            "{applied}"
                        );
                    }
                    assert_eq!(applied["pending"], json!(0), "{applied}");
                }
            }
            Step::Wait { session, after } => {
                if let Some(id) = pick(&sessions, session) {
                    let waited = agent
                        .tool(
                            "wait_for_changes",
                            json!({ "session_id": id, "after_seq": after, "timeout_ms": 20 }),
                        )
                        .await;
                    let seq = waited["seq"].as_u64().unwrap();
                    assert_eq!(waited["changed"], json!(seq > after), "{waited}");
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
            read_docx(
                &hosted.direct::<DocxModel>(DOCX).await.view(),
                0,
                usize::MAX,
            )
        } else {
            read_xlsx(
                &hosted.direct::<XlsxModel>(XLSX).await.view(),
                0,
                usize::MAX,
            )
        };
        expected["seq"] = json!(seq);
        expected["pending"] = json!(0);
        assert_eq!(read, expected);
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
