//! New flow atoms are unique across every story, including hidden history.
mod common;

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::{ApplyContext, Document, Replica, Sequencer, SubmitOutcome};
use jade_docx_collab::{Anchor, DocxModel, DocxOp, JadeDocx, NoteKind, StoryId};
use proptest::prelude::*;

fn id(n: u64) -> Id {
    Id::new(n, ReplicaId(1))
}
fn context() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    }
}
fn allocated(first: Id) -> Vec<DocxOp> {
    vec![
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first,
            text: "🙂".into(),
        },
        DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: None,
            first,
            text: "🙂".into(),
            date: None,
        },
        DocxOp::SplitParagraph {
            story: StoryId::Body,
            parent: None,
            id: first,
        },
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: None,
            id: first,
            text: "n".into(),
        },
        DocxOp::InsertNote {
            kind: NoteKind::Endnote,
            story: StoryId::Body,
            parent: None,
            id: first,
            text: "n".into(),
        },
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: None,
            id: first,
            text: "x".into(),
            width_px: 100,
            height_px: 100,
        },
    ]
}

#[test]
fn every_new_atom_range_checks_all_stories_and_retains_deleted_identifiers() {
    let mut model = DocxModel::from_paragraphs(&["body"]);
    for op in [
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: id(100),
            text: "b".into(),
        },
        DocxOp::SplitParagraph {
            story: StoryId::Body,
            parent: None,
            id: id(200),
        },
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: None,
            id: id(300),
            text: "n".into(),
        },
        DocxOp::InsertNote {
            kind: NoteKind::Endnote,
            story: StoryId::Body,
            parent: None,
            id: id(400),
            text: "n".into(),
        },
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: None,
            id: id(500),
            text: "x".into(),
            width_px: 100,
            height_px: 100,
        },
        DocxOp::InsertTable {
            anchor: Anchor::Start,
            id: id(600),
            rows: vec![id(601)],
            columns: vec![id(602)],
        },
        DocxOp::InsertText {
            story: StoryId::Cell {
                table: id(600),
                row: id(601),
                column: id(602),
            },
            parent: None,
            first: id(700),
            text: "c".into(),
        },
    ] {
        model.apply(&op, &context()).unwrap();
    }
    for delete in [
        None,
        Some(DocxOp::DeleteText {
            story: StoryId::Body,
            ids: vec![id(100)],
        }),
        Some(DocxOp::DeleteNote {
            kind: NoteKind::Footnote,
            note: id(300),
        }),
        Some(DocxOp::DeleteNote {
            kind: NoteKind::Endnote,
            note: id(400),
        }),
        Some(DocxOp::DeleteTextBox { text_box: id(500) }),
        Some(DocxOp::JoinParagraph {
            story: StoryId::Body,
            mark: id(200),
        }),
        Some(DocxOp::DeleteTable { table: id(600) }),
    ] {
        if let Some(op) = delete {
            model.apply(&op, &context()).unwrap();
        }
        // Serialization/clone must not forget reservations made in hidden flows.
        let model: DocxModel =
            serde_json::from_value(serde_json::to_value(&model).unwrap()).unwrap();
        let before = model.clone();
        for existing in [100, 200, 300, 301, 302, 400, 401, 402, 500, 501, 700] {
            for op in allocated(id(existing)) {
                assert!(
                    model.validate_submission(&op, ReplicaId(1)).is_err(),
                    "reused {existing}: {op:?}"
                );
            }
            // The first ID can be unused while an implicit later atom collides.
            for op in [
                DocxOp::InsertText {
                    story: StoryId::Body,
                    parent: None,
                    first: id(existing - 1),
                    text: "a🙂".into(),
                },
                DocxOp::TrackInsertText {
                    story: StoryId::Body,
                    parent: None,
                    first: id(existing - 1),
                    text: "a🙂".into(),
                    date: None,
                },
                DocxOp::InsertNote {
                    kind: NoteKind::Footnote,
                    story: StoryId::Body,
                    parent: None,
                    id: id(existing - 2),
                    text: "n".into(),
                },
                DocxOp::InsertTextBox {
                    story: StoryId::Body,
                    parent: None,
                    id: id(existing - 1),
                    text: "x".into(),
                    width_px: 100,
                    height_px: 100,
                },
            ] {
                assert!(
                    model.validate_submission(&op, ReplicaId(1)).is_err(),
                    "partial collision: {op:?}"
                );
            }
        }
        assert_eq!(model, before, "admission is pure");
    }
}

