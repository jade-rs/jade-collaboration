mod common;
use common::{commit, fixture};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_docx_collab::intent::{try_resolve_with, Context, IntentOp};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx};
use jade_docx_collab::{NoteKind, StoryId};
use jade_docx_document_core::docx_xml::{DocxXmlDocument, XmlAttribute, XmlNode};
use jade_opc::OpcPackage;
use proptest::prelude::*;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const SETTINGS_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings";

fn rewrite(package: &mut OpcPackage, path: &str, change: impl FnOnce(&mut DocxXmlDocument)) {
    let mut doc = DocxXmlDocument::parse(&package.required_part_str(path).unwrap()).unwrap();
    change(&mut doc);
    doc.rebuild_index();
    package.replace(path, doc.to_string().unwrap().into_bytes());
}

fn without_settings(keep_orphan: bool) -> Vec<u8> {
    let mut p = OpcPackage::open(&fixture("jade-core/hello.docx")).unwrap();
    if !keep_orphan {
        p.remove("word/settings.xml");
    }
    for (path, attribute, value) in [
        ("word/_rels/document.xml.rels", "Type", SETTINGS_REL),
        ("[Content_Types].xml", "PartName", "/word/settings.xml"),
    ] {
        rewrite(&mut p, path, |doc| {
            let root = doc.root().id;
            doc.element_mut(root).unwrap().children.retain(|n| {
                !matches!(n, XmlNode::Element(e)
                if e.attributes.iter().any(|a| a.name == attribute && a.value == value))
            });
        });
    }
    p.write().unwrap()
}

fn with_settings(xml: &str) -> Vec<u8> {
    let mut package = OpcPackage::open(&fixture("jade-core/hello.docx")).unwrap();
    package.replace("word/settings.xml", xml.as_bytes().to_vec());
    package.write().unwrap()
}

fn loaded(bytes: &[u8]) -> (DocxModel, JadeDocx) {
    let jade = JadeDocx::load(bytes).unwrap();
    (DocxModel::from_word_document(jade.word()).unwrap(), jade)
}

