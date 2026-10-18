//! Notes, text boxes and comment editing meet tracked changes: tracked text
//! typed where anchors sit, tracked deletions inside notes and text boxes
//! that are then deleted, notes made while tracking is on, comments edited
//! on tracked text, and the server's two kinds of origin operations — the
//! numbers jade gives (derived) and the authors it names — surviving a
//! restart together.

mod common;

use common::{
    assert_comments_agree, assert_revisions_agree, assert_stories_agree, commit, model_marks, open,
};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::{Document, Sequencer, SubmitOutcome};
use jade_docx_collab::intent::{resolve_with, Context, IntentOp};
use jade_docx_collab::{CommentContent, DocxModel, DocxOp, JadeDocx, NoteKind, StoryId};

fn layout(model: &DocxModel, paragraph: usize) -> String {
    let p = &model.paragraphs()[paragraph];
    let mut out = String::new();
    let mut at = 0;
    for anchor in &p.anchors {
        out.push_str(&p.text[at..anchor.offset]);
        at = anchor.offset;
        out.push_str(&match anchor.story {
            StoryId::TextBox { .. } => "[box]".to_string(),
            story => format!("[{}]", model.note_id(story).unwrap()),
        });
    }
    out.push_str(&p.text[at..]);
    out
}

fn reloaded_layouts(model: &DocxModel, jade: &JadeDocx) -> (Vec<String>, Vec<String>) {
    let again = JadeDocx::load(&jade.export(model).unwrap()).unwrap();
    let again_model = DocxModel::from_word_document(again.word()).unwrap();
    let all = |m: &DocxModel| (0..m.paragraphs().len()).map(|p| layout(m, p)).collect();
    (all(&again_model), all(model))
}

fn tracked(model: &mut DocxModel, jade: &mut JadeDocx, on: bool) {
    commit(model, jade, DocxOp::SetTracking { on });
}

/// Tracked text typed after a character a deletion removed lands between
/// two note references; jade's typing cannot put it there, and its
/// insert_text_among_anchors writes plain text, so the materializer types
/// the tracked insertion right after a placeholder there.
#[test]
fn tracked_text_between_two_anchors_is_written_as_an_insertion() {
    let (mut model, mut jade) = open("footnote.docx");
    let p = model.paragraphs();
    let (l, o) = (p[0].chars[3], p[0].chars[4]);
    // "Hell" ⁿ "o" ¹.
    commit(
        &mut model,
        &mut jade,
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: Some(l),
            id: Id::new(1_000, ReplicaId(1)),
            text: "n".into(),
        },
    );
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteText {
            story: StoryId::Body,
            ids: vec![o],
        },
    );
    let first = Id::new(model.max_lamport() + 1, ReplicaId(2));
    commit(
        &mut model,
        &mut jade,
        DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: Some(o),
            first,
            text: "x".into(),
            date: None,
        },
    );
    let number = model
        .note_id(StoryId::Footnote {
            id: Id::new(1_000, ReplicaId(1)),
        })
        .unwrap()
        .to_string();
    assert_eq!(layout(&model, 0), format!("Hell[{number}]x[1]"));
    assert!(model_marks(&model)[0][4].0.is_some(), "x is an insertion");
    let (again, ours) = reloaded_layouts(&model, &jade);
    assert_eq!(again, ours);
}

