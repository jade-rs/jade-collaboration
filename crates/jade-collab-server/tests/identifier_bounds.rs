use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::{Id, IdError, ReplicaId};
use jade_collab_server::wire::{ClientMessage, ServerMessage};
use jade_collab_server::{Client, Format, Server};
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::Document;
use jade_docx_collab::{DocxModel, DocxOp, NoteKind, StoryId};
use jade_pptx_collab::{PptxModel, PptxOp};
use jade_xlsx_collab::{XlsxModel, XlsxOp};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[tokio::test(flavor = "multi_thread")]
async fn invalid_tracked_edits_never_commit_author_registration() {
    use jade_collab_server::Tokens;
    let name = "author-admission.docx";
    let server = Server::new()
        .with_tokens(Tokens::from_json(r#"{"key":{"name":"Ada","role":"editor"}}"#).unwrap());
    server.open(name, fixture("hello.docx")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/{name}/ws?token=key",
        listener.local_addr().unwrap()
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let (mut socket, _) = connect_async(url).await.unwrap();
    send(&mut socket, ClientMessage::Join).await;
    let ServerMessage::Welcome {
        replica, snapshot, ..
    } = receive(&mut socket).await
    else {
        panic!("welcome")
    };
    let model: DocxModel = serde_json::from_value(snapshot).unwrap();
    let first = Id::new(model.max_lamport() + 1, replica);
    let missing = Id::new(u64::MAX, ReplicaId::ORIGIN);
    let before = server.export(name).await.unwrap();
    for (index, op) in [
        DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: Some(missing),
            first,
            text: "x".into(),
            date: None,
        },
        DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: None,
            first,
            text: String::new(),
            date: None,
        },
        DocxOp::TrackDeleteText {
            story: StoryId::Body,
            ids: vec![missing],
            revision: first,
            date: None,
        },
        DocxOp::TrackInsertText {
            story: StoryId::Footnote { id: missing },
            parent: None,
            first,
            text: "x".into(),
            date: None,
        },
        DocxOp::TrackInsertText {
            story: StoryId::Body,
            parent: Some(model.paragraphs()[0].chars[0]),
            first: Id::new(0, replica),
            text: "x".into(),
            date: None,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let ticket = index as u64 + 1;
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: ticket,
                base_seq: 0,
                op: serde_json::to_value(op).unwrap(),
            }),
        )
        .await;
        assert!(
            matches!(receive(&mut socket).await,ServerMessage::Rejected(rejected) if rejected.client_seq==ticket),
            "logical refusal must precede author publication"
        );
        assert_eq!(server.seq(name).await.unwrap(), 0);
        assert_eq!(server.export(name).await.unwrap(), before);
        send(&mut socket, ClientMessage::Inspect { request: ticket }).await;
        let ServerMessage::Inspected { snapshot, .. } = receive(&mut socket).await else {
            panic!("inspection")
        };
        let model: DocxModel = serde_json::from_value(snapshot).unwrap();
        assert_eq!(model.author(replica), None);
    }
    socket.close(None).await.unwrap();
    serving.abort();
    let _ = serving.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owned_note_atom_cannot_be_reused_as_a_body_character() {
    let name = "unique.docx";
    let server = Server::new();
    server.open(name, fixture("hello.docx")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/{name}/ws",
        listener.local_addr().unwrap()
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let (mut socket, _) = connect_async(url).await.unwrap();
    send(&mut socket, ClientMessage::Join).await;
    let ServerMessage::Welcome {
        replica, snapshot, ..
    } = receive(&mut socket).await
    else {
        panic!("welcome")
    };
    let model: DocxModel = serde_json::from_value(snapshot).unwrap();
    let note = Id::new(model.max_lamport() + 1, replica);
    send(
        &mut socket,
        ClientMessage::Submit(Submission {
            client_seq: 1,
            base_seq: 0,
            op: serde_json::to_value(DocxOp::InsertNote {
                kind: NoteKind::Footnote,
                story: StoryId::Body,
                parent: None,
                id: note,
                text: "n".into(),
            })
            .unwrap(),
        }),
    )
    .await;
    // The native writer numbers the new footnote in a derived second record.
    for _ in 0..2 {
        assert!(matches!(
            receive(&mut socket).await,
            ServerMessage::Committed(_)
        ));
    }
    let before = server.export(name).await.unwrap();
    send(
        &mut socket,
        ClientMessage::Submit(Submission {
            client_seq: 2,
            base_seq: 2,
            op: serde_json::to_value(DocxOp::InsertText {
                story: StoryId::Body,
                parent: None,
                first: note.offset(2),
                text: "x".into(),
            })
            .unwrap(),
        }),
    )
    .await;
    assert!(
        matches!(receive(&mut socket).await, ServerMessage::Rejected(_)),
        "an ID already used in a note must be refused before native application"
    );
    assert_eq!(server.seq(name).await.unwrap(), 2);
    assert_eq!(server.export(name).await.unwrap(), before);
    assert!(server.faults(name).await.unwrap().is_empty());
    socket.close(None).await.unwrap();
    serving.abort();
    let _ = serving.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn atom_collision_refusals_are_durable_and_precede_author_registration() {
    use jade_collab_server::{StorageOptions, Tokens};
    use jade_docx_collab::JadeDocx;
    let name = "unique.docx";
    for checkpoint_every in [1, 1000] {
        let root = tempfile::tempdir().unwrap();
        let options = StorageOptions {
            checkpoint_every,
            ..Default::default()
        };
        let tokens = Tokens::from_json(r#"{"key":{"name":"Ada","role":"editor"}}"#).unwrap();
        let mut server = Server::with_storage_options(root.path(), options)
            .unwrap()
            .with_tokens(tokens.clone());
        server.open(name, fixture("hello.docx")).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut url = format!(
            "ws://{}/documents/{name}/ws?token=key",
            listener.local_addr().unwrap()
        );
        let mut serving = tokio::spawn(server.clone().serve(listener));
        let (mut socket, _) = connect_async(&url).await.unwrap();
        send(&mut socket, ClientMessage::Join).await;
        let ServerMessage::Welcome {
            replica, snapshot, ..
        } = receive(&mut socket).await
        else {
            panic!("welcome")
        };
        let model: DocxModel = serde_json::from_value(snapshot).unwrap();
        let note = Id::new(model.max_lamport() + 1, replica);
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 1,
                base_seq: 0,
                op: serde_json::to_value(DocxOp::InsertNote {
                    kind: NoteKind::Footnote,
                    story: StoryId::Body,
                    parent: None,
                    id: note,
                    text: "n".into(),
                })
                .unwrap(),
            }),
        )
        .await;
        for _ in 0..2 {
            assert!(matches!(
                receive(&mut socket).await,
                ServerMessage::Committed(_)
            ));
        }
        let before = server.export(name).await.unwrap();
        let tracked = |first| {
            serde_json::to_value(DocxOp::TrackInsertText {
                story: StoryId::Body,
                parent: None,
                first,
                text: "x".into(),
                date: None,
            })
            .unwrap()
        };
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 2,
                base_seq: 2,
                op: tracked(note.offset(2)),
            }),
        )
        .await;
        assert!(matches!(
            receive(&mut socket).await,
            ServerMessage::Rejected(_)
        ));
        assert_eq!(
            server.seq(name).await.unwrap(),
            2,
            "a refused collision must not register an author"
        );
        assert_eq!(server.export(name).await.unwrap(), before);
        socket.close(None).await.unwrap();
        serving.abort();
        let _ = serving.await;
        drop(server);
        storage_released(root.path()).await;

        server = Server::with_storage_options(root.path(), options)
            .unwrap()
            .with_tokens(tokens);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        url = format!(
            "ws://{}/documents/{name}/ws?token=key",
            listener.local_addr().unwrap()
        );
        serving = tokio::spawn(server.clone().serve(listener));
        (socket, _) = connect_async(url).await.unwrap();
        send(&mut socket, ClientMessage::Resume { replica, seq: 2 }).await;
        let ServerMessage::Resumed {
            processed_client_seq,
            missed,
        } = receive(&mut socket).await
        else {
            panic!("resume")
        };
        assert_eq!(processed_client_seq, 2);
        assert!(missed.is_empty());
        let fresh = note.offset(10);
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 2,
                base_seq: 2,
                op: tracked(fresh),
            }),
        )
        .await;
        send(&mut socket, ClientMessage::Inspect { request: 1 }).await;
        let ServerMessage::Inspected { seq, snapshot, .. } = receive(&mut socket).await else {
            panic!("changed duplicate must remain silent")
        };
        let model: DocxModel = serde_json::from_value(snapshot).unwrap();
        assert_eq!(seq, 2);
        assert_eq!(model.author(replica), None);
        assert_eq!(server.export(name).await.unwrap(), before);
        // A new ticket can still make a valid authored edit after recovery.
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 3,
                base_seq: 2,
                op: tracked(fresh),
            }),
        )
        .await;
        for _ in 0..2 {
            assert!(matches!(
                receive(&mut socket).await,
                ServerMessage::Committed(_)
            ));
        }
        send(&mut socket, ClientMessage::Inspect { request: 2 }).await;
        let ServerMessage::Inspected { seq, snapshot, .. } = receive(&mut socket).await else {
            panic!("inspection")
        };
        let model: DocxModel = serde_json::from_value(snapshot).unwrap();
        assert_eq!(seq, 4);
        assert_eq!(model.author(replica), Some("Ada"));
        let native = JadeDocx::load(&server.export(name).await.unwrap()).unwrap();
        let imported = DocxModel::from_word_document(native.word()).unwrap();
        assert_eq!(imported.paragraph_texts(), model.paragraph_texts());
        assert_eq!(
            imported.revisions()[0].revision.author.as_deref(),
            Some("Ada")
        );
        assert!(server.faults(name).await.unwrap().is_empty());
        socket.close(None).await.unwrap();
        serving.abort();
        let _ = serving.await;
    }
}
// Connection tasks can outlive an aborted listener. Concurrent process spawns
// can also transiently inherit locked handles before exec closes them. Probe
// actual lease release at every shutdown, including the bootstrap checkpoint,
// before editing storage or starting another owner.
async fn storage_released(root: &std::path::Path) {
    let lock = std::fs::File::options()
        .read(true)
        .write(true)
        .open(root.join(".lock"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    lock.unlock().unwrap();
                    break;
                }
                Err(std::fs::TryLockError::WouldBlock) => tokio::task::yield_now().await,
                Err(error) => panic!("storage lease probe: {error}"),
            }
        }
    })
    .await
    .expect("closed connections must release storage ownership");
}

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}
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
    .expect("a malformed identifier must receive an answer, not stop the document")
}
fn operations(format: Format, snapshot: Value, replica: ReplicaId) -> (Vec<Value>, Value) {
    let last = Id::new(u64::MAX, replica);
    match format {
        Format::Docx => {
            let model: DocxModel = serde_json::from_value(snapshot).unwrap();
            let invalid = [
                DocxOp::InsertText {
                    story: StoryId::Body,
                    parent: None,
                    first: last,
                    text: "ab".into(),
                },
                DocxOp::InsertNote {
                    kind: NoteKind::Footnote,
                    story: StoryId::Body,
                    parent: None,
                    id: last,
                    text: "x".into(),
                },
                DocxOp::InsertTextBox {
                    story: StoryId::Body,
                    parent: None,
                    id: last,
                    text: "x".into(),
                    width_px: 100,
                    height_px: 100,
                },
            ]
            .into_iter()
            .map(|op| serde_json::to_value(op).unwrap())
            .collect();
            (
                invalid,
                serde_json::to_value(DocxOp::InsertText {
                    story: StoryId::Body,
                    parent: None,
                    first: Id::new(model.max_lamport() + 1, replica),
                    text: "ok".into(),
                })
                .unwrap(),
            )
        }
        Format::Xlsx => {
            let model: XlsxModel = serde_json::from_value(snapshot).unwrap();
            let sheet = &model.sheets()[0];
            (
                vec![serde_json::to_value(XlsxOp::AddSheet {
                    id: last,
                    name: "New".into(),
                    after: None,
                    rows: 1,
                    columns: 1,
                })
                .unwrap()],
                serde_json::to_value(XlsxOp::SetNote {
                    sheet: sheet.id,
                    row: sheet.rows[0],
                    column: sheet.columns[0],
                    stamp: Id::new(model.max_lamport() + 1, replica),
                    note: Some("ok".into()),
                })
                .unwrap(),
            )
        }
        Format::Pptx => {
            let model: PptxModel = serde_json::from_value(snapshot).unwrap();
            let story = model
                .slides()
                .iter()
                .find_map(|slide| {
                    slide
                        .shapes
                        .iter()
                        .find(|shape| shape.editable)
                        .map(|shape| jade_pptx_collab::StoryId::Shape {
                            slide: slide.id,
                            shape: shape.id,
                        })
                })
                .unwrap();
            (
                vec![serde_json::to_value(PptxOp::InsertText {
                    story,
                    parent: None,
                    first: last,
                    text: "ab".into(),
                })
                .unwrap()],
                serde_json::to_value(PptxOp::InsertText {
                    story,
                    parent: None,
                    first: Id::new(model.max_lamport() + 1, replica),
                    text: "ok".into(),
                })
                .unwrap(),
            )
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn overflowing_peer_operations_leave_every_format_unchanged_and_editable() {
    let server = Server::new();
    for (name, path) in [
        ("hello.docx", "hello.docx"),
        ("book.xlsx", "public/fixtures/grid-operations-workbook.xlsx"),
        ("deck.pptx", "public/fixtures/sample-deck.pptx"),
    ] {
        server.open(name, fixture(path)).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(server.clone().serve(listener));
    for (name, format) in server.documents() {
        let before = server.export(&name).await.unwrap();
        let (mut socket, _) = connect_async(format!("ws://{addr}/documents/{name}/ws"))
            .await
            .unwrap();
        send(&mut socket, ClientMessage::Join).await;
        let ServerMessage::Welcome {
            replica, snapshot, ..
        } = receive(&mut socket).await
        else {
            panic!("welcome");
        };
        let (mut ops, valid) = operations(format, snapshot, replica);
        let invalid = ops.len();
        ops.push(valid);
        for (index, op) in ops.into_iter().enumerate() {
            send(
                &mut socket,
                ClientMessage::Submit(Submission {
                    client_seq: index as u64 + 1,
                    base_seq: 0,
                    op,
                }),
            )
            .await;
            let reply = receive(&mut socket).await;
            if index < invalid {
                assert!(
                    matches!(reply, ServerMessage::Rejected(_)),
                    "{name}: {reply:?}"
                );
                assert_eq!(server.export(&name).await.unwrap(), before);
                assert_eq!(server.seq(&name).await.unwrap(), 0);
            } else {
                assert!(
                    matches!(reply, ServerMessage::Committed(_)),
                    "{name}: {reply:?}"
                );
            }
            assert!(server.faults(&name).await.unwrap().is_empty());
        }
    }
    task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exhausted_clock_is_an_error_in_the_sdk_and_cli_and_reads_still_work() {
    let server = Server::new();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(server.clone().serve(listener));
    let url = format!("ws://{addr}/documents/hello.docx/ws");
    let client: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let last = Id::new(u64::MAX, client.id());
    client
        .submit(DocxOp::InsertText {
            story: StoryId::Body,
            parent: None,
            first: last,
            text: "x".into(),
        })
        .unwrap();
    client.settle().await.unwrap();
    assert_eq!(client.try_reserve(1), Err(IdError::Exhausted));
    assert!(client.view().paragraph_texts()[0].starts_with('x'));
    client
        .submit(DocxOp::DeleteText {
            story: StoryId::Body,
            ids: vec![last],
        })
        .unwrap();
    client.settle().await.unwrap();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_jade-collab"))
            .args([
                "apply",
                &url,
                r#"{"type":"insert_text","paragraph":0,"offset":0,"text":"new"}"#,
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("identifier space exhausted"), "{error}");
    assert!(!error.contains("panicked"), "{error}");
    assert!(server.faults("hello.docx").await.unwrap().is_empty());
    task.abort();
}

async fn exhausted_room<D>(format: Format, path: &str, name: &str)
where
    D: Document + serde::de::DeserializeOwned + serde::Serialize + Send + 'static,
    D::Op: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let root = tempfile::tempdir().unwrap();
    let tokens = || {
        jade_collab_server::Tokens::from_json(r#"{"editor":{"name":"existing","role":"editor"}}"#)
            .unwrap()
    };
    let server = Server::with_storage(root.path()).unwrap();
    server.open(name, fixture(path)).unwrap();
    drop(server);
    storage_released(root.path()).await;
    // Seed a reachable boundary checkpoint without issuing 2^64 operations.
    let snapshot = root.path().join(name).join("snapshot-1.json");
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&snapshot).unwrap()).unwrap();
    saved["checkpoint"]["seq"] = u64::MAX.into();
    saved["checkpoint"]["next_replica"] = (u64::MAX - 1).into();
    saved["checkpoint"]["processed"] = serde_json::json!({"1":7});
    saved["checkpoint"]["owners"] = serde_json::json!({"1":"existing"});
    std::fs::write(&snapshot, serde_json::to_vec(&saved).unwrap()).unwrap();

    let mut retained = None;
    for iteration in 0..2 {
        let server = Server::with_storage(root.path())
            .unwrap()
            .with_tokens(tokens());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}{}",
            listener.local_addr().unwrap(),
            jade_collab_server::document_socket_path(name)
        );
        let serving = tokio::spawn(server.clone().serve(listener));
        let client = if let Some(source) = retained.take() {
            Client::<D>::resume(&url, &source, Some("editor"))
                .await
                .unwrap()
        } else {
            Client::<D>::connect(&url, Some("editor")).await.unwrap()
        };
        assert_eq!(client.id(), ReplicaId(u64::MAX - 1));
        assert_eq!(client.seq(), u64::MAX);
        let before = server.export(name).await.unwrap();
        let refused = Client::<D>::connect(&url, Some("editor"))
            .await
            .err()
            .expect("no id remains");
        assert!(
            refused
                .to_string()
                .contains("replica identifier space exhausted"),
            "{refused}"
        );
        // A known replica continues to resume and obtain durable refusal
        // verdicts; exhaustion must not stop the actor or its current sockets.
        let (mut socket, _) = connect_async(format!("{url}?token=editor")).await.unwrap();
        send(
            &mut socket,
            ClientMessage::Resume {
                replica: ReplicaId(1),
                seq: u64::MAX,
            },
        )
        .await;
        assert!(
            matches!(receive(&mut socket).await, ServerMessage::Resumed { processed_client_seq, .. } if processed_client_seq == 7 + iteration)
        );
        let (_, valid) = operations(
            format,
            serde_json::to_value(client.view()).unwrap(),
            ReplicaId(1),
        );
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 8 + iteration,
                base_seq: u64::MAX,
                op: valid,
            }),
        )
        .await;
        assert!(matches!(
            receive(&mut socket).await,
            ServerMessage::Rejected(_)
        ));
        assert_eq!(server.seq(name).await.unwrap(), u64::MAX);
        assert_eq!(server.export(name).await.unwrap(), before);
        assert!(server.faults(name).await.unwrap().is_empty());
        retained = Some(client.disconnect().await);
        drop(socket);
        serving.abort();
        let _ = serving.await;
        drop(server);
        storage_released(root.path()).await;
    }
}

