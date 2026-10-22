mod common;

use common::{fixture, statuses, Agent, Hosted, DOCX, XLSX};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn pinned_reads_of_every_format_keep_the_same_content_and_metadata_while_others_edit() {
    let hosted = Hosted::start().await;
    hosted
        .server
        .open("deck.pptx", fixture("public/fixtures/sample-deck.pptx"))
        .unwrap();
    for name in [DOCX, XLSX, "deck.pptx"] {
        let mut reader = Agent::new();
        let mut editor = Agent::new();
        let session = reader.join(&hosted.url(name)).await;
        let writer = editor.join(&hosted.url(name)).await;
        let first = reader
            .tool("read_document", json!({"session_id":session, "limit":1}))
            .await;
        let snapshot = first["snapshot"]
            .as_str()
            .expect("a page carries its snapshot token")
            .to_string();
        let frozen = reader
            .tool(
                "read_document",
                json!({"session_id":session, "snapshot":snapshot, "limit":100_000}),
            )
            .await;
        let edits = match name {
            DOCX => {
                json!([{ "type":"insert_paragraph_break", "paragraph":0, "offset":0 }, { "type":"insert_text", "paragraph":0, "offset":0, "text":"changed" }])
            }
            XLSX => {
                json!([{ "type":"insert_row", "sheet":"Operations", "before":1 }, { "type":"set_format", "sheet":"Operations", "range":"B50", "change":{"bold":true} }])
            }
            _ => {
                json!([{ "type":"duplicate_slide", "slide":1 }, { "type":"insert_text", "slide":1, "target":"notes", "paragraph":0, "offset":0, "text":"changed" }])
            }
        };
        assert_eq!(
            statuses(&editor.apply(&writer, edits).await),
            ["applied"; 2]
        );
        let seq = hosted.server.seq(name).await.unwrap();
        let waited = reader
            .tool(
                "wait_for_changes",
                json!({"session_id":session, "after_seq":seq - 1}),
            )
            .await;
        assert!(waited["seq"].as_u64().unwrap() >= seq);
        let after = reader
            .tool(
                "read_document",
                json!({"session_id":session, "snapshot":snapshot, "limit":100_000}),
            )
            .await;
        assert_eq!(after, frozen, "immutable snapshot of {name}");
        assert_eq!(
            reader
                .tool(
                    "read_document",
                    json!({"session_id":session, "snapshot":snapshot, "limit":1})
                )
                .await,
            first
        );
        let live = reader.read_all(&session).await;
        assert_ne!(live["snapshot"], frozen["snapshot"]);
        assert_eq!(live["seq"], json!(seq));
        assert_ne!(live, frozen);
        assert!(reader
            .tool_failure(
                "read_document",
                json!({"session_id":session, "snapshot":snapshot})
            )
            .await
            .contains("snapshot"));
        assert!(hosted.server.faults(name).await.unwrap().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_tokens_are_scoped_to_the_session_replaced_explicitly_and_removed_on_leave() {
    let hosted = Hosted::start().await;
    let mut reader = Agent::new();
    let a = reader.join(&hosted.url(DOCX)).await;
    let b = reader.join(&hosted.url(XLSX)).await;
    let first = reader.read_all(&a).await;
    let token = first["snapshot"].as_str().unwrap().to_string();
    assert!(reader
        .tool_failure("read_document", json!({"session_id":b, "snapshot":token}))
        .await
        .contains("snapshot"));
    assert_eq!(
        reader
            .tool(
                "read_document",
                json!({"session_id":a, "snapshot":token, "limit":100_000})
            )
            .await,
        first
    );
    let replacement = reader.read_all(&a).await;
    assert_ne!(replacement["snapshot"], first["snapshot"]);
    assert!(reader
        .tool_failure("read_document", json!({"session_id":a, "snapshot":token}))
        .await
        .contains("snapshot"));
    reader.tool("leave_document", json!({"session_id":a})).await;
    let (code, _) = reader
        .rpc_error(
            "read_document",
            json!({"session_id":a, "snapshot":replacement["snapshot"]}),
        )
        .await;
    assert_eq!(code, -32602);
    // A failed token lookup never replaces another session's retained snapshot.
    let current = reader.read_all(&b).await;
    assert!(reader
        .tool_failure(
            "read_document",
            json!({"session_id":b, "snapshot":"unknown"})
        )
        .await
        .contains("snapshot"));
    assert_eq!(
        reader
            .tool(
                "read_document",
                json!({"session_id":b, "snapshot":current["snapshot"], "limit":100_000})
            )
            .await,
        current
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_from_another_mcp_instance_never_selects_a_new_instances_first_snapshot() {
    let hosted = Hosted::start().await;
    let mut old = Agent::new();
    let mut new = Agent::new();
    let old_session = old.join(&hosted.url(DOCX)).await;
    let new_session = new.join(&hosted.url(DOCX)).await;
    assert_ne!(
        old_session, new_session,
        "a stale session id must not select another instance's document"
    );
    assert_eq!(
        new.rpc_error("read_document", json!({"session_id":old_session}))
            .await
            .0,
        -32602
    );
    assert_eq!(
        new.rpc_error("apply_edits", json!({"session_id":old_session,"edits":[]}))
            .await
            .0,
        -32602
    );
    let previous = old.read_all(&old_session).await;
    let current = new.read_all(&new_session).await;
    assert_ne!(
        previous["snapshot"], current["snapshot"],
        "new MCP instances must not reuse old read tokens"
    );
    assert!(new
        .tool_failure(
            "read_document",
            json!({"session_id":new_session,"snapshot":previous["snapshot"]})
        )
        .await
        .contains("snapshot"));
    assert_eq!(
        new.tool(
            "read_document",
            json!({"session_id":new_session,"snapshot":current["snapshot"],"limit":100_000})
        )
        .await,
        current
    );
}
