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
    assert_eq!(common::without_snapshot(read), expected);
    assert!(server.faults("deck.pptx").await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn advertised_pptx_edits_are_readable_pinned_and_written_to_the_actual_deck() {
    use common::{statuses, without_snapshot, Hosted};
    let hosted = Hosted::start().await;
    hosted
        .server
        .open("deck.pptx", fixture("public/fixtures/sample-deck.pptx"))
        .unwrap();
    let (mut editor, mut reader) = (Agent::new(), Agent::new());
    let session = editor.join(&hosted.url("deck.pptx")).await;
    let observer = reader.join(&hosted.url("deck.pptx")).await;
    let tools = editor.request("tools/list", json!({})).await;
    let apply = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "apply_edits")
        .unwrap();
    let picture = apply["inputSchema"]["properties"]["edits"]["items"]["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["properties"]["type"]["const"] == "insert_picture")
        .unwrap();
    let png = picture["properties"]["image"]["examples"][0].clone();
    assert!(png.is_string());
    assert_eq!(statuses(&editor.apply(&session, json!([
        {"type":"insert_text_box","slide":1,"x":-1000,"y":-2000,"width":914400,"height":457200,"text":"a日🙂b"},
        {"type":"set_slide_hidden","slide":1,"hidden":true},
        {"type":"insert_picture","slide":1,"x":-100,"y":200,"width":19050,"image":png}
    ])).await), ["applied";3]);
    let current = editor.read_all(&session).await;
    let shapes = current["slides"][0]["shapes"].as_array().unwrap();
    let text_box = shapes.iter().find(|s| s["text"] == "a日🙂b").unwrap()["shape"].clone();
    let picture = shapes
        .iter()
        .find(|s| s["kind"] == "picture" && s["geometry"]["x"] == -100)
        .unwrap();
    assert_eq!(picture["aspect_locked"], true);
    let picture_id = picture["shape"].clone();
    assert_eq!(statuses(&editor.apply(&session, json!([
        {"type":"format_text","slide":1,"shape":text_box,"start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":8,"format":{"attribute":"bold","value":false}},
        {"type":"format_text","slide":1,"shape":text_box,"start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":8,"format":{"attribute":"east_asian_font","typeface":"Yu Gothic"}},
        {"type":"align_paragraphs","slide":1,"shape":text_box,"start_paragraph":0,"end_paragraph":0,"alignment":"center"},
        {"type":"set_geometry","slide":1,"shape":picture_id,"width":38100}
    ])).await), ["applied";4]);
    let seq = hosted.server.seq("deck.pptx").await.unwrap();
    reader
        .tool(
            "wait_for_changes",
            json!({"session_id":observer,"after_seq":seq-1}),
        )
        .await;
    let pinned = reader.read_all(&observer).await;
    let own = editor.read_all(&session).await;
    assert_eq!(without_snapshot(own), without_snapshot(pinned.clone()));
    assert_eq!(pinned["slides"][0]["hidden"], true);
    let shapes = pinned["slides"][0]["shapes"].as_array().unwrap();
    let shape = shapes.iter().find(|s| s["shape"] == text_box).unwrap();
    assert_eq!(
        shape["formatting"],
        json!([
            {"paragraph":0,"start":0,"end":8,"format":{"attribute":"bold","value":false}},
            {"paragraph":0,"start":0,"end":8,"format":{"attribute":"east_asian_font","typeface":"Yu Gothic"}}
        ])
    );
    let picture = shapes.iter().find(|s| s["shape"] == picture_id).unwrap();
    assert_eq!(picture["geometry"]["width"], 38100);
    assert_eq!(picture["geometry"]["height"], 38100);
    let deck =
        jade_pptx_reader::read_deck(&hosted.server.export("deck.pptx").await.unwrap()).unwrap();
    assert!(deck.slides[0].content.hidden);
    let text = deck.slides[0]
        .content
        .shapes
        .iter()
        .find_map(|s| match s {
            jade_pptx_reader::ShapeItem::Shape(s) => s.text.as_ref().filter(|t| {
                t.paragraphs
                    .iter()
                    .flat_map(|p| &p.runs)
                    .map(|r| r.text.as_str())
                    .collect::<String>()
                    == "a日🙂b"
            }),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        text.paragraphs[0].align,
        Some(jade_pptx_reader::TextAlign::Center)
    );
    assert_eq!(text.paragraphs[0].runs[0].bold_stated, Some(false));
    assert_eq!(
        text.paragraphs[0].runs[0].east_asian_font.as_deref(),
        Some("Yu Gothic")
    );
    assert_eq!(
        statuses(
            &editor
                .apply(
                    &session,
                    json!([
                        {"type":"set_slide_hidden","slide":1,"hidden":false},
                        {"type":"delete_shape","slide":1,"shape":picture_id}
                    ])
                )
                .await
        ),
        ["applied"; 2]
    );
    assert_eq!(
        reader
            .tool(
                "read_document",
                json!({"session_id":observer,"snapshot":pinned["snapshot"],"limit":100_000})
            )
            .await,
        pinned
    );
    assert!(hosted.server.faults("deck.pptx").await.unwrap().is_empty());
}
