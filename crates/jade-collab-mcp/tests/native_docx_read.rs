mod common;
use common::{statuses, Agent, Hosted, DOCX, XLSX};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn native_formatting_is_read_with_the_same_committed_text_and_version_and_stays_pinned() {
    let hosted = Hosted::start().await;
    let (mut reader, mut writer) = (Agent::new(), Agent::new());
    let session = reader.join(&hosted.url(DOCX)).await;
    let editor = writer.join(&hosted.url(DOCX)).await;
    assert_eq!(statuses(&writer.apply(&editor, json!([
        {"type":"format","start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":5,"change":{"kind":"bold","value":false}},
        {"type":"format_paragraphs","start_paragraph":0,"end_paragraph":0,"change":{"kind":"alignment","alignment":"center"}}
    ])).await), ["applied";2]);
    let seq = hosted.server.seq(DOCX).await.unwrap();
    let inspected = reader
        .tool(
            "read_document",
            json!({"session_id":session,"include_base_formatting":true,"limit":1}),
        )
        .await;
    assert_eq!(inspected["view"], "authoritative");
    assert_eq!(inspected["seq"], seq);
    assert_eq!(inspected["pending"], 0);
    assert_eq!(inspected["paragraphs"][0]["text"], "Hello\u{c}");
    assert_eq!(
        inspected["paragraphs"][0]["paragraph_format"]["alignment"],
        "center"
    );
    assert_eq!(
        inspected["paragraphs"][0]["resolved_formatting"][0]["style"]["bold"],
        false
    );
    assert!(inspected["paragraphs"][0]["resolved_formatting"][0]["style"]["font_size"].is_number());
    assert_eq!(
        hosted.server.seq(DOCX).await.unwrap(),
        seq,
        "inspection is not logged"
    );
    assert_eq!(
        statuses(
            &writer
                .apply(
                    &editor,
                    json!([
                        {"type":"insert_text","paragraph":0,"offset":0,"text":"日🙂"}
                    ])
                )
                .await
        ),
        ["applied"]
    );
    let pinned = reader
        .tool(
            "read_document",
            json!({"session_id":session,"snapshot":inspected["snapshot"],"limit":1}),
        )
        .await;
    assert_eq!(pinned, inspected);
    let next = reader
        .tool(
            "read_document",
            json!({"session_id":session,"snapshot":inspected["snapshot"],"start":1,"limit":1}),
        )
        .await;
    assert_eq!(next["seq"], seq);
    assert_eq!(next["paragraphs"][0]["text"], "World");
    assert!(next["paragraphs"][0]["resolved_formatting"].is_array());
    assert!(reader
        .tool_failure(
            "apply_edits",
            json!({"session_id":session,"snapshot":inspected["snapshot"],"edits":[]})
        )
        .await
        .contains("changed"));
    let current = reader
        .tool(
            "read_document",
            json!({"session_id":session,"include_base_formatting":true}),
        )
        .await;
    assert!(current["paragraphs"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("日🙂"));
    assert_eq!(
        statuses(
            &reader
                .tool(
                    "apply_edits",
                    json!({"session_id":session,"snapshot":current["snapshot"],"edits":[
                        {"type":"insert_text","paragraph":1,"offset":0,"text":"guarded"}
                    ]})
                )
                .await
        ),
        ["applied"]
    );
    assert!(hosted.server.faults(DOCX).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn native_read_modes_are_explicit_and_failed_requests_preserve_the_retained_snapshot() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let docx = agent.join(&hosted.url(DOCX)).await;
    let xlsx = agent.join(&hosted.url(XLSX)).await;
    let plain = agent.read_all(&docx).await;
    assert!(agent
        .tool_failure(
            "read_document",
            json!({"session_id":docx,"snapshot":plain["snapshot"],"include_base_formatting":true})
        )
        .await
        .contains("formatting"));
    assert_eq!(
        agent
            .tool(
                "read_document",
                json!({"session_id":docx,"snapshot":plain["snapshot"],"limit":100_000})
            )
            .await,
        plain
    );
    assert!(agent
        .tool_failure(
            "read_document",
            json!({"session_id":xlsx,"include_base_formatting":true})
        )
        .await
        .contains("DOCX"));
    let rich = agent
        .tool(
            "read_document",
            json!({"session_id":docx,"include_base_formatting":true,"limit":100_000}),
        )
        .await;
    assert!(agent
        .tool_failure(
            "read_document",
            json!({"session_id":docx,"snapshot":rich["snapshot"],"include_base_formatting":false})
        )
        .await
        .contains("formatting"));
    assert_eq!(
        agent
            .tool(
                "read_document",
                json!({"session_id":docx,"snapshot":rich["snapshot"],"limit":100_000})
            )
            .await,
        rich
    );
}
