//! Real writer failures must leave pending edits recoverable from the healthy prefix.
use super::*;

fn failure_docx(
    _model: &jade_docx_collab::DocxModel,
    id: jade_collab_core::id::Id,
    text: &str,
) -> jade_docx_collab::DocxOp {
    jade_docx_collab::DocxOp::InsertText {
        story: jade_docx_collab::StoryId::Body,
        parent: None,
        first: id,
        text: text.into(),
    }
}
fn failure_xlsx(
    m: &jade_xlsx_collab::XlsxModel,
    stamp: jade_collab_core::id::Id,
    text: &str,
) -> jade_xlsx_collab::XlsxOp {
    let (sheet, row, column) = m.locate("Operations", "E6").unwrap();
    jade_xlsx_collab::XlsxOp::SetCell {
        sheet,
        row,
        column,
        stamp,
        value: jade_xlsx_collab::CellInput::Text(text.into()),
    }
}
fn failure_pptx(
    m: &jade_pptx_collab::PptxModel,
    first: jade_collab_core::id::Id,
    text: &str,
) -> jade_pptx_collab::PptxOp {
    jade_pptx_collab::PptxOp::InsertText {
        story: jade_pptx_collab::StoryId::Notes {
            slide: m.slides()[0].id,
        },
        parent: None,
        first,
        text: text.into(),
    }
}
fn enqueue_native<D>(
    client: &crate::Client<D>,
    text: &str,
    make: fn(&D, jade_collab_core::id::Id, &str) -> D::Op,
) where
    D: jade_collab_sync::Document + serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    D::Op: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    client
        .try_edit(None, |edit| {
            let model = edit.view().clone();
            let first = edit.try_reserve(text.chars().count() as u64)?;
            edit.submit(make(&model, first, text))?;
            Ok::<_, Rejection>(())
        })
        .unwrap();
}
async fn failure_storage_released(root: &std::path::Path) {
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
                Err(error) => panic!("storage probe: {error}"),
            }
        }
    })
    .await
    .unwrap();
}
async fn native_failure<D>(
    format: Format,
    path: &str,
    name: &str,
    make: fn(&D, jade_collab_core::id::Id, &str) -> D::Op,
    checkpoint_every: u64,
    after: bool,
) where
    D: jade_collab_sync::Document + serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    D::Op: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let root = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every,
        ..Default::default()
    };
    let server = Server::with_storage_options(root.path(), options).unwrap();
    server
        .start_with(name, format, || {
            room::failure_tests::spawn(
                format,
                Source::NewStored {
                    bytes: fixture(path),
                    dir: root.path().join(name),
                    options,
                },
                2,
                after,
            )
        })
        .unwrap();
    server
        .open("unaffected.docx", fixture("hello.docx"))
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        crate::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let client = crate::Client::<D>::connect(&url, None).await.unwrap();
    let observer = crate::Client::<D>::connect(&url, None).await.unwrap();
    enqueue_native(&client, "A", make);
    client.settle().await.unwrap();
    observer.wait_for_seq(1).await.unwrap();
    let prefix = serde_json::to_value(client.view()).unwrap();
    let file = server.export(name).await.unwrap();
    client.take_events();
    observer.take_events();
    enqueue_native(&client, "B", make);
    let expected = serde_json::to_value(client.view()).unwrap();
    let settled = tokio::time::timeout(Duration::from_secs(3), client.settle())
        .await
        .expect("native failure must close its connection");
    assert!(
        settled.is_err(),
        "a failed native write must not be acknowledged"
    );
    assert_eq!(client.pending_len(), 1);
    assert!(
        client.take_events().iter().all(|event| !matches!(
            event,
            jade_collab_sync::ReplicaEvent::Acknowledged { .. }
                | jade_collab_sync::ReplicaEvent::Rejected { .. }
        )),
        "the failed edit remains unknown, rather than acknowledged/rejected"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(3), client.inspect())
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(3), observer.wait_for_seq(2))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(observer.view()).unwrap(),
        prefix,
        "observers never see the failed operation"
    );
    assert!(
        server.export(name).await.is_err(),
        "a partially changed file cannot be downloaded"
    );
    assert_eq!(server.seq("unaffected.docx").await.unwrap(), 0);
    assert!(!server.export("unaffected.docx").await.unwrap().is_empty());
    let retained = client.disconnect().await;
    observer.disconnect().await;
    serving.abort();
    let _ = serving.await;
    drop(server);
    failure_storage_released(root.path()).await;

    let server = Server::with_storage_options(root.path(), options).unwrap();
    assert_eq!(
        server.seq(name).await.unwrap(),
        1,
        "only the healthy prefix was checkpointed/journaled"
    );
    assert_eq!(server.export(name).await.unwrap(), file);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        crate::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let fresh = crate::Client::<D>::connect(&url, None).await.unwrap();
    assert_eq!(serde_json::to_value(fresh.view()).unwrap(), prefix);
    let resumed = crate::Client::resume(&url, &retained, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), resumed.settle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(resumed.view()).unwrap(), expected);
    assert_eq!(resumed.pending_len(), 0);
    assert_eq!(server.seq(name).await.unwrap(), 2);
    assert!(server.faults(name).await.unwrap().is_empty());
    resumed.disconnect().await;
    fresh.disconnect().await;
    serving.abort();
    let _ = serving.await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_failures_close_without_verdicts_and_recover_pending_edits_for_every_format() {
    for checkpoint_every in [1, 1000] {
        for after in [false, true] {
            native_failure::<jade_docx_collab::DocxModel>(
                Format::Docx,
                "hello.docx",
                "failure.docx",
                failure_docx,
                checkpoint_every,
                after,
            )
            .await;
            native_failure::<jade_xlsx_collab::XlsxModel>(
                Format::Xlsx,
                "public/fixtures/grid-operations-workbook.xlsx",
                "failure.xlsx",
                failure_xlsx,
                checkpoint_every,
                after,
            )
            .await;
            native_failure::<jade_pptx_collab::PptxModel>(
                Format::Pptx,
                "public/fixtures/sample-deck.pptx",
                "failure.pptx",
                failure_pptx,
                checkpoint_every,
                after,
            )
            .await;
        }
    }
}