#[test]
fn the_files_track_revisions_setting_initializes_the_collaborative_mode() {
    for (value, expected) in [
        ("", true),
        (" w:val=\"true\"", true),
        (" w:val=\"1\"", true),
        (" w:val=\"on\"", true),
        (" w:val=\"false\"", false),
        (" w:val=\"0\"", false),
        (" w:val=\"off\"", false),
    ] {
        let xml = format!(r#"<w:settings xmlns:w="{W}"><w:trackRevisions{value}/></w:settings>"#);
        assert_eq!(loaded(&with_settings(&xml)).0.tracking(), expected, "{xml}");
    }
}

#[test]
fn toggling_tracking_writes_the_file_without_changing_other_settings_or_parts() {
    let xml = format!(
        r#"<w:settings xmlns:w="{W}" xmlns:x="urn:custom"><w:zoom w:percent="125"/><w:trackRevisions w:val="0"/><w:compat/><x:opaque x:key="keep-me"/></w:settings>"#
    );
    let (mut model, mut jade) = loaded(&with_settings(&xml));
    let before = OpcPackage::open(&jade.export_base().unwrap()).unwrap();
    for on in [true, true, false, true, false] {
        commit(&mut model, &mut jade, DocxOp::SetTracking { on });
        let bytes = jade.export(&model).unwrap();
        assert_eq!(loaded(&bytes).0.tracking(), on);
        let after = OpcPackage::open(&bytes).unwrap();
        let settings = after.required_part_str("word/settings.xml").unwrap();
        assert_eq!(
            settings.matches("<w:trackRevisions").count(),
            usize::from(on)
        );
        assert!(settings.contains("keep-me"));
        assert!(settings.contains("125"));
        for part in before.part_names() {
            if part != "word/settings.xml" {
                assert_eq!(before.part(part), after.part(part), "changed {part}");
            }
        }
    }
}

#[test]
fn namespace_aliases_and_explicit_false_values_are_understood() {
    for xml in [
        format!(r#"<q:settings xmlns:q="{W}"><q:trackRevisions/></q:settings>"#),
        format!(r#"<settings xmlns="{W}"><trackRevisions/></settings>"#),
        format!(
            r#"<q:settings xmlns:q="{W}" xmlns:v="{W}"><v:trackRevisions v:val="on"/></q:settings>"#
        ),
    ] {
        let (mut model, mut jade) = loaded(&with_settings(&xml));
        assert!(model.tracking(), "{xml}");
        commit(&mut model, &mut jade, DocxOp::SetTracking { on: false });
        assert!(!loaded(&jade.export(&model).unwrap()).0.tracking());
    }
    let xml = format!(
        r#"<w:settings xmlns:w="{W}" xmlns:x="urn:other"><x:trackRevisions/><w:compat><w:trackRevisions/></w:compat></w:settings>"#
    );
    assert!(!loaded(&with_settings(&xml)).0.tracking());
}

#[test]
fn missing_settings_are_created_with_relationship_and_content_type_and_keep_working_with_notes() {
    let (mut model, mut jade) = loaded(&without_settings(false));
    assert!(!model.tracking());
    commit(&mut model, &mut jade, DocxOp::SetTracking { on: false });
    assert!(OpcPackage::open(&jade.export_base().unwrap())
        .unwrap()
        .part("word/settings.xml")
        .is_none());
    commit(&mut model, &mut jade, DocxOp::SetTracking { on: true });
    let p = OpcPackage::open(&jade.export(&model).unwrap()).unwrap();
    assert!(p
        .required_part_str("word/settings.xml")
        .unwrap()
        .contains("trackRevisions"));
    assert!(p
        .required_part_str("word/_rels/document.xml.rels")
        .unwrap()
        .contains(SETTINGS_REL));
    assert!(p
        .required_part_str("[Content_Types].xml")
        .unwrap()
        .contains("wordprocessingml.settings+xml"));
    assert!(loaded(&p.write().unwrap()).0.tracking());
    let first = Id::new(model.max_lamport() + 1, ReplicaId(1));
    commit(
        &mut model,
        &mut jade,
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: None,
            id: first,
            text: "a note".into(),
        },
    );
    commit(&mut model, &mut jade, DocxOp::SetTracking { on: false });
    let (reloaded, _) = loaded(&jade.export(&model).unwrap());
    assert!(!reloaded.tracking());
    assert_eq!(reloaded.paragraph_texts(), model.paragraph_texts());
}

#[test]
fn adding_settings_preserves_orphan_parts_and_avoids_relationship_id_collisions() {
    let mut p = OpcPackage::open(&without_settings(true)).unwrap();
    let orphan = p.part("word/settings.xml").unwrap().to_vec();
    p.replace("word/custom.xml", b"<custom>keep-me</custom>".to_vec());
    rewrite(&mut p, "word/_rels/document.xml.rels", |doc| {
        let child = doc.create_element(
            "Relationship",
            vec![
                XmlAttribute {
                    name: "Id".into(),
                    value: "rIdJadeSettings".into(),
                },
                XmlAttribute {
                    name: "Type".into(),
                    value: "urn:custom".into(),
                },
                XmlAttribute {
                    name: "Target".into(),
                    value: "custom.xml".into(),
                },
            ],
            Vec::new(),
        );
        let root = doc.root().id;
        doc.element_mut(root)
            .unwrap()
            .children
            .push(XmlNode::Element(child));
    });
    let (mut model, mut jade) = loaded(&p.write().unwrap());
    commit(&mut model, &mut jade, DocxOp::SetTracking { on: true });
    let p = OpcPackage::open(&jade.export(&model).unwrap()).unwrap();
    assert_eq!(p.part("word/settings.xml").unwrap(), orphan);
    assert_eq!(
        p.part("word/custom.xml").unwrap(),
        b"<custom>keep-me</custom>"
    );
    let rels = p.required_part_str("word/_rels/document.xml.rels").unwrap();
    assert!(rels.contains("rIdJadeSettings0"));
    assert!(p.part("word/jade-settings-0.xml").is_some());
    assert!(loaded(&p.write().unwrap()).0.tracking());
    commit(&mut model, &mut jade, DocxOp::SetTracking { on: false });
    assert!(!loaded(&jade.export(&model).unwrap()).0.tracking());
}

#[test]
fn a_nonstandard_settings_part_path_is_used_for_both_reading_and_writing() {
    let mut p = OpcPackage::open(&fixture("jade-core/hello.docx")).unwrap();
    p.remove("word/settings.xml");
    p.replace(
        "word/config/options.xml",
        format!(r#"<q:settings xmlns:q="{W}"><q:trackRevisions/></q:settings>"#).into_bytes(),
    );
    rewrite(&mut p, "word/_rels/document.xml.rels", |doc| {
        let root = doc.root().id;
        for node in &mut doc.element_mut(root).unwrap().children {
            let XmlNode::Element(e) = node else { continue };
            if e.attributes
                .iter()
                .any(|a| a.name == "Type" && a.value == SETTINGS_REL)
            {
                e.attributes
                    .iter_mut()
                    .find(|a| a.name == "Target")
                    .unwrap()
                    .value = "config/options.xml".into();
            }
        }
    });
    rewrite(&mut p, "[Content_Types].xml", |doc| {
        let root = doc.root().id;
        for node in &mut doc.element_mut(root).unwrap().children {
            let XmlNode::Element(e) = node else { continue };
            for attribute in &mut e.attributes {
                if attribute.name == "PartName" && attribute.value == "/word/settings.xml" {
                    attribute.value = "/word/config/options.xml".into();
                }
            }
        }
    });
    let (mut model, mut jade) = loaded(&p.write().unwrap());
    assert!(model.tracking());
    commit(&mut model, &mut jade, DocxOp::SetTracking { on: false });
    let bytes = jade.export(&model).unwrap();
    assert!(!loaded(&bytes).0.tracking());
    let p = OpcPackage::open(&bytes).unwrap();
    assert!(p.part("word/settings.xml").is_none());
    assert!(!p
        .required_part_str("word/config/options.xml")
        .unwrap()
        .contains("trackRevisions"));
}

#[test]
fn invalid_tracking_values_are_reported_instead_of_silently_disabling_tracking() {
    let bytes = with_settings(&format!(
        r#"<w:settings xmlns:w="{W}"><w:trackRevisions w:val="maybe"/></w:settings>"#
    ));
    let jade = JadeDocx::load(&bytes).unwrap();
    assert!(DocxModel::from_word_document(jade.word()).is_err());
}

#[test]
fn old_checkpoints_restore_the_models_authoritative_mode_to_the_base_file() {
    let (mut model, jade) = loaded(&fixture("jade-core/hello.docx"));
    model
        .apply(
            &DocxOp::SetTracking { on: true },
            &ApplyContext {
                origin: ReplicaId(1),
                base_seq: 0,
                seq: None,
            },
        )
        .unwrap();
    let restored = JadeDocx::restore(&jade.export_base().unwrap(), &model).unwrap();
    assert!(loaded(&restored.export_base().unwrap()).0.tracking());
    assert!(loaded(&restored.export(&model).unwrap()).0.tracking());
}

#[test]
fn a_tracking_toggle_does_not_reinterpret_an_edit_already_prepared_by_another_replica() {
    use jade_collab_sync::sim::Simulation;
    use jade_docx_collab::intent::resolve;

    let (model, jade) = loaded(&fixture("jade-core/hello.docx"));
    let mut sim = Simulation::with_materializer(model, jade, 2);
    let queue = |sim: &mut Simulation<DocxModel, JadeDocx>, client, intent| {
        let view = sim.client(client).view().clone();
        let ops = resolve(&view, &intent, &mut |n| sim.client_mut(client).reserve(n)).unwrap();
        for op in ops {
            sim.local(client, op).unwrap();
        }
    };
    for (on, text) in [(true, "plain"), (false, "tracked")] {
        queue(&mut sim, 0, IntentOp::SetTracking { on });
        assert!(sim.deliver_up(0));
        // The other replica has not seen the switch yet. Its local mode
        // determines the explicit operation it submits, even after the switch.
        assert_eq!(sim.client(1).view().tracking(), !on);
        queue(
            &mut sim,
            1,
            IntentOp::InsertText {
                paragraph: 1,
                offset: 0,
                text: text.into(),
            },
        );
        sim.quiesce().unwrap();
        let model = sim.server().document();
        let jade = sim.server().materializer();
        assert_eq!(model.tracking(), on);
        let file = loaded(&jade.export(model).unwrap()).0;
        assert_eq!(file.tracking(), on);
        for state in [model, &file] {
            assert_eq!(state.revisions().iter().any(|r| r.text.contains(text)), !on);
        }
        assert_eq!(sim.client(0).view(), sim.client(1).view());
    }
}

fn name_author(model: &mut DocxModel, jade: &mut JadeDocx) {
    if model.author(ReplicaId(1)) != Some("Ada") {
        common::commit_as(
            model,
            jade,
            DocxOp::SetAuthor {
                replica: ReplicaId(1),
                name: "Ada".into(),
            },
            ReplicaId::ORIGIN,
        );
    }
}

fn marked_text(model: &DocxModel) -> Vec<(char, bool)> {
    let paragraph = &model.paragraphs()[1];
    paragraph
        .text
        .chars()
        .zip(&paragraph.tracked)
        .map(|(c, revision)| (c, revision.inserted.is_some()))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// An independent character list tracks which insertions should survive
    /// accept/reject decisions while the mode, files and checkpoints change.
    #[test]
    fn tracking_mode_and_revisions_survive_toggle_edit_reopen_and_checkpoint_histories(
        missing in any::<bool>(), initial in any::<bool>(),
        steps in prop::collection::vec((0u8..6, any::<bool>(), prop::sample::select(vec!["a", "日本", "🙂"])), 1..40),
    ) {
        let bytes = if missing { without_settings(false) }
            else { with_settings(&format!(r#"<w:settings xmlns:w="{W}"><w:trackRevisions w:val="{initial}"/></w:settings>"#)) };
        let (mut model, mut jade) = loaded(&bytes);
        let mut tracking = !missing && initial;
        let mut reference = marked_text(&model);
        name_author(&mut model, &mut jade);
        for (kind, on, text) in steps {
            match kind {
                0 => {
                    commit(&mut model, &mut jade, DocxOp::SetTracking { on });
                    tracking = on;
                }
                1 => {
                    let mut clock = jade_collab_core::id::Clock::new(ReplicaId(1));
                    clock.observe_lamport(model.max_lamport());
                    let context = Context { replica: Some(ReplicaId(1)), date: Some("2026-10-21T04:00:00Z".into()) };
                    let ops = try_resolve_with(&model, &IntentOp::InsertText {
                        paragraph: 1, offset: model.paragraphs()[1].text.len(), text: text.into(),
                    }, &context, &mut |n| clock.try_reserve(n)).unwrap();
                    for op in ops { commit(&mut model, &mut jade, op); }
                    reference.extend(text.chars().map(|c| (c, tracking)));
                }
                2 => {
                    (model, jade) = loaded(&jade.export(&model).unwrap());
                    name_author(&mut model, &mut jade);
                }
                3 => {
                    let checkpoint: DocxModel = serde_json::from_slice(&serde_json::to_vec(&model).unwrap()).unwrap();
                    jade = JadeDocx::restore(&jade.export_base().unwrap(), &checkpoint).unwrap();
                    model = checkpoint;
                }
                _ => {
                    let revisions: Vec<_> = model.revisions().into_iter().map(|r| r.id).collect();
                    if !revisions.is_empty() {
                        let op = if kind == 4 { DocxOp::AcceptRevisions { revisions } } else { DocxOp::RejectRevisions { revisions } };
                        commit(&mut model, &mut jade, op);
                    }
                    if kind == 4 { for (_, tracked) in &mut reference { *tracked = false; } }
                    else { reference.retain(|(_, tracked)| !tracked); }
                }
            }
            prop_assert_eq!(model.tracking(), tracking);
            prop_assert_eq!(marked_text(&model), reference.clone());
            let (file, _) = loaded(&jade.export(&model).unwrap());
            prop_assert_eq!(file.tracking(), tracking);
            prop_assert_eq!(marked_text(&file), reference.clone());
        }
    }
}
