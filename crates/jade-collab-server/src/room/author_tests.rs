//! Author registration is a prerequisite for authenticated tracked edits.
use super::*;
use crate::outbox::outbox;
use jade_collab_core::id::Id;
use jade_collab_sync::Document;
use jade_docx_collab::StoryId;
use std::time::Duration;

async fn read(queue: &mut tokio::sync::mpsc::Receiver<String>) -> ServerMessage {
    let wire = tokio::time::timeout(Duration::from_secs(3), queue.recv())
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&wire).unwrap()
}
async fn position(actor: &Actor) -> u64 {
    let (reply, answer) = oneshot::channel();
    actor.commands.send(Command::Seq { reply }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), answer)
        .await
        .unwrap()
        .unwrap()
}
async fn exported(actor: &Actor) -> Vec<u8> {
    let (reply, answer) = oneshot::channel();
    actor
        .commands
        .send(Command::Export { reply })
        .await
        .unwrap();
    answer.await.unwrap().unwrap()
}
async fn join(
    actor: &Actor,
) -> (
    ReplicaId,
    Outbox,
    tokio::sync::mpsc::Receiver<String>,
    DocxModel,
) {
    let (outbox, mut queue, _) = outbox(16);
    let (reply, answer) = oneshot::channel();
    actor
        .commands
        .send(Command::Join {
            principal: Some("Ada".into()),
            outbox: outbox.clone(),
            reply,
        })
        .await
        .unwrap();
    let replica = answer.await.unwrap().unwrap();
    let ServerMessage::Welcome { snapshot, .. } = read(&mut queue).await else {
        panic!("welcome expected")
    };
    (
        replica,
        outbox,
        queue,
        serde_json::from_value(snapshot).unwrap(),
    )
}
async fn send(
    actor: &Actor,
    replica: ReplicaId,
    outbox: &Outbox,
    ticket: u64,
    first: Id,
    author: &str,
) {
    let op = DocxOp::TrackInsertText {
        story: StoryId::Body,
        parent: None,
        first,
        text: "X".into(),
        date: None,
    };
    actor
        .commands
        .send(Command::Submit {
            replica,
            outbox: outbox.clone(),
            author: Some(author.into()),
            refusal: None,
            submission: Submission {
                client_seq: ticket,
                base_seq: 0,
                op: serde_json::to_value(op).unwrap(),
            },
        })
        .await
        .unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn author_registration_refusal_is_durable_and_cannot_commit_an_unattributed_edit() {
    for bad in ["", " \t\r\n", "bad\0name", "bad\u{fffe}name"] {
        for checkpoint_every in [1, 1000] {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("authors.docx");
            let options = StorageOptions {
                checkpoint_every,
                ..Default::default()
            };
            let bytes = std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx"),
            )
            .unwrap();
            let actor = spawn(
                "authors.docx",
                Format::Docx,
                Source::NewStored {
                    bytes,
                    dir: dir.clone(),
                    options,
                },
            )
            .unwrap();
            let (replica, outbox, mut queue, model) = join(&actor).await;
            let before = exported(&actor).await;
            let first = Id::new(model.max_lamport() + 1, replica);
            send(&actor, replica, &outbox, 1, first, bad).await;
            assert!(
                matches!(read(&mut queue).await,ServerMessage::Rejected(rejected) if rejected.client_seq==1),
                "author failure must be a refusal, never an unattributed commit: {bad:?}"
            );
            assert_eq!(position(&actor).await, 0);
            assert_eq!(exported(&actor).await, before);
            drop(actor);

            let actor = spawn(
                "authors.docx",
                Format::Docx,
                Source::Stored { dir, options },
            )
            .unwrap();
            let (outbox, mut queue, _) = crate::outbox::outbox(16);
            let (reply, answer) = oneshot::channel();
            actor
                .commands
                .send(Command::Resume {
                    principal: Some("Ada".into()),
                    replica,
                    seq: 0,
                    outbox: outbox.clone(),
                    reply,
                })
                .await
                .unwrap();
            answer.await.unwrap().unwrap();
            assert!(
                matches!(read(&mut queue).await,ServerMessage::Resumed {processed_client_seq:1,missed} if missed.is_empty())
            );
            assert_eq!(exported(&actor).await, before);
            // Correcting the author cannot revive an already refused ticket.
            send(&actor, replica, &outbox, 1, first, "Ada").await;
            assert_eq!(position(&actor).await, 0);
            assert!(queue.try_recv().is_err());
            send(
                &actor,
                replica,
                &outbox,
                2,
                Id::new(first.lamport + 1, replica),
                "Ada",
            )
            .await;
            let ServerMessage::Committed(author) = read(&mut queue).await else {
                panic!("author expected")
            };
            assert_eq!(author.origin, ReplicaId::ORIGIN);
            let ServerMessage::Committed(edit) = read(&mut queue).await else {
                panic!("edit expected")
            };
            assert_eq!(edit.client_seq, 2);
            assert_eq!(position(&actor).await, 2);
            let file = exported(&actor).await;
            let jade = JadeDocx::load(&file).unwrap();
            let reloaded = DocxModel::from_word_document(jade.word()).unwrap();
            assert!(reloaded
                .revisions()
                .iter()
                .any(|revision| revision.text == "X"
                    && revision.revision.author.as_deref() == Some("Ada")));
        }
    }
}
