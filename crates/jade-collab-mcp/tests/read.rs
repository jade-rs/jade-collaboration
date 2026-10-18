//! `read_document` on a docx view lists every paragraph an edit can address
//! — body, table cells, text boxes, footnotes and endnotes, in jade's order —
//! and says which story each belongs to and where its text boxes and note
//! references sit.

mod common;

use common::fixture;
use jade_collab_mcp::read_docx;
use jade_docx_collab::{DocxModel, JadeDocx};
use serde_json::json;

#[test]
fn read_document_names_note_and_text_box_paragraphs() {
    let jade = JadeDocx::load(&fixture(
        "public/fixtures/footnote-table-textbox-order.docx",
    ))
    .unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    let read = read_docx(&model, 0, 100);
    assert_eq!(read["paragraph_count"], json!(18));
    let paragraphs = read["paragraphs"].as_array().unwrap();
    let story = |index: usize| paragraphs[index]["story"].clone();
    assert_eq!(story(0), json!({ "kind": "body" }));
    assert_eq!(
        story(1),
        json!({ "kind": "footnote", "note": "9", "paragraph": 0 })
    );
    assert_eq!(story(2)["kind"], json!("table_cell"));
    assert_eq!(paragraphs[9]["text"], json!("Host after box"));
    assert_eq!(
        story(10),
        json!({ "kind": "text_box", "anchor_paragraph": 9, "text_box": 0, "paragraph": 0 })
    );
    assert_eq!(
        story(16),
        json!({ "kind": "endnote", "note": "5", "paragraph": 0 })
    );
    // Where the host paragraph's text box and note references are.
    assert_eq!(
        paragraphs[9]["anchors"],
        json!([
            { "offset": 4, "kind": "footnote", "note": "5" },
            { "offset": 4, "kind": "text_box", "text_box": 0 },
            { "offset": 14, "kind": "footnote", "note": "3" },
        ])
    );
    // "Cell C" places nothing.
    assert_eq!(paragraphs[6]["text"], json!("Cell C"));
    assert!(paragraphs[6].get("anchors").is_none());
}

/// Comments are listed with the id edits name them by, their thread, text,
/// author and state — the file's and the session's alike.
#[test]
fn read_document_lists_comments() {
    let jade = JadeDocx::load(&fixture("public/fixtures/comment.docx")).unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    let ids: Vec<String> = model.comments().iter().map(|c| c.id.to_string()).collect();
    let read = read_docx(&model, 0, 100);
    assert_eq!(
        read["comments"],
        json!([
            {
                "id": ids[0],
                "file_id": "0",
                "reply_to": null,
                "text": "Hello Comment",
                "author": "植木 智之",
                "resolved": false,
            },
            {
                "id": ids[1],
                "file_id": "1",
                "reply_to": ids[0],
                "text": "Reply",
                "author": "植木 智之",
                "resolved": false,
            },
        ])
    );
}
