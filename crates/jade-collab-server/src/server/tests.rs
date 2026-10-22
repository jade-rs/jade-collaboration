use super::*;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(name),
    )
    .unwrap()
}

#[test]
fn a_blocked_document_open_does_not_hold_up_other_documents_or_name_checks() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::with_storage(root.path()).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (entered, ready) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let opening = server.clone();
    let dir = root.path().join("slow.docx");
    let worker = std::thread::spawn(move || {
        opening.start_with("slow.docx", Format::Docx, || {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            room::spawn(
                "slow.docx",
                Format::Docx,
                Source::NewStored {
                    bytes: fixture("hello.docx"),
                    dir,
                    options: StorageOptions::default(),
                },
            )
        })
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    let other = server.clone();
    let unexpected = root.path().join("losing-upload.docx");
    let (finished, done) = mpsc::channel();
    let probe = std::thread::spawn(move || {
        assert_eq!(other.documents(), vec![("hello.docx".into(), Format::Docx)]);
        assert!(
            matches!(other.commands("slow.docx"), Err(ServerError::NotFound(_))),
            "an opening room is not yet public"
        );
        assert!(matches!(
            other.start(
                "slow.docx",
                Format::Docx,
                Source::NewStored {
                    bytes: fixture("hello.docx"),
                    dir: unexpected.clone(),
                    options: StorageOptions::default()
                }
            ),
            Err(ServerError::Exists(_))
        ));
        assert!(
            !unexpected.exists(),
            "a competing claim must not initialize storage"
        );
        other
            .open(
                "other.xlsx",
                fixture("public/fixtures/grid-operations-workbook.xlsx"),
            )
            .unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!(
                "ws://{}/documents/hello.docx/ws",
                listener.local_addr().unwrap()
            );
            let serving = tokio::spawn(other.clone().serve(listener));
            let client: crate::Client<jade_docx_collab::DocxModel> =
                crate::Client::connect(&url, None).await.unwrap();
            client
                .edit(None, |context| {
                    let first = context.try_reserve(1).unwrap();
                    context
                        .submit(jade_docx_collab::DocxOp::InsertText {
                            story: jade_docx_collab::StoryId::Body,
                            parent: None,
                            first,
                            text: "x".into(),
                        })
                        .unwrap();
                })
                .unwrap();
            client.settle().await.unwrap();
            assert_eq!(other.seq("hello.docx").await.unwrap(), 1);
            assert!(!other.export("hello.docx").await.unwrap().is_empty());
            client.disconnect().await;
            serving.abort();
            let _ = serving.await;
        });
        finished.send(()).unwrap();
    });
    // Release the simulated I/O even on failure, so the test never strands
    // a thread or relies on an unusually large/slow input file.
    let progressed = done.recv_timeout(Duration::from_secs(5));
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    let probe_result = probe.join();
    assert!(
        progressed.is_ok(),
        "other documents were blocked by initialization: {progressed:?}"
    );
    probe_result.unwrap();
    assert_eq!(server.documents().len(), 3);
    drop(server);
    let restored = Server::with_storage(root.path()).unwrap();
    assert_eq!(restored.documents().len(), 3);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert_eq!(restored.seq("hello.docx").await.unwrap(), 1);
        assert_eq!(restored.seq("slow.docx").await.unwrap(), 0);
    });
}

#[test]
fn failed_and_unwound_initializers_release_the_name_without_poisoning_other_rooms() {
    let server = Server::new();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let fail = server.start_with("new.docx", Format::Docx, || {
        Err("cannot initialize this file".into())
    });
    assert!(matches!(fail, Err(ServerError::Open { .. })));
    assert_eq!(server.documents().len(), 1);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = server.start_with("new.docx", Format::Docx, || {
            panic!("initialization unwound")
        });
    }));
    assert!(unwound.is_err());
    assert_eq!(server.documents().len(), 1);
    server.open("new.docx", fixture("hello.docx")).unwrap();
    assert_eq!(server.documents().len(), 2);
}

#[tokio::test]
async fn published_document_names_follow_the_snapshot_when_storage_uses_another_spelling() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::with_storage(root.path()).unwrap();
    server.open("Logical.docx", fixture("hello.docx")).unwrap();
    drop(server);
    // Portable stand-in for a filesystem retaining a different spelling of
    // an aliased path: the logical name must belong to the winning snapshot.
    std::fs::rename(
        root.path().join("Logical.docx"),
        root.path().join("physical.docx"),
    )
    .unwrap();
    let restored = Server::with_storage(root.path()).unwrap();
    assert_eq!(
        restored.documents(),
        vec![("Logical.docx".to_owned(), Format::Docx)]
    );
    assert_eq!(restored.seq("Logical.docx").await.unwrap(), 0);
    assert!(matches!(
        restored.seq("physical.docx").await,
        Err(ServerError::NotFound(_))
    ));
    assert!(!restored.export("Logical.docx").await.unwrap().is_empty());
}

