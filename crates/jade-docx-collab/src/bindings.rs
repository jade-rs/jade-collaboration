//! The native run mapper skips move wrappers, although the parser flattens
//! their runs. Map against an id-preserving view using equivalent insertion/
//! deletion wrappers; keep the actual XML and revision kinds unchanged.
use jade_docx_document_core::docx_xml::{
    attach_document_xml_ids, attach_text_box_xml_ids, XmlElement, XmlNode,
};
use jade_docx_document_core::word_document::WordDocument;

fn contains_move(element: &XmlElement) -> bool {
    matches!(element.name.as_str(), "w:moveFrom" | "w:moveTo")
        || element
            .children
            .iter()
            .any(|node| matches!(node, XmlNode::Element(child) if contains_move(child)))
}

fn normalize(element: &mut XmlElement) {
    match element.name.as_str() {
        "w:moveFrom" => element.name = "w:del".into(),
        "w:moveTo" => element.name = "w:ins".into(),
        _ => {}
    }
    for child in &mut element.children {
        if let XmlNode::Element(child) = child {
            normalize(child);
        }
    }
}

pub(crate) fn refresh(word: &mut WordDocument) {
    let Some(raw) = word
        .raw_document
        .as_ref()
        .filter(|xml| contains_move(xml.root()))
    else {
        return;
    };
    let mut mapping = raw.clone();
    let root = mapping.root().id;
    if let Some(root) = mapping.element_mut(root) {
        normalize(root);
    }
    let Some(document) = word.document_mut() else {
        return;
    };
    // Only names changed; every node id and tree position is identical.
    let _ = attach_document_xml_ids(document, &mapping);
    attach_text_box_xml_ids(document, &mapping);
}
