//! The persisted tracking switch, independent of jade's per-edit revision session.
use jade_docx_document_core::docx_xml::{DocxXmlDocument, XmlAttribute, XmlElement, XmlNode};
use jade_docx_document_core::word_document::{DocumentPart, WordDocument};
use jade_opc::{rels_part_name, resolve_target, OpcPackage};
use std::collections::{HashMap, HashSet};

const WORD: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const STRICT_WORD: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const SETTINGS_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";
const STRICT_SETTINGS_REL: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/settings";
const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const TYPES: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const SETTINGS_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml";
type Namespaces = HashMap<String, String>;

fn scope(parent: &Namespaces, element: &XmlElement) -> Namespaces {
    let mut out = parent.clone();
    for a in &element.attributes {
        if a.name == "xmlns" {
            out.insert(String::new(), a.value.clone());
        } else if let Some(prefix) = a.name.strip_prefix("xmlns:") {
            out.insert(prefix.into(), a.value.clone());
        }
    }
    out
}
fn split(name: &str) -> (&str, &str) {
    name.split_once(':').unwrap_or(("", name))
}
fn matches(name: &str, ns: &Namespaces, uri: &str, local: &str) -> bool {
    let (prefix, actual) = split(name);
    actual == local && ns.get(prefix).is_some_and(|bound| bound == uri)
}
fn child_name(root: &XmlElement, local: &str) -> String {
    let prefix = split(&root.name).0;
    if prefix.is_empty() {
        local.into()
    } else {
        format!("{prefix}:{local}")
    }
}
fn parse(xml: &str) -> Result<DocxXmlDocument, String> {
    DocxXmlDocument::parse(xml).map_err(|e| format!("settings XML: {e}"))
}
fn word_scope(root: &XmlElement) -> Result<(Namespaces, String), String> {
    let ns = scope(&Namespaces::new(), root);
    let uri = ns.get(split(&root.name).0).cloned().unwrap_or_default();
    if split(&root.name).1 != "settings" || !matches!(uri.as_str(), WORD | STRICT_WORD) {
        return Err("settings part has no WordprocessingML settings root".into());
    }
    Ok((ns, uri))
}
fn is_flag(e: &XmlElement, parent: &Namespaces, uri: &str) -> bool {
    matches(&e.name, &scope(parent, e), uri, "trackRevisions")
}
fn is_value(a: &XmlAttribute, ns: &Namespaces, uri: &str) -> bool {
    a.name.contains(':') && matches(&a.name, ns, uri, "val")
}
fn path(word: &WordDocument) -> Option<String> {
    let main = word.document_part.as_ref()?;
    main.relationships
        .iter()
        .find(|r| {
            matches!(r.r#type.as_str(), SETTINGS_REL | STRICT_SETTINGS_REL)
                && r.target_mode.as_deref() != Some("External")
        })
        .map(|r| resolve_target(&main.path, &r.target))
}

pub(crate) fn tracking(word: &WordDocument) -> Result<bool, String> {
    let Some(path) = path(word) else {
        return Ok(false);
    };
    let part = word
        .parts
        .get(&path)
        .ok_or_else(|| format!("settings part {path} is missing"))?;
    let doc = parse(&part.content)?;
    let (ns, uri) = word_scope(doc.root())?;
    for node in &doc.root().children {
        let XmlNode::Element(e) = node else { continue };
        if !is_flag(e, &ns, &uri) {
            continue;
        }
        let local = scope(&ns, e);
        return match e
            .attributes
            .iter()
            .find(|a| is_value(a, &local, &uri))
            .map(|a| a.value.as_str())
        {
            None | Some("true" | "on" | "1") => Ok(true),
            Some("false" | "off" | "0") => Ok(false),
            Some(value) => Err(format!("invalid trackRevisions value {value:?}")),
        };
    }
    Ok(false)
}

/// CT_Settings orders trackRevisions after revisionView and before the
/// remaining settings. Use local names so alias/default namespaces work too.
const BEFORE_TRACKING: &[&str] = &[
    "writeProtection",
    "view",
    "zoom",
    "removePersonalInformation",
    "removeDateAndTime",
    "doNotDisplayPageBoundaries",
    "displayBackgroundShape",
    "printPostScriptOverText",
    "printFractionalCharacterWidth",
    "printFormsData",
    "embedTrueTypeFonts",
    "embedSystemFonts",
    "saveSubsetFonts",
    "saveFormsData",
    "mirrorMargins",
    "alignBordersAndEdges",
    "bordersDoNotSurroundHeader",
    "bordersDoNotSurroundFooter",
    "gutterAtTop",
    "hideSpellingErrors",
    "hideGrammaticalErrors",
    "activeWritingStyle",
    "proofState",
    "formsDesign",
    "attachedTemplate",
    "linkStyles",
    "stylePaneFormatFilter",
    "stylePaneSortMethod",
    "documentType",
    "mailMerge",
    "revisionView",
];

fn toggle(xml: &str, on: bool) -> Result<String, String> {
    let mut doc = parse(xml)?;
    let (ns, uri) = word_scope(doc.root())?;
    let first = doc.root().children.iter().find_map(|n| match n {
        XmlNode::Element(e) if is_flag(e, &ns, &uri) => Some(e.clone()),
        _ => None,
    });
    let mut flag = if on {
        Some(match first {
            Some(mut e) => {
                let local = scope(&ns, &e);
                e.attributes.retain(|a| !is_value(a, &local, &uri));
                e
            }
            None => doc.create_element(
                child_name(doc.root(), "trackRevisions"),
                Vec::new(),
                Vec::new(),
            ),
        })
    } else {
        None
    };
    let root_id = doc.root().id;
    let root = doc.element_mut(root_id).expect("parsed root");
    let mut children = Vec::new();
    let mut placed = false;
    for child in std::mem::take(&mut root.children) {
        if matches!(&child, XmlNode::Element(e) if is_flag(e, &ns, &uri)) {
            if let Some(flag) = flag.take() {
                children.push(XmlNode::Element(flag));
                placed = true;
            }
        } else {
            children.push(child);
        }
    }
    if on && !placed {
        let at = children
            .iter()
            .enumerate()
            .filter_map(|(i, n)| match n {
                XmlNode::Element(e)
                    if BEFORE_TRACKING.contains(&split(&e.name).1)
                        && matches(&e.name, &scope(&ns, e), &uri, split(&e.name).1) =>
                {
                    Some(i + 1)
                }
                _ => None,
            })
            .next_back()
            .unwrap_or(0);
        children.insert(at, XmlNode::Element(flag.expect("new flag")));
    }
    root.children = children;
    doc.rebuild_index();
    doc.to_string().map_err(|e| e.to_string())
}

fn attr(name: &str, value: &str) -> XmlAttribute {
    XmlAttribute {
        name: name.into(),
        value: value.into(),
    }
}
fn attribute<'a>(e: &'a XmlElement, name: &str) -> Option<&'a str> {
    e.attributes
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.value.as_str())
}
fn metadata(xml: &str, namespace: &str, root: &str) -> Result<DocxXmlDocument, String> {
    let doc = parse(xml)?;
    if !matches(
        &doc.root().name,
        &scope(&Namespaces::new(), doc.root()),
        namespace,
        root,
    ) {
        return Err(format!("expected OPC {root} root"));
    }
    Ok(doc)
}