#[test]
fn invalid_persisted_names_are_refused_before_becoming_routes() {
    for invalid in ["../outside.docx", ".hidden.docx", "a.xlsx"] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::with_storage(root.path()).unwrap();
        server.open("a.docx", fixture("hello.docx")).unwrap();
        drop(server);
        let path = root.path().join("a.docx/snapshot-1.json");
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        metadata["name"] = serde_json::json!(invalid);
        std::fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        assert!(
            Server::with_storage(root.path()).is_err(),
            "invalid saved name {invalid:?} was hosted"
        );
    }
}

#[test]
fn a_reserved_document_name_is_refused_before_touching_storage() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::with_storage(root.path()).unwrap();
    let bytes =
        std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx"))
            .unwrap();
    server.open("hello.docx", bytes.clone()).unwrap();
    let unexpected = root.path().join("must-not-be-created.docx");
    // The final name reservation must protect even a caller that checked
    // availability before another upload won the race.
    let error = server
        .start(
            "hello.docx",
            Format::Docx,
            Source::NewStored {
                bytes,
                dir: unexpected.clone(),
                options: StorageOptions::default(),
            },
        )
        .unwrap_err();
    assert!(matches!(error, ServerError::Exists(_)), "{error}");
    assert!(
        !unexpected.exists(),
        "a losing upload must not initialize or overwrite storage"
    );
}

fn pressure_tokens(role: &str) -> Tokens {
    Tokens::from_json(&serde_json::json!({"key":{"name":"Ada","role":role}}).to_string()).unwrap()
}

type TestSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn pressure_read(socket: &mut TestSocket) -> ServerMessage {
    let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("socket must progress while the actor is paused")
        .unwrap()
        .unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}