/// A note made while tracking is on is not a revision (neither model nor
/// jade tracks a note's creation); typing into it is.
#[test]
fn a_note_made_while_tracking_takes_tracked_text() {
    let (mut model, mut jade) = open("jade-core/hello.docx");
    tracked(&mut model, &mut jade, true);
    let ctx = Context {
        replica: Some(ReplicaId(1)),
        date: None,
    };
    let mut next = model.max_lamport() + 1;
    let mut ids = |n: u64| {
        let id = Id::new(next, ReplicaId(1));
        next += n;
        id
    };
    for intent in [
        IntentOp::InsertFootnote {
            paragraph: 1,
            offset: 2,
            text: "note".into(),
        },
        IntentOp::InsertText {
            paragraph: 2,
            offset: 5,
            text: " more".into(),
        },
    ] {
        for op in resolve_with(&model, &intent, &ctx, &mut ids).unwrap() {
            commit(&mut model, &mut jade, op);
        }
    }
    assert_eq!(model.paragraph_texts()[2], "\tnote more");
    let marks = &model_marks(&model)[2];
    assert!(marks[..5].iter().all(|(i, d)| i.is_none() && d.is_none()));
    assert!(marks[5..].iter().all(|(i, _)| i.is_some()));
    assert_eq!(model.revisions().len(), 1);
    let (again, ours) = reloaded_layouts(&model, &jade);
    assert_eq!(again, ours);
}

/// Tracked deletions inside a note and a text box go with the story when it
/// is deleted: they are no longer listed, and jade agrees.
#[test]
fn deleting_a_note_or_text_box_takes_its_tracked_changes() {
    let (mut model, mut jade) = open("footnote-table-textbox-order.docx");
    let p = model.paragraphs();
    let note = p
        .iter()
        .find(|q| q.text == " Note for Host")
        .unwrap()
        .clone();
    let in_box = p.iter().find(|q| q.text == "Box text").unwrap().clone();
    for (story, chars, revision) in [
        (note.story, note.chars[1..3].to_vec(), 9_000),
        (in_box.story, in_box.chars[0..2].to_vec(), 9_001),
    ] {
        commit(
            &mut model,
            &mut jade,
            DocxOp::TrackDeleteText {
                story,
                ids: chars,
                revision: Id::new(revision, ReplicaId(1)),
                date: None,
            },
        );
    }
    assert_eq!(model.revisions().len(), 2);
    let StoryId::Footnote { id } = note.story else {
        panic!()
    };
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteNote {
            kind: NoteKind::Footnote,
            note: id,
        },
    );
    let StoryId::TextBox { id } = in_box.story else {
        panic!()
    };
    commit(
        &mut model,
        &mut jade,
        DocxOp::DeleteTextBox { text_box: id },
    );
    assert!(model.revisions().is_empty(), "{:?}", model.revisions());
    let (again, ours) = reloaded_layouts(&model, &jade);
    assert_eq!(again, ours);
}

/// A comment's text is edited while the text it is on is deleted as a
/// tracked change.
#[test]
fn a_comment_on_tracked_deleted_text_is_edited() {
    let (mut model, mut jade) = open("comment.docx");
    let p = model.paragraphs();
    commit(
        &mut model,
        &mut jade,
        DocxOp::TrackDeleteText {
            story: StoryId::Body,
            ids: p[0].chars[6..].to_vec(),
            revision: Id::new(9_000, ReplicaId(1)),
            date: None,
        },
    );
    let root = model.comments()[0].id;
    commit(
        &mut model,
        &mut jade,
        DocxOp::UpdateComment {
            comment: root,
            text: "on deleted text".into(),
        },
    );
    commit(
        &mut model,
        &mut jade,
        DocxOp::RejectRevisions {
            revisions: vec![Id::new(9_000, ReplicaId(1))],
        },
    );
    assert_eq!(model.comments()[0].content.text, "on deleted text");
}

