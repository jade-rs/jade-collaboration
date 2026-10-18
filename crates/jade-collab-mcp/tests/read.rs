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

/// Tracked changes: the document's mode and every live revision, and on
/// each paragraph the byte spans each revision covers.
#[test]
fn read_document_shows_tracked_insertions_and_deletions() {
    let jade = JadeDocx::load(&fixture("public/fixtures/review.docx")).unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    let read = read_docx(&model, 0, 100);
    assert_eq!(read["tracking"], json!(false));
    let revisions = read["revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 2);
    let insertion = &revisions[0];
    assert_eq!(insertion["kind"], json!("insertion"));
    assert_eq!(insertion["author"], json!("植木 智之"));
    assert_eq!(insertion["date"], json!("2025-11-12T19:17:00Z"));
    assert_eq!(insertion["file_id"], json!("0"));
    assert_eq!(insertion["text"], json!("Hello "));
    assert_eq!(insertion["paragraphs"], json!([0]));
    let id = insertion["id"].as_str().unwrap().to_string();
    assert!(id.contains("@r0"), "{id}");
    assert_eq!(revisions[1]["kind"], json!("deletion"));
    assert_eq!(revisions[1]["text"], json!("World"));
    let deletion = revisions[1]["id"].as_str().unwrap().to_string();
    // On the paragraph: which bytes each revision covers.
    let paragraph = &read["paragraphs"][0];
    assert_eq!(paragraph["text"], json!("Hello World"));
    assert_eq!(
        paragraph["revisions"],
        json!([
            { "kind": "insertion", "revision": id, "start": 0, "end": 6 },
            { "kind": "deletion", "revision": deletion, "start": 6, "end": 11 },
        ])
    );
    assert!(read["paragraphs"][1].get("revisions").is_none());
}
