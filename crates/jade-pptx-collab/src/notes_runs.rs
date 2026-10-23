//! Remove redundant *unformatted* note runs without rebuilding their XML tree.
//! Everything outside eligible adjacent r/t spans remains byte-for-byte intact.
use quick_xml::{events::Event, name::ResolveResult, NsReader};
use std::ops::Range;

pub(crate) const MIN_RUNS: usize = 16;
pub(crate) const CHECK_EVERY: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    P,
    StrictP,
    D,
    StrictD,
    Mc,
    Other,
}
impl Ns {
    fn presentation(self) -> bool {
        matches!(self, Self::P | Self::StrictP)
    }
    fn drawing(self) -> bool {
        matches!(self, Self::D | Self::StrictD)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Space {
    Inherit,
    Preserve,
    Default,
}
struct Node {
    ns: Ns,
    tag: String,
    outer: Range<usize>,
    inner: Range<usize>,
    children: Vec<usize>,
    parent: Option<usize>,
    attributes: usize,
    space: Option<Space>,
    body: bool,
    opaque_text: bool,
}
fn parse(xml: &str) -> Option<Vec<Node>> {
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    loop {
        let start = reader.buffer_position() as usize;
        let (ns, event) = reader.read_resolved_event_into(&mut buffer).ok()?;
        let ns = match ns {
            ResolveResult::Bound(ns) => match ns.as_ref() {
                b"http://schemas.openxmlformats.org/presentationml/2006/main" => Ns::P,
                b"http://purl.oclc.org/ooxml/presentationml/main" => Ns::StrictP,
                b"http://schemas.openxmlformats.org/drawingml/2006/main" => Ns::D,
                b"http://purl.oclc.org/ooxml/drawingml/main" => Ns::StrictD,
                b"http://schemas.openxmlformats.org/markup-compatibility/2006" => Ns::Mc,
                _ => Ns::Other,
            },
            _ => Ns::Other,
        };
        let end = reader.buffer_position() as usize;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let mut attributes = 0;
                let mut space = Some(Space::Inherit);
                let mut body = false;
                for attr in e.attributes() {
                    let attr = attr.ok()?;
                    let key = attr.key.as_ref();
                    if key == b"xmlns" || key.starts_with(b"xmlns:") {
                        continue;
                    }
                    attributes += 1;
                    let value = attr.unescape_value().ok()?;
                    body |= key == b"type" && value == "body";
                    space = if attributes == 1 && key == b"xml:space" {
                        match value.as_ref() {
                            "preserve" => Some(Space::Preserve),
                            "default" => Some(Space::Default),
                            _ => None,
                        }
                    } else {
                        None
                    };
                }
                let parent = stack.last().copied();
                let index = nodes.len();
                if let Some(parent) = parent {
                    nodes[parent].children.push(index);
                }
                nodes.push(Node {
                    ns,
                    tag: std::str::from_utf8(e.local_name().as_ref())
                        .ok()?
                        .to_owned(),
                    outer: start..end,
                    inner: end..end,
                    children: Vec::new(),
                    parent,
                    attributes,
                    space,
                    body,
                    opaque_text: false,
                });
                if matches!(event, Event::Start(_)) {
                    stack.push(index);
                }
            }
            Event::End(_) => {
                let index = stack.pop()?;
                nodes[index].inner.end = start;
                nodes[index].outer.end = end;
            }
            Event::Text(t) => {
                t.xml_content().ok()?;
            }
            Event::GeneralRef(r) => {
                let value = r.decode().ok()?;
                quick_xml::escape::unescape(&format!("&{value};")).ok()?;
            }
            Event::DocType(_) => return None,
            Event::CData(_) | Event::Comment(_) | Event::PI(_) => {
                if let Some(&index) = stack.last() {
                    nodes[index].opaque_text = true;
                }
            }
            Event::Decl(_) if !stack.is_empty() => return None,
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !stack.is_empty() {
        return None;
    }
    Some(nodes)
}
fn child(nodes: &[Node], index: usize, tag: &str) -> Option<usize> {
    nodes[index]
        .children
        .iter()
        .copied()
        .find(|&i| nodes[i].ns.presentation() && nodes[i].tag == tag)
}
fn body_paragraphs(nodes: &[Node]) -> Option<Vec<usize>> {
    if nodes.first()?.tag != "notes"
        || !nodes[0].ns.presentation()
        || nodes.iter().filter(|n| n.parent.is_none()).count() != 1
    {
        return None;
    }
    let tree = child(nodes, child(nodes, 0, "cSld")?, "spTree")?;
    let mut pending: Vec<_> = nodes[tree].children.iter().copied().rev().collect();
    let mut bodies = Vec::new();
    while let Some(i) = pending.pop() {
        let n = &nodes[i];
        if n.ns == Ns::Mc && n.tag == "AlternateContent" {
            if let Some(&fallback) = n
                .children
                .iter()
                .find(|&&j| nodes[j].ns == Ns::Mc && nodes[j].tag == "Fallback")
            {
                pending.extend(nodes[fallback].children.iter().copied().rev());
            }
        } else if n.ns.presentation()
            && matches!(n.tag.as_str(), "sp" | "pic" | "cxnSp" | "grpSp")
            && child(nodes, i, "nvSpPr")
                .and_then(|j| child(nodes, j, "nvPr"))
                .and_then(|j| child(nodes, j, "ph"))
                .is_some_and(|j| nodes[j].body)
        {
            bodies.push(i);
        }
    }
    if bodies.len() != 1 || nodes[bodies[0]].tag != "sp" {
        return None;
    }
    let body = child(nodes, bodies[0], "txBody")?;
    Some(
        nodes[body]
            .children
            .iter()
            .copied()
            .filter(|&i| nodes[i].ns.drawing() && nodes[i].tag == "p")
            .collect(),
    )
}
fn plain(nodes: &[Node], i: usize, paragraph_ns: Ns, xml: &str) -> Option<(usize, Space)> {
    let r = &nodes[i];
    if r.ns != paragraph_ns || r.tag != "r" || r.attributes != 0 || r.children.len() != 1 {
        return None;
    }
    let t = r.children[0];
    let n = &nodes[t];
    if n.ns != paragraph_ns
        || n.tag != "t"
        || !n.children.is_empty()
        || n.opaque_text
        || r.inner.start != n.outer.start
        || r.inner.end != n.outer.end
        || xml[n.inner.clone()].contains('\r')
    {
        return None;
    }
    Some((t, n.space?))
}
#[derive(Default)]
struct Group {
    runs: Vec<(usize, usize)>,
    tail: Vec<u8>,
    space: Option<Space>,
}
impl Group {
    fn adjacent(&self, nodes: &[Node], run: usize, space: Space, raw: &str) -> bool {
        if let Some(&(last, _)) = self.runs.last() {
            if nodes[last].outer.end != nodes[run].outer.start || self.space != Some(space) {
                return false;
            }
        }
        // Removing t/r boundaries must not manufacture an illegal XML ]]>.
        let mut boundary = self.tail.clone();
        boundary.extend_from_slice(&raw.as_bytes()[..raw.len().min(2)]);
        !boundary.windows(3).any(|w| w == b"]]>")
    }
    fn push(&mut self, r: usize, t: usize, space: Space, raw: &str) {
        self.runs.push((r, t));
        self.space = Some(space);
        self.tail
            .extend_from_slice(&raw.as_bytes()[raw.len().saturating_sub(2)..]);
        if self.tail.len() > 2 {
            self.tail.drain(..self.tail.len() - 2);
        }
    }
    fn flush(&mut self, nodes: &[Node], xml: &str, patches: &mut Vec<(Range<usize>, String)>) {
        if self.runs.len() >= MIN_RUNS {
            let (first, t) = self.runs[0];
            let last = self.runs.last().unwrap().0;
            let raw: String = self
                .runs
                .iter()
                .map(|&(_, t)| &xml[nodes[t].inner.clone()])
                .collect();
            let text = &nodes[t];
            let replacement = if text.inner.start == text.outer.end {
                let empty = &xml[text.outer.clone()];
                let end = empty.as_bytes()[1..]
                    .iter()
                    .position(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>'))
                    .unwrap()
                    + 1;
                format!("{}>{raw}</{}>", &empty[..empty.len() - 2], &empty[1..end])
            } else {
                format!(
                    "{}{raw}{}",
                    &xml[text.outer.start..text.inner.start],
                    &xml[text.inner.end..text.outer.end]
                )
            };
            patches.push((
                nodes[first].outer.start..nodes[last].outer.end,
                format!(
                    "{}{}{}",
                    &xml[nodes[first].outer.start..text.outer.start],
                    replacement,
                    &xml[text.outer.end..nodes[first].outer.end]
                ),
            ));
        }
        *self = Self::default();
    }
}

pub(crate) fn compact(xml: &str) -> Option<String> {
    let nodes = parse(xml)?;
    let mut patches = Vec::new();
    for p in body_paragraphs(&nodes)? {
        let mut group = Group::default();
        for &r in &nodes[p].children {
            if let Some((t, space)) = plain(&nodes, r, nodes[p].ns, xml) {
                let raw = &xml[nodes[t].inner.clone()];
                if !group.adjacent(&nodes, r, space, raw) {
                    group.flush(&nodes, xml, &mut patches);
                }
                group.push(r, t, space, raw);
            } else {
                group.flush(&nodes, xml, &mut patches);
            }
        }
        group.flush(&nodes, xml, &mut patches);
    }
    if patches.is_empty() {
        return None;
    }
    let mut result = String::with_capacity(xml.len());
    let mut cursor = 0;
    for (range, replacement) in patches {
        result.push_str(&xml[cursor..range.start]);
        result.push_str(&replacement);
        cursor = range.end;
    }
    result.push_str(&xml[cursor..]);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    fn note(runs: &str) -> String {
        format!("<p:notes xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id=\"2\"/><p:nvPr><p:ph type=\"body\"/></p:nvPr></p:nvSpPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p>{runs}</a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>")
    }
    #[test]
    fn adjacent_plain_text_is_joined_and_unmodelled_bytes_stay_exact() {
        let run = "<a:r><a:t>A&amp;字🙂</a:t></a:r>";
        let barrier="<a:r custom=\"keep\"><a:rPr b=\"1\"><a:extLst><x:data xmlns:x=\"urn:private\"/></a:extLst></a:rPr><a:t>opaque</a:t></a:r>";
        let source = note(&format!("{}{}{}", run.repeat(20), barrier, run.repeat(16)));
        let expected = note(&format!(
            "<a:r><a:t>{}</a:t></a:r>{barrier}<a:r><a:t>{}</a:t></a:r>",
            "A&amp;字🙂".repeat(20),
            "A&amp;字🙂".repeat(16)
        ));
        assert_eq!(compact(&source), Some(expected));
    }
    #[test]
    fn empty_text_can_be_the_surviving_run() {
        let source = note(&format!(
            "<a:r><a:t/></a:r>{}",
            "<a:r><a:t>x</a:t></a:r>".repeat(16)
        ));
        assert_eq!(
            compact(&source),
            Some(note(&format!("<a:r><a:t>{}</a:t></a:r>", "x".repeat(16))))
        );
    }
    #[test]
    fn namespace_aliases_resolve_but_foreign_names_are_barriers() {
        let run="<z:r xmlns:z=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><z:t>x</z:t></z:r>";
        let source = note(&run.repeat(20));
        let result = compact(&source).unwrap();
        assert!(result.contains(&format!("<z:t>{}</z:t>", "x".repeat(20))));
        assert!(compact(&note(
            &run.replace("2006/main", "2006/not-drawing").repeat(20)
        ))
        .is_none());
    }
    #[test]
    fn whitespace_modes_opaque_content_and_xml_boundaries_are_not_crossed() {
        for run in [
            "<a:r x=\"keep\"><a:t>x</a:t></a:r>",
            "<a:r><a:rPr/><a:t>x</a:t></a:r>",
            "<a:r><a:t><![CDATA[x]]></a:t></a:r>",
            "<a:r><a:t>x<!--keep--></a:t></a:r>",
            "<a:r><a:t>\r</a:t></a:r>",
        ] {
            assert!(compact(&note(&run.repeat(20))).is_none(), "{run}");
        }
        let a = "<a:r><a:t>]]</a:t></a:r>";
        let b = "<a:r><a:t>></a:t></a:r>";
        assert!(compact(&note(&format!("{a}{b}").repeat(20))).is_none());
        let a = "<a:r><a:t xml:space=\"preserve\"> </a:t></a:r>";
        let b = "<a:r><a:t xml:space=\"default\">x</a:t></a:r>";
        assert!(compact(&note(&format!("{a}{b}").repeat(20))).is_none());
    }
    #[test]
    fn strict_notes_keep_their_namespace_and_unknown_siblings() {
        let run = "<a:r><a:t>&#13;&amp;lt;</a:t></a:r>";
        let source = note(&run.repeat(20))
            .replace(
                "http://schemas.openxmlformats.org/presentationml/2006/main",
                "http://purl.oclc.org/ooxml/presentationml/main",
            )
            .replace(
                "http://schemas.openxmlformats.org/drawingml/2006/main",
                "http://purl.oclc.org/ooxml/drawingml/main",
            );
        let result = compact(&source).unwrap();
        assert!(result.contains("http://purl.oclc.org/ooxml/drawingml/main"));
        assert!(result.contains(&"&#13;&amp;lt;".repeat(20)));
    }

    fn native_notes(xml: &str) -> jade_pptx_writer::TextSnapshot {
        use jade_opc::{parse_relationships, rels_part_name, resolve_target, OpcPackage};
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../jade-core/public/fixtures/sample-deck.pptx"),
        )
        .unwrap();
        let mut writer = jade_pptx_writer::EditSession::open(&bytes).unwrap();
        writer.set_slide_notes(0, "seed").unwrap();
        let slide = jade_pptx_reader::read_deck(writer.bytes()).unwrap().slides[0]
            .part_name
            .clone();
        let mut package = OpcPackage::open_preserving(writer.bytes()).unwrap();
        let rels =
            parse_relationships(&package.required_part_str(&rels_part_name(&slide)).unwrap())
                .unwrap();
        let part = resolve_target(&slide, &rels.by_type_suffix("/notesSlide")[0].target);
        package.replace(&part, xml.as_bytes().to_vec());
        jade_pptx_writer::notes_text(&package.write().unwrap(), 0).unwrap()
    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn native_text_capabilities_and_opaque_positions_survive_compaction(
            pieces in prop::collection::vec((0u8..6,0usize..8),0..64), strict in any::<bool>(),
        ) {
            let mut body="<a:r><a:t>s</a:t></a:r>".repeat(32);
            let mut reference="s".repeat(32);
            let mut opaque=Vec::new();
            for (i,(kind,text)) in pieces.into_iter().enumerate() {
                let texts=["", "字🙂", "&<>\"'", "e\u{301}", "\t", "\n", "\r", "]]>"];
                let text=texts[text];
                if kind<2 {
                    let escaped=quick_xml::escape::escape(text).replace('\r',"&#13;");
                    body.push_str(&format!("<a:r><a:t>{escaped}</a:t></a:r>"));
                    reference.push_str(text);
                } else {
                    let raw=match kind {
                        2=>format!("<a:r><a:rPr b=\"1\" lang=\"en-US\"/><a:t>bold{i}</a:t></a:r>"),
                        3=>format!("<u:r xmlns:u=\"urn:opaque\"><u:t>foreign{i}</u:t></u:r>"),
                        4=>format!("<a:r><a:t><![CDATA[cdata{i}字🙂]]></a:t></a:r>"),
                        _=>format!("<u:keep xmlns:u=\"urn:opaque\" id=\"{i}\"><!--exact--></u:keep>"),
                    };
                    let marker=format!("(opaque{i})");
                    reference.push_str(&marker);
                    body.push_str(&raw);
                    opaque.push((raw,marker));
                }
            }
            let mut before=note(&body);
            if strict {
                before=before.replace("http://schemas.openxmlformats.org/presentationml/2006/main","http://purl.oclc.org/ooxml/presentationml/main").replace("http://schemas.openxmlformats.org/drawingml/2006/main","http://purl.oclc.org/ooxml/drawingml/main");
            }
            let after=compact(&before).expect("mandatory leading plain group must compact");
            prop_assert!(compact(&after).is_none(),"compaction must be idempotent");
            let old=native_notes(&before); let new=native_notes(&after);
            prop_assert_eq!((&old.text,old.can_edit,old.locked),(&new.text,new.can_edit,new.locked));
            let mut projected=after;
            for (raw,marker) in opaque {
                prop_assert_eq!(projected.matches(&raw).count(),1,"opaque XML must remain exactly once");
                projected=projected.replace(&raw,&format!("<a:r><a:t>{marker}</a:t></a:r>"));
            }
            prop_assert_eq!(native_notes(&projected).text,reference,"opaque boundaries must keep their text positions");
        }
    }

    #[test]
    fn paragraphs_and_nonbody_placeholders_are_never_collapsed() {
        let run = "<a:r><a:t>x</a:t></a:r>";
        let source = note(&format!(
            "{}</a:p><a:p custom=\"keep\">{}",
            run.repeat(16),
            run.repeat(16)
        ));
        let expected = note(&format!(
            "<a:r><a:t>{}</a:t></a:r></a:p><a:p custom=\"keep\"><a:r><a:t>{}</a:t></a:r>",
            "x".repeat(16),
            "x".repeat(16)
        ));
        assert_eq!(compact(&source), Some(expected));
        let body = note(&run.repeat(16));
        let header=format!("<p:sp><p:nvSpPr><p:cNvPr id=\"3\"/><p:nvPr><p:ph type=\"hdr\"/></p:nvPr></p:nvSpPr><p:txBody><a:p>{}</a:p></p:txBody></p:sp>",run.repeat(32));
        let source = body.replace("<p:spTree>", &format!("<p:spTree>{header}"));
        assert!(compact(&source).unwrap().contains(&header));
        assert!(compact(&source.replace("type=\"hdr\"", "type=\"body\"")).is_none());
    }
}