/// The server's origin operations of both kinds — an author it names
/// (`SetAuthor`) and a number jade gave (`Numbered`, derived) — and the
/// tracked text and notes they concern survive a restart from the
/// checkpoint and the base file.
#[test]
fn authors_and_numbers_survive_a_restart_together() {
    let (model, jade) = open("jade-core/hello.docx");
    let p = model.paragraphs();
    let mut server = Sequencer::with_materializer(model, jade);
    let replica = server.join().replica;
    server
        .commit_origin(DocxOp::SetAuthor {
            replica,
            name: "Ann".into(),
        })
        .unwrap();
    let mut client_seq = 0;
    let mut submit = |server: &mut Sequencer<DocxModel, JadeDocx>, op: DocxOp| {
        client_seq += 1;
        let base_seq = server.seq();
        let outcome = server.submit(
            replica,
            Submission {
                client_seq,
                base_seq,
                op: op.clone(),
            },
        );
        assert!(
            matches!(outcome, SubmitOutcome::Committed(_)),
            "{op:?}: {outcome:?}"
        );
        server.take_derived()
    };
    submit(&mut server, DocxOp::SetTracking { on: true });
    let note = Id::new(1_000, replica);
    let derived = submit(
        &mut server,
        DocxOp::InsertNote {
            kind: NoteKind::Endnote,
            story: StoryId::Body,
            parent: Some(p[0].chars[1]),
            id: note,
            text: "e".into(),
        },
    );
    assert_eq!(derived.len(), 1);
    submit(
        &mut server,
        DocxOp::TrackInsertText {
            story: StoryId::Endnote { id: note },
            parent: Some(note.offset(2)),
            first: Id::new(1_100, replica),
            text: "!".into(),
            date: None,
        },
    );
    let comment = Id::new(1_200, replica);
    submit(
        &mut server,
        DocxOp::AddComment {
            story: StoryId::Body,
            start: p[1].chars[0],
            end: p[1].chars[1],
            id: comment,
            content: CommentContent::new("c"),
        },
    );
    let bytes = server.materializer().export_base().unwrap();
    let mut server = Sequencer::restore(server.checkpoint(), JadeDocx::load(&bytes).unwrap());
    submit(
        &mut server,
        DocxOp::UpdateComment {
            comment,
            text: "after".into(),
        },
    );
    submit(
        &mut server,
        DocxOp::TrackInsertText {
            story: StoryId::Endnote { id: note },
            parent: Some(Id::new(1_100, replica)),
            first: Id::new(1_300, replica),
            text: "?".into(),
            date: None,
        },
    );
    assert!(server.faults().is_empty(), "{:?}", server.faults());
    let (model, jade) = (server.document(), server.materializer());
    assert_stories_agree(model, jade, "after the restart");
    assert_comments_agree(model, jade, "after the restart");
    assert_revisions_agree(model, jade, "after the restart");
    let authors: Vec<_> = model
        .revisions()
        .iter()
        .map(|r| r.revision.author.clone())
        .collect();
    assert!(!authors.is_empty());
    assert!(
        authors.iter().all(|a| a.as_deref() == Some("Ann")),
        "{authors:?}"
    );
}

/// A note placed after a reference that follows tracked-deleted text: the
/// placeholder the materializer types there took the deletion from the
/// text before it (jade-core's insert_text_among_anchors copied the run's
/// revision; fixed in jade-rs/jade-core#45) and stayed in the file.
#[test]
fn a_note_after_a_reference_behind_tracked_deleted_text() {
    let (mut model, mut jade) = open("footnote.docx");
    let p = model.paragraphs();
    let reference = p[0].anchors[0].id;
    commit(
        &mut model,
        &mut jade,
        DocxOp::TrackDeleteText {
            story: StoryId::Body,
            ids: vec![p[0].chars[4]],
            revision: Id::new(9_000, ReplicaId(1)),
            date: None,
        },
    );
    commit(
        &mut model,
        &mut jade,
        DocxOp::InsertNote {
            kind: NoteKind::Footnote,
            story: StoryId::Body,
            parent: Some(reference),
            id: Id::new(10_000, ReplicaId(1)),
            text: "n".into(),
        },
    );
    let number = model
        .note_id(StoryId::Footnote {
            id: Id::new(10_000, ReplicaId(1)),
        })
        .unwrap()
        .to_string();
    assert_eq!(layout(&model, 0), format!("Hello[1][{number}]"));
    let (again, ours) = reloaded_layouts(&model, &jade);
    assert_eq!(again, ours);
}
