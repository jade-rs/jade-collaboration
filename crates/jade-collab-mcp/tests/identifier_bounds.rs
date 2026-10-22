mod common;
use common::{statuses, Agent, Hosted, DOCX};
use jade_collab_core::id::Id;
use jade_docx_collab::{DocxModel, DocxOp, StoryId};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn an_exhausted_clock_returns_an_invalid_edit_and_the_agent_can_keep_reading_and_deleting() {
    let hosted = Hosted::start().await;
    let writer = hosted.direct::<DocxModel>(DOCX).await;
    let original = writer.view().paragraph_texts()[0].clone();
    writer
        .submit(DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: Id::new(u64::MAX, writer.id()),
            text: "X".into(),
        })
        .unwrap();
    writer.settle().await.unwrap();
    let mut agent = Agent::new();
    let session = agent.join(&hosted.url(DOCX)).await;
    let before = agent.read_all(&session).await;
    let applied = agent.apply(&session, json!([{ "type": "insert_text", "paragraph": 0, "offset": 0, "text": "cannot allocate" }])).await;
    assert_eq!(statuses(&applied), ["invalid"]);
    assert!(
        applied.to_string().contains("identifier space exhausted"),
        "{applied}"
    );
    assert_eq!(
        agent
            .tool(
                "read_document",
                json!({"session_id":session,"snapshot":before["snapshot"],"limit":100_000})
            )
            .await,
        before,
        "a refused edit does not alter the retained snapshot"
    );
    assert_eq!(
        common::without_snapshot(agent.read_all(&session).await),
        common::without_snapshot(before),
        "the live session remains readable"
    );
    let deleted = agent.apply(&session, json!([{ "type": "delete_text", "paragraph": 0, "offset": 0, "direction": "forward", "count": 1 }])).await;
    assert_eq!(statuses(&deleted), ["applied"]);
    assert_eq!(
        agent.read_all(&session).await["paragraphs"][0]["text"],
        original
    );
}