/// Add the optional part and both OPC declarations atomically. Reloading only
/// on this first creation refreshes jade's relationship caches as well.
fn create(word: &WordDocument) -> Result<Vec<u8>, String> {
    let bytes = word.export_docx().map_err(|e| e.to_string())?;
    let mut package = OpcPackage::open(&bytes).map_err(|e| e.to_string())?;
    let main = &word
        .document_part
        .as_ref()
        .ok_or("the document has no main part")?
        .path;
    let directory = main.rsplit_once('/').map_or("", |(d, _)| d);
    let qualified = |name: &str| {
        if directory.is_empty() {
            name.into()
        } else {
            format!("{directory}/{name}")
        }
    };
    let mut name = "settings.xml".to_string();
    for n in 0.. {
        if package.part(&qualified(&name)).is_none() {
            break;
        }
        name = format!("jade-settings-{n}.xml");
    }
    let part = qualified(&name);
    let xml = toggle(&format!(r#"<w:settings xmlns:w="{WORD}"/>"#), true)?;
    package.replace(&part, xml.into_bytes());
    let rels_path = rels_part_name(main);
    let rels_xml = package
        .part_str(&rels_path)
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| format!(r#"<Relationships xmlns="{RELS}"/>"#));
    let mut rels = metadata(&rels_xml, RELS, "Relationships")?;
    let ids: HashSet<_> = rels
        .root()
        .children
        .iter()
        .filter_map(|n| match n {
            XmlNode::Element(e) => attribute(e, "Id").map(str::to_owned),
            _ => None,
        })
        .collect();
    let mut id = "rIdJadeSettings".to_string();
    for n in 0.. {
        if !ids.contains(&id) {
            break;
        }
        id = format!("rIdJadeSettings{n}");
    }
    let rel = rels.create_element(
        child_name(rels.root(), "Relationship"),
        vec![
            attr("Id", &id),
            attr("Type", SETTINGS_REL),
            attr("Target", &name),
        ],
        Vec::new(),
    );
    let root_id = rels.root().id;
    rels.element_mut(root_id)
        .expect("root")
        .children
        .push(XmlNode::Element(rel));
    rels.rebuild_index();
    package.replace(
        &rels_path,
        rels.to_string().map_err(|e| e.to_string())?.into_bytes(),
    );
    let mut types = metadata(
        &package
            .required_part_str("[Content_Types].xml")
            .map_err(|e| e.to_string())?,
        TYPES,
        "Types",
    )?;
    let part_name = format!("/{part}");
    let type_namespaces = scope(&Namespaces::new(), types.root());
    let root_id = types.root().id;
    let root = types.element_mut(root_id).expect("root");
    // A dangling declaration may predate the missing part; replace it once.
    root.children.retain(|n| {
        !matches!(n, XmlNode::Element(e)
        if matches(&e.name, &scope(&type_namespaces, e), TYPES, "Override")
            && attribute(e, "PartName") == Some(part_name.as_str()))
    });
    let declaration = types.create_element(
        child_name(types.root(), "Override"),
        vec![
            attr("PartName", &part_name),
            attr("ContentType", SETTINGS_TYPE),
        ],
        Vec::new(),
    );
    types
        .element_mut(root_id)
        .expect("root")
        .children
        .push(XmlNode::Element(declaration));
    types.rebuild_index();
    package.replace(
        "[Content_Types].xml",
        types.to_string().map_err(|e| e.to_string())?.into_bytes(),
    );
    package.write().map_err(|e| e.to_string())
}

pub(crate) fn set_tracking(word: &mut WordDocument, on: bool) -> Result<(), String> {
    let Some(path) = path(word) else {
        if on {
            let bytes = create(word)?;
            let loaded = crate::JadeDocx::load(&bytes)?;
            *word = loaded.word().clone();
        }
        return Ok(());
    };
    let part = word
        .parts
        .get_mut(&path)
        .ok_or_else(|| format!("settings part {path} is missing"))?;
    let content = toggle(&part.content, on)?;
    *part = DocumentPart {
        content,
        ..part.clone()
    };
    Ok(())
}
