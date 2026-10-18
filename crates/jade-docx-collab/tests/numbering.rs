//! jade numbers what it creates — comments now, notes too — itself, reusing
//! freed numbers. The server commits the number jade gave as a derived
//! operation right after the operation that made it, so the model (every
//! replica's, and the checkpoint's) knows it, and a server restored from a
//! checkpoint and the file it exported still addresses everything.

mod common;

use common::{assert_comments_agree, assert_stories_agree, open};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::{ApplyContext, Document, Sequencer, SubmitOutcome};
use jade_docx_collab::{CommentContent, DocxModel, DocxOp, JadeDocx, NoteKind, Numbered, StoryId};

fn submit(
    server: &mut Sequencer<DocxModel, JadeDocx>,
    from: ReplicaId,
    client_seq: u64,
    op: DocxOp,
) -> SubmitOutcome<DocxOp> {
    let base_seq = server.seq();
    server.submit(
        from,
        Submission {
            client_seq,
            base_seq,
            op,
        },
    )
}

/// The server, stopped and brought back from its checkpoint and the file
/// jade wrote at that point, as a durable server restarts.
fn restart(server: &Sequencer<DocxModel, JadeDocx>) -> Sequencer<DocxModel, JadeDocx> {
    let bytes = server.materializer().export().unwrap();
    Sequencer::restore(server.checkpoint(), JadeDocx::load(&bytes).unwrap())
}

#[test]
fn a_new_comment_is_numbered_by_a_derived_operation() {
    let (model, jade) = open("jade-core/hello.docx");
    let p = model.paragraphs();
    let mut server = Sequencer::with_materializer(model, jade);
    let replica = server.join().replica;
    let root = Id::new(1_000, replica);
    let outcome = submit(
        &mut server,
        replica,
        1,
        DocxOp::AddComment {
            story: StoryId::Body,
            start: p[0].chars[0],
            end: p[0].chars[2],
            id: root,
            content: CommentContent::new("why?"),
        },
    );
    assert!(
        matches!(outcome, SubmitOutcome::Committed(_)),
        "{outcome:?}"
    );
    let derived = server.take_derived();
    assert_eq!(derived.len(), 1);
    let jade_id = server
        .materializer()
        .word()
        .comments
        .keys()
        .next()
        .unwrap()
        .clone();
    assert_eq!(derived[0].origin, ReplicaId::ORIGIN);
    assert_eq!(derived[0].seq, 2, "right after the comment");
    assert_eq!(
        derived[0].op,
        DocxOp::Numbered {
            item: Numbered::Comment { id: root },
            file_id: jade_id.clone(),
        }
    );
    let view = &server.document().comments()[0];
    assert_eq!(view.file_id.as_deref(), Some(jade_id.as_str()));
    assert!(server.faults().is_empty(), "{:?}", server.faults());
}

#[test]
fn a_restored_server_still_addresses_the_comments_made_before() {
    let (model, jade) = open("jade-core/hello.docx");
    let p = model.paragraphs();
    let mut server = Sequencer::with_materializer(model, jade);
    let replica = server.join().replica;
    let (root, reply, late) = (
        Id::new(1_000, replica),
        Id::new(1_001, replica),
        Id::new(1_002, replica),
    );
    submit(
        &mut server,
        replica,
        1,
        DocxOp::AddComment {
            story: StoryId::Body,
            start: p[0].chars[0],
            end: p[0].chars[2],
            id: root,
            content: CommentContent::new("why?"),
        },
    );
    submit(
        &mut server,
        replica,
        2,
        DocxOp::ReplyToComment {
            comment: root,
            id: reply,
            content: CommentContent::new("because"),
        },
    );
    let mut server = restart(&server);
    for (client_seq, op) in [
        (
            3,
            DocxOp::ReplyToComment {
                comment: reply,
                id: late,
                content: CommentContent::new("after the restart"),
            },
        ),
        (
            4,
            DocxOp::UpdateComment {
                comment: root,
                text: "edited".into(),
            },
        ),
        (
            5,
            DocxOp::ResolveComment {
                comment: root,
                resolved: true,
            },
        ),
        (6, DocxOp::DeleteComment { comment: reply }),
    ] {
        let outcome = submit(&mut server, replica, client_seq, op.clone());
        assert!(
            matches!(outcome, SubmitOutcome::Committed(_)),
            "{op:?}: {outcome:?}"
        );
    }
    assert!(server.faults().is_empty(), "{:?}", server.faults());
    assert_comments_agree(
        server.document(),
        server.materializer(),
        "after the restart",
    );
    assert_stories_agree(
        server.document(),
        server.materializer(),
        "after the restart",
    );
    assert_eq!(server.document().comments().len(), 2);
}

