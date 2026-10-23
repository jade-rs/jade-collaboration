//! The author prerequisite must not publish before a logically refused edit.
use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_server::wire::{ClientMessage, ServerMessage};
use jade_collab_server::{Server, StorageOptions, Tokens};
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::Document;
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx, StoryId};
use proptest::prelude::*;
use serde_json::json;
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
async fn send(socket: &mut Socket, message: ClientMessage) {
    socket
        .send(Message::Text(
            serde_json::to_string(&message).unwrap().into(),
        ))
        .await
        .unwrap();
}
async fn receive(socket: &mut Socket) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
                return serde_json::from_str(&text).unwrap();
            }
        }
    })
    .await
    .expect("an authored submission must reach a verdict")
}
async fn listen(server: &Server) -> (Socket, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/authored.docx/ws?token=key",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(server.clone().serve(listener));
    (connect_async(url).await.unwrap().0, task)
}
async fn released(root: &Path) {
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .open(root.join(".lock"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match file.try_lock() {
                Ok(()) => {
                    file.unlock().unwrap();
                    break;
                }
                Err(std::fs::TryLockError::WouldBlock) => tokio::task::yield_now().await,
                Err(error) => panic!("storage lease: {error}"),
            }
        }
    })
    .await
    .unwrap();
}
fn operation(kind: u8, fresh: Id, parent: Id) -> DocxOp {
    let missing = Id::new(u64::MAX, ReplicaId::ORIGIN);
    match kind % 6 {
        1 => DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: Some(missing),
            first: fresh,
            text: "x".into(),
            date: None,
        },
        2 => DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: None,
            first: fresh,
            text: String::new(),
            date: None,
        },
        3 => DocxOp::TrackDeleteText {
            story: StoryId::Body,
            ids: vec![missing],
            revision: fresh,
            date: None,
        },
        4 => DocxOp::TrackInsertText {
            story: StoryId::Footnote { id: missing },
            parent: None,
            first: fresh,
            text: "x".into(),
            date: None,
        },
        5 => DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: Some(parent),
            first: Id::new(0, fresh.replica),
            text: "x".into(),
            date: None,
        },
        _ => DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: None,
            first: fresh,
            text: "字🙂".into(),
            date: None,
        },
    }
}

