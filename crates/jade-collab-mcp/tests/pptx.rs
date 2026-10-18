//! An agent on a deck.

mod common;

use common::{fixture, Agent};
use jade_collab_mcp::read_pptx;
use jade_collab_server::{Client, Server};
use jade_pptx_collab::PptxModel;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_joins_reads_and_edits_a_deck() {
    let server = Server::new();
    server
        .open("deck.pptx", fixture("public/fixtures/sample-deck.pptx"))
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.clone().serve(listener));
    let url = format!("ws://{addr}/documents/deck.pptx/ws");

    let mut agent = Agent::new();
    let joined = agent.tool("join_document", json!({ "url": url })).await;
    assert_eq!(joined["format"], json!("pptx"));
    let session = joined["session_id"].as_str().unwrap().to_string();
    let read = agent.read_all(&session).await;
    assert_eq!(read["format"], json!("pptx"));
    let slides = read["slides"].as_array().unwrap().len();
    assert!(slides >= 1);

    let applied = agent
        .apply(
            &session,
            json!([
                { "type": "insert_text", "slide": 1, "target": "notes", "paragraph": 0, "offset": 0, "text": "agent note" },
                { "type": "duplicate_slide", "slide": 1 },
                { "type": "set_cell", "sheet": "S", "reference": "A1", "value": { "kind": "blank" } },
            ]),
        )
        .await;
    let statuses: Vec<&str> = applied["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["applied", "applied", "invalid"], "{applied}");

    let read = agent.read_all(&session).await;
    assert_eq!(read["slides"].as_array().unwrap().len(), slides + 1);
    assert!(read["slides"][0]["notes"]
        .as_str()
        .unwrap()
        .starts_with("agent note"));

    let direct: Client<PptxModel> = Client::connect(&url, None).await.unwrap();
    let mut expected = read_pptx(&direct.view(), 0, usize::MAX);
    expected["seq"] = read["seq"].clone();
    expected["pending"] = json!(0);
    assert_eq!(read, expected);
    assert!(server.faults("deck.pptx").await.unwrap().is_empty());
}