#[tokio::test]
async fn exhausted_native_rooms_refuse_new_ids_and_edits_but_resume_and_export_after_restart() {
    tokio::time::timeout(Duration::from_secs(30), async {
        exhausted_room::<DocxModel>(Format::Docx, "hello.docx", "bounded.docx").await;
        exhausted_room::<XlsxModel>(
            Format::Xlsx,
            "public/fixtures/grid-operations-workbook.xlsx",
            "bounded.xlsx",
        )
        .await;
        exhausted_room::<PptxModel>(
            Format::Pptx,
            "public/fixtures/sample-deck.pptx",
            "bounded.pptx",
        )
        .await;
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_cannot_mint_ids_in_the_origin_or_another_replicas_namespace() {
    for (name, path) in [
        ("mint.docx", "hello.docx"),
        ("mint.xlsx", "public/fixtures/grid-operations-workbook.xlsx"),
        ("mint.pptx", "public/fixtures/sample-deck.pptx"),
    ] {
        let server = Server::new();
        server.open(name, fixture(path)).unwrap();
        let before = server.export(name).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}{}/ws",
            listener.local_addr().unwrap(),
            jade_collab_server::document_path(name)
        );
        let serving = tokio::spawn(server.clone().serve(listener));
        let (mut attacker, _) = connect_async(&url).await.unwrap();
        send(&mut attacker, ClientMessage::Join).await;
        let ServerMessage::Welcome {
            replica: author,
            format,
            snapshot,
            ..
        } = receive(&mut attacker).await
        else {
            panic!("welcome")
        };
        let (mut victim, _) = connect_async(&url).await.unwrap();
        send(&mut victim, ClientMessage::Join).await;
        let ServerMessage::Welcome {
            replica: victim_id, ..
        } = receive(&mut victim).await
        else {
            panic!("welcome")
        };
        for (index, foreign) in [ReplicaId::ORIGIN, victim_id, ReplicaId(999)]
            .into_iter()
            .enumerate()
        {
            let forged = operations(format, snapshot.clone(), foreign).1;
            send(
                &mut attacker,
                ClientMessage::Submit(Submission {
                    client_seq: index as u64 + 1,
                    base_seq: 0,
                    op: forged,
                }),
            )
            .await;
            assert!(
                matches!(receive(&mut attacker).await, ServerMessage::Rejected(_)),
                "{format:?}: a client must mint only its own IDs (forged {foreign})"
            );
            assert_eq!(server.seq(name).await.unwrap(), 0);
            assert_eq!(server.export(name).await.unwrap(), before);
        }
        let own = operations(format, snapshot.clone(), author).1;
        send(
            &mut attacker,
            ClientMessage::Submit(Submission {
                client_seq: 4,
                base_seq: 0,
                op: own,
            }),
        )
        .await;
        assert!(matches!(
            receive(&mut attacker).await,
            ServerMessage::Committed(_)
        ));
        assert!(matches!(
            receive(&mut victim).await,
            ServerMessage::Committed(_)
        ));
        let own = operations(format, snapshot, victim_id).1;
        send(
            &mut victim,
            ClientMessage::Submit(Submission {
                client_seq: 1,
                base_seq: 1,
                op: own,
            }),
        )
        .await;
        assert!(matches!(
            receive(&mut victim).await,
            ServerMessage::Committed(_)
        ));
        assert_eq!(server.seq(name).await.unwrap(), 2);
        assert!(server.faults(name).await.unwrap().is_empty());
        attacker.close(None).await.unwrap();
        victim.close(None).await.unwrap();
        serving.abort();
        let _ = serving.await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn namespace_refusals_survive_restart_without_author_registration_or_duplicate_side_effects()
{
    use jade_collab_server::{StorageOptions, Tokens};
    for checkpoint_every in [1, 1000] {
        let root = tempfile::tempdir().unwrap();
        let options = StorageOptions {
            checkpoint_every,
            ..Default::default()
        };
        let tokens = Tokens::from_json(r#"{"key":{"name":"Ada","role":"editor"}}"#).unwrap();
        let server = Server::with_storage_options(root.path(), options)
            .unwrap()
            .with_tokens(tokens.clone());
        server.open("tracked.docx", fixture("hello.docx")).unwrap();
        let before = server.export("tracked.docx").await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}/documents/tracked.docx/ws?token=key",
            listener.local_addr().unwrap()
        );
        let serving = tokio::spawn(server.clone().serve(listener));
        let (mut socket, _) = connect_async(url).await.unwrap();
        send(&mut socket, ClientMessage::Join).await;
        let ServerMessage::Welcome {
            replica, snapshot, ..
        } = receive(&mut socket).await
        else {
            panic!("welcome")
        };
        let model: DocxModel = serde_json::from_value(snapshot).unwrap();
        let first = Id::new(model.max_lamport() + 1, replica);
        let tracked = |first| {
            serde_json::to_value(DocxOp::TrackInsertText {
                story: StoryId::Body,
                parent: None,
                first,
                text: "x".into(),
                date: None,
            })
            .unwrap()
        };
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 1,
                base_seq: 0,
                op: tracked(Id {
                    replica: ReplicaId::ORIGIN,
                    ..first
                }),
            }),
        )
        .await;
        assert!(
            matches!(receive(&mut socket).await, ServerMessage::Rejected(_)),
            "no author commit may precede refusal"
        );
        assert_eq!(server.seq("tracked.docx").await.unwrap(), 0);
        assert_eq!(server.export("tracked.docx").await.unwrap(), before);
        socket.close(None).await.unwrap();
        serving.abort();
        let _ = serving.await;
        drop(server);
        storage_released(root.path()).await;
        let server = Server::with_storage_options(root.path(), options)
            .unwrap()
            .with_tokens(tokens);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}/documents/tracked.docx/ws?token=key",
            listener.local_addr().unwrap()
        );
        let serving = tokio::spawn(server.clone().serve(listener));
        let (mut socket, _) = connect_async(&url).await.unwrap();
        send(&mut socket, ClientMessage::Resume { replica, seq: 0 }).await;
        let ServerMessage::Resumed {
            processed_client_seq,
            missed,
        } = receive(&mut socket).await
        else {
            panic!("resumed")
        };
        assert_eq!(processed_client_seq, 1);
        assert!(missed.is_empty());
        assert_eq!(server.export("tracked.docx").await.unwrap(), before);
        // A changed payload cannot revive the refused ticket or register its
        // author. Inspect is an ordered barrier after this duplicate frame.
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 1,
                base_seq: 0,
                op: tracked(first),
            }),
        )
        .await;
        send(&mut socket, ClientMessage::Inspect { request: 1 }).await;
        let ServerMessage::Inspected { seq, snapshot, .. } = receive(&mut socket).await else {
            panic!("duplicate must not produce an author commit")
        };
        assert_eq!(seq, 0);
        let model: DocxModel = serde_json::from_value(snapshot).unwrap();
        assert_eq!(model.author(replica), None);
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 2,
                base_seq: 0,
                op: tracked(first),
            }),
        )
        .await;
        for _ in 0..2 {
            assert!(matches!(
                receive(&mut socket).await,
                ServerMessage::Committed(_)
            ));
        }
        assert_eq!(server.seq("tracked.docx").await.unwrap(), 2);
        assert!(server.faults("tracked.docx").await.unwrap().is_empty());
        let reader: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
        assert_eq!(reader.view().author(replica), Some("Ada"));
        assert!(reader.view().paragraph_texts()[0].starts_with('x'));
        reader.disconnect().await;
        socket.close(None).await.unwrap();
        serving.abort();
        let _ = serving.await;
    }
}