// The thread pauses deterministically after join. This exercises actual socket
// admission, rather than relying on a large/slow native document to fill a queue.
#[tokio::test(flavor = "current_thread")]
async fn full_input_keeps_other_documents_and_token_changes_live() {
    use tokio_tungstenite::tungstenite::Message as Frame;
    for revoke in [false, true] {
        let server = Server::new().with_tokens(pressure_tokens("editor"));
        server.open("fast.docx", fixture("hello.docx")).unwrap();
        let (release, gate) = mpsc::channel();
        let (finished, done) = oneshot::channel();
        server
            .start_with("slow.docx", Format::Docx, || {
                Ok(room::Actor::test_worker(1, 1024, move |mut inbox| {
                    {
                        let entry = inbox.blocking_recv().unwrap();
                        let Command::Join { outbox, reply, .. } = entry.value else {
                            panic!("join first")
                        };
                        outbox
                            .send(
                                ServerMessage::Welcome {
                                    format: Format::Docx,
                                    replica: jade_collab_core::id::ReplicaId(1),
                                    seq: 0,
                                    snapshot: serde_json::Value::Null,
                                }
                                .to_json(),
                            )
                            .unwrap();
                        let _ = reply.send(Ok(jade_collab_core::id::ReplicaId(1)));
                    }
                    let _ = gate.recv();
                    let mut seen = Vec::new();
                    while let Some(entry) = inbox.blocking_recv() {
                        match entry.value {
                            Command::Submit {
                                submission,
                                refusal,
                                ..
                            } => seen.push((submission.client_seq, refusal.is_some())),
                            Command::Disconnected { .. } => break,
                            _ => panic!("unexpected pressure command"),
                        }
                    }
                    let _ = finished.send(seen);
                }))
            })
            .unwrap();
        let commands = server.commands("slow.docx").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://{}/documents/slow.docx/ws?token=key",
            listener.local_addr().unwrap()
        );
        let serving = tokio::spawn(server.clone().serve(listener));
        let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        socket
            .send(Frame::Text(r#"{"type":"join"}"#.into()))
            .await
            .unwrap();
        assert!(matches!(
            pressure_read(&mut socket).await,
            ServerMessage::Welcome { .. }
        ));
        let mut bytes = 0;
        for ticket in [1, 2] {
            let text = serde_json::to_string(&ClientMessage::Submit(
                jade_collab_sync::protocol::Submission {
                    client_seq: ticket,
                    base_seq: 0,
                    op: serde_json::Value::Null,
                },
            ))
            .unwrap();
            bytes += text.len();
            socket.send(Frame::Text(text.into())).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while commands.bytes_in_use() < bytes {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second frame must be waiting for the occupied input slot");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), server.seq("fast.docx"))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        if revoke {
            server.set_tokens(Tokens::default()).unwrap();
            assert!(
                matches!(
                    pressure_read(&mut socket).await,
                    ServerMessage::Revoked { .. }
                ),
                "revocation must not await actor capacity"
            );
        } else {
            server.set_tokens(pressure_tokens("viewer")).unwrap();
        }
        release.send(()).unwrap();
        if !revoke {
            socket.close(None).await.unwrap();
        }
        let seen = tokio::time::timeout(Duration::from_secs(3), done)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            seen,
            if revoke {
                vec![(1, false)]
            } else {
                vec![(1, false), (2, true)]
            }
        );
        serving.abort();
        let _ = serving.await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn input_wait_deadline_cancels_capacity_and_oversize_fails_before_decode() {
    let (commands, _inbox) = crate::mailbox::channel(1, 128);
    let (reply, _answer) = oneshot::channel();
    commands.send(Command::Seq { reply }).await.unwrap();
    let (outbox, _outgoing, overloaded) = outbox(8);
    let mut access = Access {
        grant: None,
        role: Role::Editor,
        tokens: None,
    };
    assert!(matches!(
        input_slot(
            &commands,
            10,
            &mut access,
            &outbox,
            &overloaded,
            Duration::from_millis(20)
        )
        .await,
        Err(Ending::InputBusy)
    ));
    assert_eq!(
        commands.bytes_in_use(),
        1,
        "timed out reservation returned its bytes"
    );
    assert!(matches!(
        input_slot(
            &commands,
            129,
            &mut access,
            &outbox,
            &overloaded,
            Duration::from_secs(1)
        )
        .await,
        Err(Ending::InputTooLarge { limit: 128 })
    ));
    commands.close();
    assert!(matches!(
        input_slot(
            &commands,
            1,
            &mut access,
            &outbox,
            &overloaded,
            Duration::from_secs(1)
        )
        .await,
        Err(Ending::Stopped)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn actor_drop_drains_committed_commands_without_waiting_on_the_runtime_to_drop_a_permit() {
    let (done, answer) = oneshot::channel();
    let actor = room::Actor::test_worker(2, 16, move |mut inbox| {
        let mut count = 0;
        while inbox.blocking_recv().is_some() {
            count += 1;
        }
        let _ = done.send(count);
    });
    let uncommitted = actor.commands.reserve(4).await.unwrap();
    let (reply, _answer) = oneshot::channel();
    actor.commands.send(Command::Seq { reply }).await.unwrap();
    drop(actor); // joins on this executor, while the uncommitted permit is live
    assert_eq!(answer.await.unwrap(), 1);
    assert!(matches!(
        uncommitted.send(Command::Seq {
            reply: oneshot::channel().0
        }),
        Err(crate::mailbox::Error::Closed)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn oversize_socket_frame_is_refused_before_json_decode_or_actor_admission() {
    use tokio_tungstenite::tungstenite::Message as Frame;
    let server = Server::new();
    let (finished, done) = oneshot::channel();
    server
        .start_with("small.docx", Format::Docx, || {
            Ok(room::Actor::test_worker(1, 128, move |mut inbox| {
                let entry = inbox.blocking_recv().unwrap();
                assert!(matches!(
                    entry.value,
                    Command::Disconnected { replica: None, .. }
                ));
                let _ = finished.send(());
            }))
        })
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/small.docx/ws",
        listener.local_addr().unwrap()
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    socket
        .send(Frame::Text("x".repeat(129).into()))
        .await
        .unwrap();
    let ServerMessage::Error { message } = pressure_read(&mut socket).await else {
        panic!("input error expected")
    };
    assert!(
        message.contains("128-byte"),
        "size admission precedes the invalid-JSON error: {message}"
    );
    let Some(Ok(Frame::Close(Some(frame)))) = socket.next().await else {
        panic!("close expected")
    };
    assert_eq!(u16::from(frame.code), 1009);
    tokio::time::timeout(Duration::from_secs(3), done)
        .await
        .unwrap()
        .unwrap();
    serving.abort();
    let _ = serving.await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_revoked_join_waiter_closes_and_cleans_up_before_learning_its_replica() {
    use tokio_tungstenite::tungstenite::Message as Frame;
    let server = Server::new().with_tokens(pressure_tokens("editor"));
    let (release, gate) = mpsc::channel();
    let (entered, ready) = oneshot::channel();
    let (finished, done) = oneshot::channel();
    server
        .start_with("slow.docx", Format::Docx, || {
            Ok(room::Actor::test_worker(1, 1024, move |mut inbox| {
                let entry = inbox.blocking_recv().unwrap();
                let Command::Join { reply, .. } = entry.value else {
                    panic!("join expected")
                };
                let _ = entered.send(());
                let _ = gate.recv();
                assert!(
                    reply.send(Ok(jade_collab_core::id::ReplicaId(1))).is_err(),
                    "join waiter was cancelled"
                );
                drop(entry._bytes);
                let entry = inbox.blocking_recv().unwrap();
                assert!(
                    matches!(entry.value, Command::Disconnected { replica: None, .. }),
                    "cleanup uses connection identity when its assigned id was never received"
                );
                let _ = finished.send(());
            }))
        })
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/slow.docx/ws?token=key",
        listener.local_addr().unwrap()
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    socket
        .send(Frame::Text(r#"{"type":"join"}"#.into()))
        .await
        .unwrap();
    ready.await.unwrap();
    server.set_tokens(Tokens::default()).unwrap();
    assert!(matches!(
        pressure_read(&mut socket).await,
        ServerMessage::Revoked { .. }
    ));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), done)
        .await
        .unwrap()
        .unwrap();
    serving.abort();
    let _ = serving.await;
}

mod native_failure;
