use super::*;
use proptest::prelude::*;
use std::sync::OnceLock;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
}

async fn host() -> (
    jade_collab_server::Server,
    String,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let server = jade_collab_server::Server::new();
    server
        .open(
            "hello.docx",
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx"),
            )
            .unwrap(),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/hello.docx/ws",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(server.clone().serve(listener));
    (server, url, task)
}

fn ok(result: Result<Value, CallError>) -> Value {
    match result {
        Ok(value) => value,
        Err(CallError::Failed(e) | CallError::Invalid(e)) => panic!("{e}"),
    }
}

#[test]
fn exhausted_snapshot_tokens_do_not_wrap_or_invalidate_an_existing_snapshot() {
    runtime().block_on(async {
        let (_server, url, task) = host().await;
        let mut mcp = McpServer::new();
        let joined = ok(mcp.join(json!({"url":url})).await);
        let session = joined["session_id"].as_str().unwrap();
        let first = ok(mcp
            .read(json!({"session_id":session, "limit":100_000}))
            .await);
        mcp.next_snapshot = u64::MAX;
        assert!(matches!(
            mcp.read(json!({"session_id":session})).await,
            Err(CallError::Failed(_))
        ));
        assert_eq!(mcp.next_snapshot, u64::MAX);
        assert_eq!(mcp.snapshots.len(), 1);
        mcp.next_session = u64::MAX;
        assert!(matches!(
            mcp.join(json!({"url":url})).await,
            Err(CallError::Failed(_))
        ));
        assert_eq!(mcp.sessions.len(), 1);
        assert_eq!(
            ok(mcp
                .read(json!({"session_id":session,"snapshot":first["snapshot"],"limit":100_000}))
                .await),
            first
        );
        ok(mcp.leave(json!({"session_id":session})).await);
        assert!(mcp.snapshots.is_empty());
        task.abort();
        let _ = task.await;
    });
}

