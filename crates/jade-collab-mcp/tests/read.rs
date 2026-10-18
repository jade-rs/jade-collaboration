//! `read_document` on a docx view lists every paragraph an edit can address
//! — body, table cells, text boxes, footnotes and endnotes, in jade's order —
//! and says which story each belongs to and where its text boxes and note
//! references sit.

mod common;

use common::fixture;
use jade_collab_mcp::{read_docx, read_xlsx};
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

/// An xlsx read lists, per sheet, the formats written through the
/// collaboration — borders included — by A1 reference.
#[test]
fn read_document_lists_formats_and_borders_written_in_the_session() {
    use jade_collab_core::id::{Id, ReplicaId};
    use jade_collab_sync::{ApplyContext, Document};
    use jade_xlsx_collab::intent::{resolve, XlsxIntent};
    use jade_xlsx_collab::XlsxModel;

    let mut model = XlsxModel::blank(&[("S", 3, 3)]);
    let mut next = 100;
    for intent in [
        json!({ "type": "set_borders", "sheet": "S", "range": "A1:B1",
                "borders": { "outline": { "style": "thin", "color": "FF0000" } } }),
        json!({ "type": "set_format", "sheet": "S", "range": "C3", "change": { "bold": true } }),
    ] {
        let intent: XlsxIntent = serde_json::from_value(intent).unwrap();
        let op = resolve(&model, &intent, &mut |n| {
            next += n;
            Id::new(next, ReplicaId(2))
        })
        .unwrap();
        let ctx = ApplyContext {
            origin: ReplicaId(2),
            base_seq: 0,
            seq: None,
        };
        model.apply(&op, &ctx).unwrap();
    }
    let read = read_xlsx(&model, 0, 100);
    let line = json!({ "style": "thin", "color": "FF0000" });
    assert_eq!(
        read["sheets"][0]["formats"],
        json!({
            "A1": { "border_left": line, "border_top": line, "border_bottom": line },
            "B1": { "border_right": line, "border_top": line, "border_bottom": line },
            "C3": { "bold": true },
        })
    );
}