#[test]
fn only_the_server_numbers() {
    let mut model = DocxModel::from_paragraphs(&["abc"]);
    let c = model.paragraphs()[0].chars.clone();
    let comment = Id::new(100, ReplicaId(1));
    let ctx = |origin| ApplyContext {
        origin,
        base_seq: 0,
        seq: Some(1),
    };
    model
        .apply(
            &DocxOp::AddComment {
                story: StoryId::Body,
                start: c[0],
                end: c[1],
                id: comment,
                content: CommentContent::new("x"),
            },
            &ctx(ReplicaId(1)),
        )
        .unwrap();
    let number = |id| DocxOp::Numbered {
        item: Numbered::Comment { id },
        file_id: "4".into(),
    };
    let before = model.clone();
    assert!(model.apply(&number(comment), &ctx(ReplicaId(1))).is_err());
    assert!(model
        .apply(&number(Id::new(7, ReplicaId(1))), &ctx(ReplicaId::ORIGIN))
        .is_err());
    assert_eq!(model, before);
    model
        .apply(&number(comment), &ctx(ReplicaId::ORIGIN))
        .unwrap();
    assert_eq!(model.comments()[0].file_id.as_deref(), Some("4"));
    assert_eq!(
        DocxModel::lane(&number(comment)),
        jade_collab_sync::Lane::Sequenced
    );
}

#[test]
fn a_restored_server_still_addresses_the_notes_made_before() {
    let (model, jade) = open("jade-core/hello.docx");
    let p = model.paragraphs();
    let mut server = Sequencer::with_materializer(model, jade);
    let replica = server.join().replica;
    let (footnote, endnote) = (Id::new(1_000, replica), Id::new(1_100, replica));
    for (client_seq, kind, id) in [
        (1, NoteKind::Footnote, footnote),
        (2, NoteKind::Endnote, endnote),
    ] {
        let outcome = submit(
            &mut server,
            replica,
            client_seq,
            DocxOp::InsertNote {
                kind,
                story: StoryId::Body,
                parent: Some(p[0].chars[1]),
                id,
                text: "note".into(),
            },
        );
        assert!(
            matches!(outcome, SubmitOutcome::Committed(_)),
            "{outcome:?}"
        );
        let derived = server.take_derived();
        assert_eq!(derived.len(), 1, "{derived:?}");
        assert!(matches!(
            derived[0].op,
            DocxOp::Numbered {
                item: Numbered::Footnote { .. } | Numbered::Endnote { .. },
                ..
            }
        ));
    }
    let mut server = restart(&server);
    for (client_seq, op) in [
        (
            3,
            DocxOp::InsertText {
                story: StoryId::Footnote { id: footnote },
                parent: Some(footnote.offset(5)),
                first: Id::new(2_000, replica),
                text: " typed".into(),
            },
        ),
        (
            4,
            DocxOp::InsertText {
                story: StoryId::Endnote { id: endnote },
                parent: None,
                first: Id::new(2_100, replica),
                text: "E".into(),
            },
        ),
        (
            5,
            DocxOp::DeleteNote {
                kind: NoteKind::Footnote,
                note: footnote,
            },
        ),
    ] {
        let outcome = submit(&mut server, replica, client_seq, op.clone());
        assert!(
            matches!(outcome, SubmitOutcome::Committed(_)),
            "{op:?}: {outcome:?}"
        );
        assert_stories_agree(server.document(), server.materializer(), &format!("{op:?}"));
    }
    assert!(server.faults().is_empty(), "{:?}", server.faults());
    assert_eq!(server.document().paragraph_texts()[2], "E\tnote");
}