async fn history(
    initial_seq: u64,
    initially_named: bool,
    checkpoint: bool,
    steps: &[(u8, u8)],
    oracle: Option<&Path>,
) {
    let root = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every: if checkpoint { 1 } else { 1000 },
        ..Default::default()
    };
    let tokens = Tokens::from_json(r#"{"key":{"name":"Ada","role":"editor"}}"#).unwrap();
    let mut server = Server::with_storage_options(root.path(), options)
        .unwrap()
        .with_tokens(tokens.clone());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx");
    server
        .open("authored.docx", std::fs::read(fixture).unwrap())
        .unwrap();
    drop(server);
    released(root.path()).await;
    let snapshot = root.path().join("authored.docx/snapshot-1.json");
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&snapshot).unwrap()).unwrap();
    saved["checkpoint"]["seq"] = initial_seq.into();
    if initially_named {
        saved["checkpoint"]["document"]["authors"] = json!([[1, "Ada"]]);
    }
    std::fs::write(snapshot, serde_json::to_vec(&saved).unwrap()).unwrap();
    server = Server::with_storage_options(root.path(), options)
        .unwrap()
        .with_tokens(tokens.clone());
    let (mut socket, mut task) = listen(&server).await;
    send(&mut socket, ClientMessage::Join).await;
    let ServerMessage::Welcome {
        replica, snapshot, ..
    } = receive(&mut socket).await
    else {
        panic!("welcome")
    };
    let initial: DocxModel = serde_json::from_value(snapshot).unwrap();
    let parent = initial.paragraphs()[0].chars[0];
    let mut expected = initial.paragraph_texts()[0].clone();
    let mut named = initially_named;
    let mut seq = initial_seq;
    let mut ticket = 0;
    let mut oracle_input = format!(
        "init {} {initial_seq} {}\n",
        u64::MAX,
        u8::from(initially_named)
    );
    let mut observed = vec![format!(
        "init {initial_seq} 0 {} 0",
        u8::from(initially_named)
    )];
    let original_chars = expected.chars().count();
    // Refusal, changed duplicate, recovery and valid edit occur in every case.
    for (index, &(action, value)) in [(1, 1), (2, 0), (3, 0), (0, 0), (1, 4), (2, 0), (3, 0)]
        .iter()
        .chain(steps)
        .chain(&[(3, 0)])
        .enumerate()
    {
        let before = server.export("authored.docx").await.unwrap();
        let fresh = Id::new(initial.max_lamport() + 10 + index as u64 * 10, replica);
        let verdict = match action % 4 {
            3 => {
                oracle_input.push_str("restore\n");
                socket.close(None).await.unwrap();
                task.abort();
                let _ = task.await;
                drop(server);
                released(root.path()).await;
                server = Server::with_storage_options(root.path(), options)
                    .unwrap()
                    .with_tokens(tokens.clone());
                (socket, task) = listen(&server).await;
                send(&mut socket, ClientMessage::Resume { replica, seq }).await;
                let ServerMessage::Resumed {
                    processed_client_seq,
                    missed,
                } = receive(&mut socket).await
                else {
                    panic!("resume")
                };
                assert_eq!(processed_client_seq, ticket);
                assert!(missed.is_empty());
                assert_eq!(server.export("authored.docx").await.unwrap(), before);
                "restore"
            }
            2 => {
                oracle_input.push_str(&format!("s {ticket} 1 1\n"));
                // A valid changed payload at a refused ticket cannot register an author.
                send(
                    &mut socket,
                    ClientMessage::Submit(Submission {
                        client_seq: ticket,
                        base_seq: seq,
                        op: serde_json::to_value(operation(0, fresh, parent)).unwrap(),
                    }),
                )
                .await;
                "dup"
            }
            action => {
                ticket += 1;
                let kind = if action == 0 { 0 } else { 1 + value % 5 };
                oracle_input.push_str(&format!("s {ticket} {} 1\n", u8::from(kind == 0)));
                let required = if named { 1u128 } else { 2 };
                let accepted = kind == 0 && seq as u128 + required <= u64::MAX as u128;
                send(
                    &mut socket,
                    ClientMessage::Submit(Submission {
                        client_seq: ticket,
                        base_seq: seq,
                        op: serde_json::to_value(operation(kind, fresh, parent)).unwrap(),
                    }),
                )
                .await;
                if accepted {
                    if !named {
                        let ServerMessage::Committed(author) = receive(&mut socket).await else {
                            panic!("author must precede edit")
                        };
                        assert_eq!(author.origin, ReplicaId::ORIGIN);
                        assert_eq!(author.seq, seq + 1);
                    }
                    let ServerMessage::Committed(edit) = receive(&mut socket).await else {
                        panic!("edit")
                    };
                    seq = (seq as u128 + required) as u64;
                    assert_eq!(edit.seq, seq);
                    assert_eq!(edit.client_seq, ticket);
                    named = true;
                    expected.insert_str(0, "字🙂");
                } else {
                    assert!(
                        matches!(receive(&mut socket).await,ServerMessage::Rejected(rejected) if rejected.client_seq==ticket)
                    );
                    assert_eq!(server.export("authored.docx").await.unwrap(), before);
                }
                if accepted {
                    "ok"
                } else {
                    "err"
                }
            }
        };
        send(
            &mut socket,
            ClientMessage::Inspect {
                request: index as u64,
            },
        )
        .await;
        let ServerMessage::Inspected {
            seq: actual_seq,
            snapshot,
            ..
        } = receive(&mut socket).await
        else {
            panic!("no phantom author or duplicate publication")
        };
        let model: DocxModel = serde_json::from_value(snapshot).unwrap();
        assert_eq!(actual_seq, seq);
        assert_eq!(server.seq("authored.docx").await.unwrap(), seq);
        assert_eq!(model.author(replica), named.then_some("Ada"));
        assert_eq!(model.paragraph_texts()[0], expected);
        let inserted = model.paragraph_texts()[0].chars().count() - original_chars;
        assert_eq!(inserted % 2, 0);
        observed.push(format!(
            "{verdict} {actual_seq} {ticket} {} {}",
            u8::from(model.author(replica).is_some()),
            inserted / 2
        ));
        let native = JadeDocx::load(&server.export("authored.docx").await.unwrap()).unwrap();
        let imported = DocxModel::from_word_document(native.word()).unwrap();
        assert_eq!(imported.paragraph_texts(), model.paragraph_texts());
        assert!(imported
            .revisions()
            .iter()
            .all(|r| r.revision.author.as_deref() == Some("Ada")));
        assert!(server.faults("authored.docx").await.unwrap().is_empty());
    }
    socket.close(None).await.unwrap();
    task.abort();
    let _ = task.await;
    if let Some(oracle) = oracle {
        let mut child = Command::new(oracle)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(oracle_input.as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            observed
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn authored_edit_log_capacity_uses_exactly_one_or_two_slots() {
    for seq in [0, u64::MAX - 2, u64::MAX - 1, u64::MAX] {
        for named in [false, true] {
            for checkpoint in [false, true] {
                history(seq, named, checkpoint, &[], None).await;
            }
        }
    }
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn stateful_authored_admission_matches_content_author_log_and_durable_tickets(
        initial_seq in prop_oneof![Just(0), (0u64..12).prop_map(|n|u64::MAX-n)],
        named in any::<bool>(),checkpoint in any::<bool>(),
        steps in prop::collection::vec((0u8..4,any::<u8>()),1..25),
    ) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(history(initial_seq,named,checkpoint,&steps,None));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    #[ignore="requires JADE_LEAN_AUTHORED_ORACLE; see lean/README.md"]
    fn native_authored_histories_match_the_lean_transition_model(
        initial_seq in prop_oneof![Just(0), (0u64..12).prop_map(|n|u64::MAX-n)],
        named in any::<bool>(),checkpoint in any::<bool>(),
        steps in prop::collection::vec((0u8..4,any::<u8>()),1..25),
    ) {
        let oracle=std::env::var("JADE_LEAN_AUTHORED_ORACLE").expect("set JADE_LEAN_AUTHORED_ORACLE");
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(history(initial_seq,named,checkpoint,&steps,Some(Path::new(&oracle))));
    }
}