#[test]
fn freshness_uses_exact_ids_without_rejecting_unused_old_clock_slots() {
    let mut model = DocxModel::from_paragraphs(&["body"]);
    model
        .apply(
            &DocxOp::InsertText {
                story: StoryId::Body,
                parent: None,
                first: id(1000),
                text: "x".into(),
            },
            &context(),
        )
        .unwrap();
    for first in [id(10), Id::new(1000, ReplicaId(2)), id(u64::MAX)] {
        let op = DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first,
            text: "🙂".into(),
        };
        model.validate_submission(&op, first.replica).unwrap();
    }
    for op in [
        DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: id(u64::MAX),
            text: "a🙂".into(),
        },
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: None,
            id: id(u64::MAX),
            text: String::new(),
        },
        DocxOp::InsertTextBox {
            story: StoryId::Body,
            parent: None,
            id: id(u64::MAX),
            text: "🙂".into(),
            width_px: 100,
            height_px: 100,
        },
    ] {
        assert!(model.validate_submission(&op, ReplicaId(1)).is_err());
    }
    model
        .validate_submission(
            &DocxOp::InsertTextBox {
                story: StoryId::Body,
                parent: None,
                id: id(u64::MAX),
                text: String::new(),
                width_px: 100,
                height_px: 100,
            },
            ReplicaId(1),
        )
        .unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn stateful_atom_reservations_survive_native_deletion_duplicates_and_restore(
        steps in prop::collection::vec((0u8..9,any::<u8>()),1..40),
    ) {
        let (model, jade) = common::open("jade-core/hello.docx");
        let base = model.max_lamport() + 1000;
        let mut seq = Sequencer::with_materializer(model, jade);
        let replica = seq.join().replica;
        let mut retained = Vec::<Id>::new();
        let mut notes = Vec::<(NoteKind,Id)>::new();
        let mut boxes = Vec::<Id>::new();
        let mut ticket = 0;
        // Ensure every history includes a hidden note, checkpoint and changed
        // duplicate, before exploring generated interleavings.
        for (step,&(kind,value)) in [(1,0),(5,0),(4,0),(5,1),(7,0),(5,2),(8,0),(2,0),(6,0),(7,0)]
            .iter().chain(&steps).enumerate()
        {
            let fresh = Id::new(base + step as u64 * 100,replica);
            if kind == 7 {
                let checkpoint = serde_json::from_value(serde_json::to_value(seq.checkpoint()).unwrap()).unwrap();
                let native = JadeDocx::restore(&seq.materializer().export_base().unwrap(),seq.document()).unwrap();
                seq = Sequencer::restore(checkpoint,native);
            } else {
                let mut added = 0;
                let mut new_note = None;
                let mut new_box = None;
                let mut reused = false;
                let op = match kind {
                    1 => {
                        let kind = if value%2==0 {NoteKind::Footnote} else {NoteKind::Endnote};
                        new_note = Some((kind,fresh)); added=4;
                        DocxOp::InsertNote {kind,story:StoryId::Body,parent:None,id:fresh,text:"n🙂".into()}
                    }
                    2 => {
                        new_box=Some(fresh); added=3;
                        DocxOp::InsertTextBox {story:StoryId::Body,parent:None,id:fresh,text:"x🙂".into(),width_px:100,height_px:100}
                    }
                    3 => {added=1; DocxOp::SplitParagraph {story:StoryId::Body,parent:None,id:fresh}}
                    4 if !notes.is_empty() => {
                        let (kind,note)=notes.remove(value as usize%notes.len());
                        DocxOp::DeleteNote {kind,note}
                    }
                    6 if !boxes.is_empty() => {
                        let text_box=boxes.remove(value as usize%boxes.len());
                        DocxOp::DeleteTextBox {text_box}
                    }
                    5 if !retained.is_empty() => {
                        reused=true;
                        let atom=retained[value as usize%retained.len()];
                        let first=Id::new(atom.lamport-u64::from(value%2),atom.replica);
                        let op=DocxOp::TrackInsertText {story:StoryId::Body,parent:None,first,text:"a🙂".into(),date:None};
                        // SDK refusal is atomic before allocating a ticket.
                        let mut local=Replica::new(replica,seq.seq(),seq.document().clone());
                        let before=local.clone();
                        prop_assert!(local.submit(op.clone()).is_err());
                        prop_assert_eq!(local.view(),before.view());
                        prop_assert_eq!(local.next_client_seq(),before.next_client_seq());
                        prop_assert_eq!(local.pending_len(),0);
                        op
                    }
                    _ => {
                        let paragraphs=seq.document().paragraphs();
                        let story=paragraphs[value as usize%paragraphs.len()].story;
                        added=2;
                        DocxOp::InsertText {story,parent:None,first:fresh,text:"a🙂".into()}
                    }
                };
                let before=seq.checkpoint();
                let native_before=seq.materializer().export(seq.document()).unwrap();
                let duplicate=kind==8 && ticket>0;
                if !duplicate {ticket+=1;}
                let outcome=seq.submit(replica,Submission {client_seq:ticket,base_seq:seq.seq(),op});
                if duplicate {
                    prop_assert!(matches!(outcome,SubmitOutcome::Duplicate));
                    prop_assert_eq!(seq.checkpoint(),before);
                } else if reused {
                    prop_assert!(matches!(outcome,SubmitOutcome::Rejected(_)));
                    prop_assert_eq!(seq.document(),&before.document);
                    prop_assert_eq!(seq.seq(),before.seq);
                    prop_assert_eq!(seq.materializer().export(seq.document()).unwrap(),native_before);
                } else {
                    prop_assert!(matches!(outcome,SubmitOutcome::Committed(_)),"{outcome:?}");
                    retained.extend((0..added).map(|k|fresh.offset(k)));
                    if let Some(note)=new_note {notes.push(note);}
                    if let Some(text_box)=new_box {boxes.push(text_box);}
                }
                seq.take_derived();
                prop_assert_eq!(seq.processed_client_seq(replica),ticket);
            }
            prop_assert!(seq.faults().is_empty());
            common::assert_stories_agree(seq.document(),seq.materializer(),"atom identity history");
            let native=JadeDocx::load(&seq.materializer().export(seq.document()).unwrap()).unwrap();
            common::assert_same_order(seq.document(),&native,"exported atom identity history");
            // Independent reservation oracle never forgets an accepted atom,
            // even after its story has vanished from the visible projection.
            for atom in &retained {
                let op=DocxOp::InsertText {story:StoryId::Body,parent:None,first:*atom,text:"x".into()};
                prop_assert!(seq.document().validate_submission(&op,replica).is_err());
            }
        }
    }
}
