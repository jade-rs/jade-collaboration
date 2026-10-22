mod common;

use common::{fixture, statuses, Hosted};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn inherited_pptx_styles_are_captured_with_content_and_remain_pinned_after_edits() {
    let hosted = Hosted::start().await;
    hosted
        .server
        .open("deck.pptx", fixture("public/fixtures/inherited-deck.pptx"))
        .unwrap();
    let (mut reader, mut writer) = (common::Agent::new(), common::Agent::new());
    let session = reader.join(&hosted.url("deck.pptx")).await;
    let editor = writer.join(&hosted.url("deck.pptx")).await;
    let initial = reader
        .tool(
            "read_document",
            json!({"session_id":session,"include_base_formatting":true,"limit":1}),
        )
        .await;
    assert_eq!(initial["view"], "authoritative");
    assert_eq!(initial["pending"], 0);
    assert_eq!(initial["seq"], 0);
    let shape = initial["slides"][0]["shapes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["shape"] == 2)
        .unwrap();
    let p = &shape["paragraphs"][0];
    assert_eq!(p["text"], shape["text"]);
    assert_eq!(p["resolved_formatting"][0]["style"]["size_pt"], 30.0);
    assert_eq!(p["resolved_formatting"][0]["style"]["color"], "#A020F0");
    assert_eq!(p["resolved_formatting"][0]["style"]["font"], "serif");
    assert!(p["paragraph_format"]["alignment"].is_string());
    assert_eq!(statuses(&writer.apply(&editor, json!([
        {"type":"format_text","slide":1,"shape":2,"start_paragraph":0,"start_offset":0,"end_paragraph":0,"end_offset":1,"format":{"attribute":"bold","value":false}},
        {"type":"align_paragraphs","slide":1,"shape":2,"start_paragraph":0,"end_paragraph":0,"alignment":"center"},
        {"type":"insert_text","slide":1,"target":{"shape":2},"paragraph":0,"offset":0,"text":"日🙂"}
    ])).await), ["applied";3]);
    let pinned = reader
        .tool(
            "read_document",
            json!({"session_id":session,"snapshot":initial["snapshot"],"limit":1}),
        )
        .await;
    assert_eq!(pinned, initial);
    assert!(reader.tool_failure("read_document", json!({"session_id":session,"snapshot":initial["snapshot"],"include_base_formatting":false})).await.contains("formatting"));
    let seq = hosted.server.seq("deck.pptx").await.unwrap();
    reader
        .tool(
            "wait_for_changes",
            json!({"session_id":session,"after_seq":seq-1}),
        )
        .await;
    assert!(reader
        .tool_failure(
            "apply_edits",
            json!({"session_id":session,"snapshot":initial["snapshot"],"edits":[]})
        )
        .await
        .contains("changed"));
    let current = reader
        .tool(
            "read_document",
            json!({"session_id":session,"include_base_formatting":true,"limit":1}),
        )
        .await;
    assert_eq!(current["seq"], seq);
    assert_eq!(
        hosted.server.seq("deck.pptx").await.unwrap(),
        seq,
        "inspection never appends to the journal"
    );
    let shape = current["slides"][0]["shapes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["shape"] == 2)
        .unwrap();
    assert!(shape["text"].as_str().unwrap().starts_with("日🙂"));
    assert_eq!(
        shape["paragraphs"][0]["paragraph_format"]["alignment"],
        "center"
    );
    assert!(shape["paragraphs"][0]["resolved_formatting"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["style"]["bold"] == false));
    let second = reader
        .tool(
            "read_document",
            json!({"session_id":session,"snapshot":current["snapshot"],"start":1,"limit":1}),
        )
        .await;
    assert_eq!(second["slides"][0]["slide"], 2);
    assert_eq!(second["seq"], seq);
    assert_eq!(statuses(&reader.tool("apply_edits", json!({"session_id":session,"snapshot":current["snapshot"],"edits":[{"type":"set_slide_hidden","slide":1,"hidden":true}]})).await), ["applied"]);
    assert!(hosted.server.faults("deck.pptx").await.unwrap().is_empty());
}