async fn native_replay_failure<D>(
    format: Format,
    path: &str,
    name: &str,
    make: fn(&D, jade_collab_core::id::Id, &str) -> D::Op,
) where
    D: jade_collab_sync::Document + serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    D::Op: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let root = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every: 1000,
        ..Default::default()
    };
    let server = Server::with_storage_options(root.path(), options).unwrap();
    server.open(name, fixture(path)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        crate::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let client = crate::Client::<D>::connect(&url, None).await.unwrap();
    enqueue_native(&client, "A", make);
    client.settle().await.unwrap();
    let file = server.export(name).await.unwrap();
    client.disconnect().await;
    serving.abort();
    let _ = serving.await;
    drop(server);
    failure_storage_released(root.path()).await;
    let dir = root.path().join(name);
    let saved = || {
        std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let before = saved();
    let front = Server::new();
    for after in [false, true] {
        let failure = front.start_with(name, format, || {
            room::failure_tests::spawn(
                format,
                Source::Stored {
                    dir: dir.clone(),
                    options,
                },
                1,
                after,
            )
        });
        assert!(failure.is_err(), "native replay failure must abort startup");
        assert!(front.documents().is_empty(), "no failed room is published");
        assert_eq!(
            saved(),
            before,
            "failed replay never checkpoints or changes the durable journal"
        );
    }
    front
        .start_with(name, format, || {
            room::spawn(name, format, Source::Stored { dir, options })
        })
        .unwrap();
    assert_eq!(front.seq(name).await.unwrap(), 1);
    assert_eq!(front.export(name).await.unwrap(), file);
}
#[tokio::test(flavor = "current_thread")]
async fn native_replay_failure_aborts_room_publication_without_rewriting_durable_history() {
    native_replay_failure::<jade_docx_collab::DocxModel>(
        Format::Docx,
        "hello.docx",
        "replay.docx",
        failure_docx,
    )
    .await;
    native_replay_failure::<jade_xlsx_collab::XlsxModel>(
        Format::Xlsx,
        "public/fixtures/grid-operations-workbook.xlsx",
        "replay.xlsx",
        failure_xlsx,
    )
    .await;
    native_replay_failure::<jade_pptx_collab::PptxModel>(
        Format::Pptx,
        "public/fixtures/sample-deck.pptx",
        "replay.pptx",
        failure_pptx,
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_origin_failure_preserves_author_and_ticket_recovery_boundaries() {
    use jade_docx_collab::{DocxModel, DocxOp, StoryId};
    for at in [1, 2] {
        for after in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let name = "tracked.docx";
            let options = StorageOptions {
                checkpoint_every: 1,
                ..Default::default()
            };
            let server = Server::with_storage_options(root.path(), options)
                .unwrap()
                .with_tokens(pressure_tokens("editor"));
            server
                .start_with(name, Format::Docx, || {
                    room::failure_tests::spawn(
                        Format::Docx,
                        Source::NewStored {
                            bytes: fixture("hello.docx"),
                            dir: root.path().join(name),
                            options,
                        },
                        at,
                        after,
                    )
                })
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!(
                "ws://{}{}",
                listener.local_addr().unwrap(),
                crate::document_socket_path(name)
            );
            let serving = tokio::spawn(server.clone().serve(listener));
            let client = crate::Client::<DocxModel>::connect(&url, Some("key"))
                .await
                .unwrap();
            let original = serde_json::to_value(client.view()).unwrap();
            client
                .try_edit(None, |edit| {
                    let first = edit.try_reserve(1)?;
                    edit.submit(DocxOp::TrackInsertText {
                        story: StoryId::Body,
                        parent: None,
                        first,
                        text: "X".into(),
                        date: None,
                    })?;
                    Ok::<_, Rejection>(())
                })
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(3), client.settle())
                    .await
                    .unwrap()
                    .is_err()
            );
            assert_eq!(client.pending_len(), 1);
            assert!(client.take_events().iter().all(|event| !matches!(
                event,
                jade_collab_sync::ReplicaEvent::Acknowledged { .. }
                    | jade_collab_sync::ReplicaEvent::Rejected { .. }
            )));
            let retained = client.disconnect().await;
            serving.abort();
            let _ = serving.await;
            drop(server);
            failure_storage_released(root.path()).await;
            let server = Server::with_storage_options(root.path(), options)
                .unwrap()
                .with_tokens(pressure_tokens("editor"));
            assert_eq!(
                server.seq(name).await.unwrap(),
                (at - 1) as u64,
                "only successful origin metadata may precede the unknown edit"
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!(
                "ws://{}{}",
                listener.local_addr().unwrap(),
                crate::document_socket_path(name)
            );
            let serving = tokio::spawn(server.clone().serve(listener));
            let fresh = crate::Client::<DocxModel>::connect(&url, Some("key"))
                .await
                .unwrap();
            if at == 1 {
                assert_eq!(serde_json::to_value(fresh.view()).unwrap(), original);
            } else {
                assert_eq!(fresh.view().author(retained.id()), Some("Ada"));
            }
            let back = crate::Client::resume(&url, &retained, Some("key"))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(3), back.settle())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(back.pending_len(), 0);
            assert_eq!(server.seq(name).await.unwrap(), 2);
            assert_eq!(back.view().author(back.id()), Some("Ada"));
            assert_eq!(
                back.view()
                    .paragraph_texts()
                    .join("\n")
                    .matches('X')
                    .count(),
                1
            );
            assert!(server.faults(name).await.unwrap().is_empty());
            back.disconnect().await;
            fresh.disconnect().await;
            serving.abort();
            let _ = serving.await;
        }
    }
}
