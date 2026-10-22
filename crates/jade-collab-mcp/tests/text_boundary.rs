mod common;
use common::{fixture, statuses, Agent, Hosted, DOCX, XLSX};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn unrepresentable_text_returns_terminal_receipts_and_sessions_stay_readable_and_editable() {
    let host = Hosted::start().await;
    host.server
        .open("deck.pptx", fixture("public/fixtures/sample-deck.pptx"))
        .unwrap();
    for (name, bad, good) in [
        (
            DOCX,
            json!([{ "type":"insert_text","paragraph":0,"offset":0,"text":"bad\0text" }]),
            json!([{ "type":"insert_text","paragraph":0,"offset":0,"text":"日本🙂" }]),
        ),
        (
            XLSX,
            json!([{ "type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"formula","value":"="} },{ "type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"formula","value":"A1&\"\0\""} }]),
            json!([{ "type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"text","value":"ok\0literal_x0000_"} }]),
        ),
        (
            "deck.pptx",
            json!([{ "type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"bad\0text" }]),
            json!([{ "type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"日本🙂\nsecond" }]),
        ),
    ] {
        let mut agent = Agent::new();
        let session = agent.join(&host.url(name)).await;
        let pinned = agent.read_all(&session).await;
        let file = host.server.export(name).await.unwrap();
        let seq = host.server.seq(name).await.unwrap();
        let result = agent.apply(&session, bad).await;
        assert!(
            statuses(&result)
                .iter()
                .all(|s| matches!(s.as_str(), "invalid" | "rejected")),
            "{name}: {result}"
        );
        assert_eq!(result["complete"], true);
        assert_eq!(result["unresolved"], 0);
        assert_eq!(host.server.seq(name).await.unwrap(), seq);
        assert_eq!(host.server.export(name).await.unwrap(), file);
        assert_eq!(
            agent
                .tool(
                    "read_document",
                    json!({"session_id":session,"snapshot":pinned["snapshot"],"limit":100_000})
                )
                .await,
            pinned
        );
        let receipt = agent
            .tool(
                "get_edit_status",
                json!({"session_id":session,"receipt":result["receipt"]}),
            )
            .await;
        assert_eq!(statuses(&receipt), statuses(&result));
        assert_eq!(receipt["complete"], true);
        let accepted = agent
            .tool(
                "apply_edits",
                json!({"session_id":session,"snapshot":pinned["snapshot"],"edits":good}),
            )
            .await;
        assert_eq!(statuses(&accepted), ["applied"]);
        assert_eq!(accepted["pending"], 0);
        assert!(host.server.faults(name).await.unwrap().is_empty());
    }
}
