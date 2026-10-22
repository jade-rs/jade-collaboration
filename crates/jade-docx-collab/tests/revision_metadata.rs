//! Lexical XML revision attributes become logical metadata exactly once.
use jade_docx_collab::{DocxModel, JadeDocx};
use jade_opc::OpcPackage;
use std::path::Path;

fn document(body: &str) -> Vec<u8> {
    let source =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx"))
            .unwrap();
    let mut package = OpcPackage::open(&source).unwrap();
    package.replace("word/document.xml",format!("<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}</w:body></w:document>").into_bytes());
    package.write().unwrap()
}
#[test]
fn revision_attribute_aliases_group_by_their_logical_values_and_decode_nested_layers() {
    let bytes=document("<w:p><w:ins w:id=\"9\" w:author=\"A&amp;B\" w:date=\"D&amp;E\"><w:r><w:t>a</w:t></w:r></w:ins><w:ins w:id=\"&#57;\" w:author=\"A&#38;B\" w:date=\"D&#x26;E\"><w:r><w:t>b</w:t></w:r></w:ins><w:ins w:id=\"10\" w:author=\"Literal &amp;amp; &amp;#9;\"><w:del w:id=\"11\" w:author=\"Outer &quot;name&quot;\"><w:r><w:delText>c</w:delText></w:r></w:del></w:ins></w:p>");
    let jade = JadeDocx::load(&bytes).unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    let revisions = model.revisions();
    assert_eq!(revisions.len(), 3);
    let merged = revisions.iter().find(|r| r.text == "ab").unwrap();
    assert_eq!(merged.revision.author.as_deref(), Some("A&B"));
    assert_eq!(merged.revision.date.as_deref(), Some("D&E"));
    assert_eq!(merged.revision.file_id.as_deref(), Some("9"));
    assert!(revisions
        .iter()
        .any(|r| r.revision.author.as_deref() == Some("Literal &amp; &#9;")));
    assert!(revisions
        .iter()
        .any(|r| r.revision.author.as_deref() == Some("Outer \"name\"")));
}
#[test]
fn literal_whitespace_normalizes_on_import_and_character_references_stay_lossless_across_reopens() {
    let mut bytes=document("<w:p><w:ins w:id=\"1\" w:author=\" A\tB\r\nC\rD\nE \" w:date=\"D&#9;E&#13;F&#10;G\"><w:r><w:t>a</w:t></w:r></w:ins><w:ins w:id=\"2\" w:author=\"A&#9;B&#10;C&#13;D &amp;amp;\"><w:r><w:t>b</w:t></w:r></w:ins></w:p>");
    for _ in 0..4 {
        let jade = JadeDocx::load(&bytes).unwrap();
        let model = DocxModel::from_word_document(jade.word()).unwrap();
        let revisions = model.revisions();
        let a = revisions.iter().find(|r| r.text == "a").unwrap();
        assert_eq!(a.revision.author.as_deref(), Some(" A B C D E "));
        assert_eq!(a.revision.date.as_deref(), Some("D\tE\rF\nG"));
        let b = revisions.iter().find(|r| r.text == "b").unwrap();
        assert_eq!(b.revision.author.as_deref(), Some("A\tB\nC\rD &amp;"));
        bytes = jade.export(&model).unwrap();
        let package = OpcPackage::open(&bytes).unwrap();
        let xml = package.required_part_str("word/document.xml").unwrap();
        assert!(!xml.contains("A\tB"));
        assert!(xml.contains("A&#x9;B&#xA;C&#xD;D &amp;amp;"));
    }
}
