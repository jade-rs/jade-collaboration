//! Column styles name stable existing columns. The native writer stretches
//! `<col min=… max=…>` across an insertion; remove that accidental style from
//! the new column while retaining its width, visibility and unknown metadata.
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::{NsReader, Writer};

fn sheet_namespace(ns: ResolveResult<'_>) -> bool {
    matches!(ns, ResolveResult::Bound(ns) if matches!(ns.as_ref(),
        b"http://schemas.openxmlformats.org/spreadsheetml/2006/main"
        | b"http://purl.oclc.org/ooxml/spreadsheetml/main"))
}

pub(crate) fn clear_style(xml: &str, column: u32) -> Result<Option<String>, String> {
    let mut reader = NsReader::from_str(xml);
    let mut depth = 0;
    let mut in_columns = false;
    let mut replacements = Vec::new();
    loop {
        let start = reader.buffer_position() as usize;
        let (ns, event) = reader.read_resolved_event().map_err(|e| e.to_string())?;
        let in_sheet = sheet_namespace(ns);
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let empty = matches!(event, Event::Empty(_));
                let local = element.local_name();
                if depth == 1 && in_sheet && local.as_ref() == b"cols" && !empty {
                    in_columns = true;
                }
                if depth == 2 && in_columns && in_sheet && local.as_ref() == b"col" {
                    let attributes = element
                        .attributes()
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?;
                    let index = |key: &[u8]| {
                        attributes
                            .iter()
                            .find(|a| a.key.as_ref() == key)
                            .and_then(|a| std::str::from_utf8(&a.value).ok()?.parse::<u32>().ok())
                    };
                    let (first, last) = (index(b"min"), index(b"max"));
                    if let (Some(first), Some(last)) = (first, last) {
                        if first <= column
                            && column <= last
                            && attributes.iter().any(|a| a.key.as_ref() == b"style")
                        {
                            let head_end = reader.buffer_position() as usize;
                            if !empty {
                                reader
                                    .read_to_end(QName(element.name().as_ref()))
                                    .map_err(|e| e.to_string())?;
                            }
                            let end = reader.buffer_position() as usize;
                            let name = std::str::from_utf8(element.name().as_ref())
                                .map_err(|e| e.to_string())?
                                .to_owned();
                            let mut writer = Writer::new(Vec::new());
                            for (from, to, keep_style) in [
                                (first, column.saturating_sub(1), true),
                                (column, column, false),
                                (column + 1, last, true),
                            ] {
                                if from > to {
                                    continue;
                                }
                                let mut head = BytesStart::new(&name);
                                for a in &attributes {
                                    if !matches!(a.key.as_ref(), b"min" | b"max")
                                        && (keep_style || a.key.as_ref() != b"style")
                                    {
                                        head.push_attribute(a.clone());
                                    }
                                }
                                let (from, to) = (from.to_string(), to.to_string());
                                head.push_attribute(("min", from.as_str()));
                                head.push_attribute(("max", to.as_str()));
                                writer
                                    .write_event(if empty {
                                        Event::Empty(head)
                                    } else {
                                        Event::Start(head)
                                    })
                                    .map_err(|e| e.to_string())?;
                                writer
                                    .get_mut()
                                    .extend_from_slice(&xml.as_bytes()[head_end..end]);
                            }
                            replacements.push((
                                start,
                                end,
                                String::from_utf8(writer.into_inner())
                                    .map_err(|e| e.to_string())?,
                            ));
                            continue;
                        }
                    }
                }
                if !empty {
                    depth += 1;
                }
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 1 {
                    in_columns = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if replacements.is_empty() {
        return Ok(None);
    }
    let mut out = xml.to_owned();
    for (start, end, replacement) in replacements.into_iter().rev() {
        out.replace_range(start..end, &replacement);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splits_only_the_new_columns_style_and_preserves_other_metadata() {
        let xml = r#"<x:worksheet xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:e="urn:extra"><x:cols><x:col min="2" max="5" style="7" width="18" hidden="1" e:label="a&amp;b"></x:col></x:cols><x:extLst><e:col min="2" max="5" style="9"/></x:extLst></x:worksheet>"#;
        let split = clear_style(xml, 3).unwrap().unwrap();
        assert!(
            split.contains(r#"style="7" width="18" hidden="1" e:label="a&amp;b" min="2" max="2""#)
        );
        assert!(split.contains(r#"width="18" hidden="1" e:label="a&amp;b" min="3" max="3""#));
        assert!(
            split.contains(r#"style="7" width="18" hidden="1" e:label="a&amp;b" min="4" max="5""#)
        );
        assert!(split.contains(r#"<e:col min="2" max="5" style="9"/>"#));
        assert!(clear_style(&split, 3).unwrap().is_none());
        assert!(clear_style(xml, 8).unwrap().is_none());
        // Removing the first/last/single column must not produce an invalid
        // empty span or a zero-based column in OOXML.
        for column in [2, 5] {
            let boundary = clear_style(xml, column).unwrap().unwrap();
            assert!(!boundary.contains("min=\"0\""));
            assert!(clear_style(&boundary, column).unwrap().is_none());
        }
        let singleton = r#"<worksheet xmlns="http://purl.oclc.org/ooxml/spreadsheetml/main"><cols><col min="1" max="1" style="7" width="20"/></cols></worksheet>"#;
        let cleared = clear_style(singleton, 1).unwrap().unwrap();
        assert!(cleared.contains(r#"<col width="20" min="1" max="1"/>"#));
        assert!(!cleared.contains("style="));
    }
}
