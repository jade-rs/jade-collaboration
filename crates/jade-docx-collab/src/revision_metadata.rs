//! XML metadata at the file boundary. The current native revision parser
//! returns lexical attribute values; live commands already carry logical text.
//! Decode only just-loaded marks, and preserve attribute whitespace on output.
use jade_docx_document_core::document::RevisionMark;
use jade_docx_document_core::word_document::WordDocument;
use quick_xml::{events::Event, Reader};

#[derive(Clone, Copy)]
enum Boundary {
    Read,
    Write,
}

fn whitespace(raw: &str, boundary: Boundary) -> String {
    let mut result = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match (boundary, ch) {
            (Boundary::Read, '\r') => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                result.push(' ');
            }
            (Boundary::Read, '\t' | '\n') => result.push(' '),
            (Boundary::Write, '\t') => result.push_str("&#x9;"),
            (Boundary::Write, '\n') => result.push_str("&#xA;"),
            (Boundary::Write, '\r') => result.push_str("&#xD;"),
            _ => result.push(ch),
        }
    }
    result
}
fn xml_metadata(xml: &str, boundary: Boundary) -> Result<Option<String>, String> {
    // Other XML, opaque custom parts and package payloads stay byte-identical.
    if !xml.contains("wordprocessingml/") || !xml.contains(['\t', '\r', '\n']) {
        return Ok(None);
    }
    let mut reader = Reader::from_str(xml);
    let mut replacements = Vec::new();
    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(start) | Event::Empty(start) => {
                for attribute in start.attributes() {
                    let attribute = attribute.map_err(|error| error.to_string())?;
                    if !matches!(
                        attribute.key.local_name().as_ref(),
                        b"author" | b"date" | b"dateUtc"
                    ) {
                        continue;
                    }
                    let raw = std::str::from_utf8(attribute.value.as_ref())
                        .map_err(|error| error.to_string())?;
                    if !raw.contains(['\t', '\r', '\n']) {
                        continue;
                    }
                    // read_event borrows this input slice, as do its attribute
                    // values. Locate that exact lexical value without rewriting
                    // namespaces, entities, whitespace or unrelated elements.
                    let std::borrow::Cow::Borrowed(value) = attribute.value else {
                        return Err("revision attribute is not borrowed from XML input".into());
                    };
                    let begin = (value.as_ptr() as usize)
                        .checked_sub(xml.as_ptr() as usize)
                        .ok_or("invalid revision attribute position")?;
                    let end = begin
                        .checked_add(value.len())
                        .ok_or("invalid revision attribute range")?;
                    if xml.as_bytes().get(begin..end) != Some(value) {
                        return Err("revision attribute lies outside XML input".into());
                    }
                    replacements.push((begin..end, whitespace(raw, boundary)));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if replacements.is_empty() {
        return Ok(None);
    }
    let mut result = xml.to_string();
    for (range, value) in replacements.into_iter().rev() {
        result.replace_range(range, &value);
    }
    Ok(Some(result))
}
fn package(bytes: &[u8], boundary: Boundary) -> Result<Vec<u8>, String> {
    let mut package =
        jade_opc::OpcPackage::open_preserving(bytes).map_err(|error| error.to_string())?;
    for part in package.part_names().to_vec() {
        if !part.ends_with(".xml") {
            continue;
        }
        let Some(bytes) = package.part(&part) else {
            continue;
        };
        let Ok(xml) = std::str::from_utf8(bytes) else {
            continue;
        };
        if let Some(rewritten) = xml_metadata(xml, boundary)? {
            package.replace(&part, rewritten.into_bytes());
        }
    }
    package.write().map_err(|error| error.to_string())
}
pub(crate) fn read_package(bytes: &[u8]) -> Result<Vec<u8>, String> {
    package(bytes, Boundary::Read)
}
pub(crate) fn write_package(bytes: &[u8]) -> Result<Vec<u8>, String> {
    package(bytes, Boundary::Write)
}
fn decode(mark: &mut RevisionMark) -> Result<(), String> {
    for value in [
        &mut mark.id,
        &mut mark.author,
        &mut mark.date,
        &mut mark.date_utc,
    ]
    .into_iter()
    .flatten()
    {
        *value = quick_xml::escape::unescape(value)
            .map_err(|error| error.to_string())?
            .into_owned();
    }
    if let Some(insertion) = mark.insertion.as_deref_mut() {
        decode(insertion)?;
    }
    Ok(())
}
pub(crate) fn loaded(word: &mut WordDocument) -> Result<(), String> {
    for surface in 0..word.paragraph_count() {
        if let Some(paragraph) = word.paragraph_mut(surface) {
            for run in &mut paragraph.runs {
                if let Some(mark) = run.revision.as_mut() {
                    decode(mark)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const XML:&str="<w:p xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:ins w:author=\"A\tB\r\nC\rD\nE &amp;#9;\"><w:r w:other=\"keep\tthis\"><w:t>body\r\ntext</w:t></w:r></w:ins></w:p>";
    #[test]
    fn imported_literal_whitespace_obeys_xml_attribute_normalization() {
        let result = xml_metadata(XML, Boundary::Read).unwrap().unwrap();
        assert!(result.contains("w:author=\"A B C D E &amp;#9;\""));
        assert!(result.contains("w:other=\"keep\tthis\""));
        assert!(result.contains("body\r\ntext"));
        assert!(xml_metadata(&result, Boundary::Read).unwrap().is_none());
    }
    #[test]
    fn written_metadata_uses_numeric_references_without_touching_existing_entities() {
        let result = xml_metadata(XML, Boundary::Write).unwrap().unwrap();
        assert!(result.contains("w:author=\"A&#x9;B&#xD;&#xA;C&#xD;D&#xA;E &amp;#9;\""));
        assert!(result.contains("w:other=\"keep\tthis\""));
        assert!(result.contains("body\r\ntext"));
        assert!(xml_metadata(&result, Boundary::Write).unwrap().is_none());
    }
}
