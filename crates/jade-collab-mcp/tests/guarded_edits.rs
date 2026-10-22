mod common;
use common::{fixture, statuses, Agent, Hosted, DOCX, XLSX};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn stale_snapshot_edits_of_every_format_fail_before_any_operation_is_submitted() {
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
        let before = reader.read_all(&session).await;
        let (remote, proposed) = match name {
            DOCX => (
                json!({"type":"insert_paragraph_break","paragraph":0,"offset":0}),
                json!({"type":"delete_text","paragraph":1,"offset":0,"direction":"forward","count":5}),
            ),
            XLSX => (
                json!({"type":"insert_row","sheet":"Operations","before":1}),
                json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"text","value":"guarded"}}),
            ),
            _ => (
                json!({"type":"duplicate_slide","slide":1}),
                json!({"type":"delete_slide","slide":2}),
            ),
        };
        assert_eq!(
            statuses(&editor.apply(&writer, json!([remote])).await),
            ["applied"]
        );
        let seq = hosted.server.seq(name).await.unwrap();
        reader
            .tool(
                "wait_for_changes",
                json!({"session_id":session,"after_seq":seq - 1}),
            )
            .await;
        let file = hosted.server.export(name).await.unwrap();
        let error = reader
            .tool_failure(
                "apply_edits",
                json!({"session_id":session,"snapshot":before["snapshot"],"edits":[proposed]}),
            )
            .await;
        assert!(error.contains("changed"), "{error}");
        assert_eq!(hosted.server.seq(name).await.unwrap(), seq);
        assert_eq!(hosted.server.export(name).await.unwrap(), file);
        assert_eq!(
            reader
                .tool(
                    "read_document",
                    json!({"session_id":session,"snapshot":before["snapshot"],"limit":100_000})
                )
                .await,
            before
        );

        let fresh = reader.read_all(&session).await;
        let applied = reader
            .tool(
                "apply_edits",
                json!({"session_id":session,"snapshot":fresh["snapshot"],"edits":[proposed]}),
            )
            .await;
        assert_eq!(statuses(&applied), ["applied"]);
        assert!(reader
            .tool_failure(
                "apply_edits",
                json!({"session_id":session,"snapshot":fresh["snapshot"],"edits":[]})
            )
            .await
            .contains("changed"));
        assert_eq!(
            reader
                .tool(
                    "read_document",
                    json!({"session_id":session,"snapshot":fresh["snapshot"],"limit":100_000})
                )
                .await,
            fresh
        );
        assert!(hosted.server.faults(name).await.unwrap().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_foreign_and_replaced_edit_guards_never_apply_an_edit() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let a = agent.join(&hosted.url(DOCX)).await;
    let b = agent.join(&hosted.url(DOCX)).await;
    let old = agent.read_all(&a).await;
    for (session, token) in [
        (b.clone(), old["snapshot"].clone()),
        (a.clone(), json!("missing")),
    ] {
        assert!(agent.tool_failure("apply_edits", json!({"session_id":session,"snapshot":token,"edits":[{"type":"insert_text","paragraph":0,"offset":0,"text":"wrong"}]})).await.contains("snapshot"));
    }
    agent.read_all(&a).await;
    assert!(agent
        .tool_failure(
            "apply_edits",
            json!({"session_id":a,"snapshot":old["snapshot"],"edits":[]})
        )
        .await
        .contains("snapshot"));
    assert_eq!(hosted.server.seq(DOCX).await.unwrap(), 0);
}