#[test]
fn exhausted_receipt_tokens_refuse_before_submission_and_preserve_a_read_guard() {
    runtime().block_on(async {
        let (_server,url,task) = host().await;
        let mut mcp = McpServer::new();
        let joined = ok(mcp.join(json!({"url":url})).await);
        let session = joined["session_id"].as_str().unwrap();
        let read = ok(mcp.read(json!({"session_id":session})).await);
        let edit = json!({"session_id":session,"snapshot":read["snapshot"],"edits":[{"type":"insert_text","paragraph":1,"offset":0,"text":"r"}]});
        mcp.next_receipt=u64::MAX;
        assert!(matches!(mcp.apply(edit.clone()).await,Err(CallError::Failed(_))));
        assert_eq!(mcp.next_receipt,u64::MAX);
        assert_eq!(mcp.session(session).unwrap_or_else(|_|panic!("session missing")).snapshot().read(0,100_000)["paragraphs"][1]["text"],"World");
        mcp.next_receipt=0;
        let applied = ok(mcp.apply(edit).await);
        assert_eq!(applied["results"][0]["status"],"applied");
        ok(mcp.leave(json!({"session_id":session})).await);
        assert!(mcp.receipts.is_empty());
        task.abort(); let _ = task.await;
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn retained_snapshots_are_immutable_scoped_and_bounded_through_session_histories(
        steps in prop::collection::vec((0u8..11, any::<usize>()), 1..45),
        pptx in any::<bool>(),
    ) {
        runtime().block_on(async {
            let (server, mut url, task) = host().await;
            let (name,base_text,edit) = if pptx {
                let bytes = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/public/fixtures/inherited-deck.pptx")).unwrap();
                let model = jade_pptx_collab::PptxModel::from_pptx(&bytes).unwrap();
                let text = model.slides()[0].shapes[0].text.clone().unwrap();
                server.open("deck.pptx",bytes).unwrap();
                url = url.replace("hello.docx/ws","deck.pptx/ws");
                ("deck.pptx",text,json!({"type":"insert_text","slide":1,"target":{"shape":2},"paragraph":0,"offset":0,"text":"日本🙂"}))
            } else {("hello.docx","World".to_owned(),json!({"type":"insert_text","paragraph":1,"offset":0,"text":"日本🙂"}))};
            let mut mcp = McpServer::new();
            let mut sessions: Vec<String> = Vec::new();
            // Independent cache model: one frozen JSON rendering per session.
            let mut retained: BTreeMap<String, Value> = BTreeMap::new();
            let mut captured_epochs: BTreeMap<String, u64> = BTreeMap::new();
            let mut epoch = 0u64;
            let mut history: Vec<String> = Vec::new();
            for (kind, choice) in steps {
                if kind == 6 {
                    // Simulate the MCP process restarting while the document
                    // server keeps running. Old tokens remain in history.
                    mcp = McpServer::new();
                    sessions.clear();
                    retained.clear();
                    captured_epochs.clear();
                } else if kind == 0 && sessions.len() < 3 {
                    let joined = ok(mcp.join(json!({"url":url})).await);
                    sessions.push(joined["session_id"].as_str().unwrap().into());
                } else if !sessions.is_empty() {
                    let session = sessions[choice % sessions.len()].clone();
                    match kind {
                        1 | 10 => {
                            let snapshot = ok(mcp.read(json!({"session_id":session,"limit":100_000,"include_base_formatting":kind == 10})).await);
                            assert_eq!(snapshot.get("view").is_some(), kind == 10);
                            history.push(snapshot["snapshot"].as_str().unwrap().into());
                            assert_eq!(snapshot["seq"], json!(epoch));
                            captured_epochs.insert(session.clone(), epoch);
                            retained.insert(session, snapshot);
                        }
                        2 | 3 if !history.is_empty() => {
                            let token = &history[(choice / 3) % history.len()];
                            let got = mcp.read(json!({"session_id":session,"snapshot":token,"limit":100_000})).await;
                            if let Some(expected) = retained.get(&session).filter(|v| v["snapshot"] == json!(token)) {
                                assert_eq!(ok(got), *expected);
                            } else { assert!(matches!(got, Err(CallError::Failed(_)))); }
                        }
                        4 => {
                            let applied = ok(mcp.apply(json!({"session_id":session,"edits":[edit]})).await);
                            assert_eq!(applied["results"][0]["status"], json!("applied"));
                            epoch += 1;
                        }
                        5 => {
                            ok(mcp.leave(json!({"session_id":session})).await);
                            sessions.retain(|s| s != &session);
                            retained.remove(&session);
                            captured_epochs.remove(&session);
                        }
                        7..=9 => {
                            let token = if kind == 7 && !history.is_empty() { history[choice % history.len()].clone() }
                                else { retained.get(&session).and_then(|s| s["snapshot"].as_str()).unwrap_or("missing").to_string() };
                            let permitted = retained.get(&session).is_some_and(|v| v["snapshot"] == json!(token))
                                && captured_epochs.get(&session) == Some(&epoch);
                            let edits = if kind == 9 { json!([]) } else { json!([edit]) };
                            let result = mcp.apply(json!({"session_id":session,"snapshot":token,"edits":edits})).await;
                            if permitted {
                                let applied = ok(result);
                                if kind != 9 { assert_eq!(applied["results"][0]["status"], json!("applied")); epoch += 1; }
                            } else { assert!(matches!(result, Err(CallError::Failed(_))), "a stale/foreign guard submitted an edit"); }
                        }
                        _ => {}
                    }
                }
                assert_eq!(mcp.snapshots.len(), retained.len());
                assert_eq!(server.seq(name).await.unwrap(), epoch);
                for session in &sessions {
                    if epoch > 0 {
                        let waited = ok(mcp.wait(json!({"session_id":session,"after_seq":epoch - 1})).await);
                        assert_eq!(waited["changed"], json!(true));
                    }
                    let live = mcp.session(session).unwrap_or_else(|_| panic!("joined session missing")).snapshot().read(0, 100_000);
                    let text = if pptx {&live["slides"][0]["shapes"][0]["text"]} else {&live["paragraphs"][1]["text"]};
                    assert_eq!(*text, json!(format!("{}{base_text}", "日本🙂".repeat(epoch as usize))));
                }
                assert!(mcp.snapshots.len() <= mcp.sessions.len());
                assert!(mcp.receipts.len() <= mcp.sessions.len());
                for (session, frozen) in &retained {
                    let (token, snapshot) = &mcp.snapshots[session];
                    let mut actual = snapshot.read(0, 100_000);
                    actual["snapshot"] = json!(token);
                    assert_eq!(actual, *frozen, "cache changed after an edit, leave or invalid token");
                }
            }
            for session in sessions { ok(mcp.leave(json!({"session_id":session})).await); }
            assert!(mcp.snapshots.is_empty());
            assert!(server.faults(name).await.unwrap().is_empty());
            task.abort(); let _ = task.await;
        });
    }
}
