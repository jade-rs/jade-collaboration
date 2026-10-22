use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::Id;
use jade_collab_server::wire::{ClientMessage, ServerMessage};
use jade_collab_server::{Client, ClientError, Server, Tokens};
use jade_collab_sync::{Document, Replica};
use jade_docx_collab::{intent::IntentOp, DocxModel};
use jade_pptx_collab::{intent::PptxIntent, PptxModel};
use jade_xlsx_collab::{intent::XlsxIntent, XlsxModel};
use proptest::prelude::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type Plan<D> = fn(&D, &mut dyn FnMut(u64) -> Id) -> Vec<<D as Document>::Op>;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}

fn state<D: Document + Serialize>(replica: &Replica<D>) -> Value
where
    D::Op: Serialize,
{
    json!({"id":replica.id(),"seq":replica.seq(),"view":replica.view(),"confirmed":replica.confirmed(),"next":replica.next_client_seq(),"pending":replica.pending_submissions()})
}

/// A fake peer that either corrupts the resume watermark or never answers.
async fn failing_peer(
    answer: bool,
) -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/fixture.docx/ws",
        listener.local_addr().unwrap()
    );
    let (seen, received) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let message = socket.next().await.unwrap().unwrap();
        let Message::Text(message) = message else {
            panic!("expected resume")
        };
        assert!(matches!(
            serde_json::from_str::<ClientMessage>(&message).unwrap(),
            ClientMessage::Resume { .. }
        ));
        let _ = seen.send(());
        if answer {
            let response = ServerMessage::Resumed {
                processed_client_seq: u64::MAX,
                missed: vec![],
            };
            socket
                .send(Message::Text(
                    serde_json::to_string(&response).unwrap().into(),
                ))
                .await
                .unwrap();
        }
        while let Some(message) = socket.next().await {
            if message.is_err() {
                break;
            }
        }
    });
    (url, received, task)
}

async fn history<D>(source: &str, name: &str, plan: Plan<D>, steps: &[u8])
where
    D: Document + DeserializeOwned + Serialize + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let server = Server::new()
        .with_tokens(Tokens::from_json(r#"{"editor":{"name":"writer","role":"editor"}}"#).unwrap());
    server.open(name, fixture(source)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        jade_collab_server::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let client = Client::<D>::connect(&url, Some("editor")).await.unwrap();
    let mut replica = client.disconnect().await;
    let mut total = 0;
    // Every format always exercises each failure, a lost answer, and retry;
    // generated transitions then cover different pending/confirmed mixtures.
    for kind in [0, 1, 2, 5, 6, 7].iter().chain(steps).chain([3].iter()) {
        let before = state(&replica);
        match kind {
            0 => {
                let view = replica.view().clone();
                for op in plan(&view, &mut |n| replica.reserve(n)) {
                    replica.submit(op).unwrap();
                    total += 1;
                }
            }
            1 => {
                let error = Client::resume(&url, &replica, Some("invalid"))
                    .await
                    .err()
                    .expect("authentication refused");
                assert!(error.to_string().contains("401"));
                assert_eq!(state(&replica), before);
            }
            2 | 5 | 6 => {
                let (bad, seen, peer) = failing_peer(*kind == 2).await;
                if *kind == 6 {
                    let mut opening = Box::pin(Client::resume(&bad, &replica, None));
                    tokio::select! {
                        _ = &mut opening => panic!("the unanswered opening completed"),
                        result = seen => result.unwrap(),
                    }
                    drop(opening);
                } else {
                    let result = Client::resume_with_timeout(
                        &bad,
                        &replica,
                        None,
                        if *kind == 2 {
                            Duration::from_secs(3)
                        } else {
                            Duration::from_millis(50)
                        },
                    )
                    .await;
                    if *kind == 2 {
                        assert!(matches!(result, Err(ClientError::Sync(_))));
                    } else {
                        assert!(matches!(result, Err(ClientError::Timeout(_))));
                    }
                }
                tokio::time::timeout(Duration::from_secs(3), peer)
                    .await
                    .expect("failed/canceled opening closed its socket")
                    .unwrap();
                assert_eq!(state(&replica), before);
            }
            3 | 7 => {
                let client = Client::resume(&url, &replica, Some("editor"))
                    .await
                    .unwrap();
                client.settle().await.unwrap();
                assert_eq!(serde_json::to_value(client.view()).unwrap(), before["view"]);
                if *kind == 7 {
                    drop(client.disconnect().await);
                    // Lose every acknowledgement deliberately, then retry the
                    // original replica. Stable tickets must prevent duplicates.
                    let retried = Client::resume(&url, &replica, Some("editor"))
                        .await
                        .unwrap();
                    retried.settle().await.unwrap();
                    replica = retried.disconnect().await;
                } else {
                    replica = client.disconnect().await;
                }
                assert_eq!(replica.pending_len(), 0);
                assert_eq!(server.seq(name).await.unwrap(), total);
            }
            _ => {
                // The client can disconnect before any of its resends are
                // answered; the next resume resolves whichever ones arrived.
                replica = Client::resume(&url, &replica, Some("editor"))
                    .await
                    .unwrap()
                    .disconnect()
                    .await;
            }
        }
    }
    let fresh = Client::<D>::connect(&url, Some("editor")).await.unwrap();
    assert_eq!(
        serde_json::to_value(fresh.view()).unwrap(),
        serde_json::to_value(replica.view()).unwrap()
    );
    assert!(server.faults(name).await.unwrap().is_empty());
    drop(fresh.disconnect().await);
    serving.abort();
    let _ = serving.await;
}

fn docx_plan(view: &DocxModel, ids: &mut dyn FnMut(u64) -> Id) -> Vec<<DocxModel as Document>::Op> {
    let intent: IntentOp =
        serde_json::from_value(json!({"type":"insert_text","paragraph":1,"offset":0,"text":"x"}))
            .unwrap();
    jade_docx_collab::intent::resolve(view, &intent, ids).unwrap()
}
fn xlsx_plan(view: &XlsxModel, ids: &mut dyn FnMut(u64) -> Id) -> Vec<<XlsxModel as Document>::Op> {
    let intent:XlsxIntent=serde_json::from_value(json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"number","value":17}})).unwrap();
    vec![jade_xlsx_collab::intent::resolve(view, &intent, ids).unwrap()]
}
fn pptx_plan(view: &PptxModel, ids: &mut dyn FnMut(u64) -> Id) -> Vec<<PptxModel as Document>::Op> {
    let intent:PptxIntent=serde_json::from_value(json!({"type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"x"})).unwrap();
    jade_pptx_collab::intent::resolve(view, &intent, ids).unwrap()
}

// Native batches run on a shared, potentially busy machine. Check that
// acknowledgements keep advancing instead of treating debug throughput as a
// network deadline. A stalled pump still fails after ten seconds.
async fn burst_progress<F: std::future::Future>(
    name: &str,
    progress: impl Fn() -> (u64, usize),
    work: F,
) -> F::Output {
    let started = tokio::time::Instant::now();
    let mut advanced = started;
    let mut observed = progress();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tokio::pin!(work);
    loop {
        tokio::select! {
            biased;
            result = &mut work => return result,
            _ = tick.tick() => {
                let current = progress();
                let now = tokio::time::Instant::now();
                if current != observed {
                    observed = current;
                    advanced = now;
                }
                assert!(now.duration_since(advanced) < Duration::from_secs(10),
                    "{name}: no native batch progress: seq={}, pending={}", observed.0, observed.1);
                assert!(now.duration_since(started) < Duration::from_secs(120),
                    "{name}: native batch exceeded overall bound: seq={}, pending={}", observed.0, observed.1);
            }
        }
    }
}

async fn burst<D>(source: &str, name: &str, plan: Plan<D>, inspect_supported: bool)
where
    D: Document + DeserializeOwned + Serialize + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let server = Server::new();
    server.open(name, fixture(source)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        jade_collab_server::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let client = Client::<D>::connect(&url, None).await.unwrap();
    let mut submitted = 0u64;
    let edit = |client: &Client<D>| {
        client
            .try_edit(None, |context| {
                let view = context.view().clone();
                let mut count = 0u64;
                for op in plan(&view, &mut |n| context.try_reserve(n).unwrap()) {
                    context.submit(op)?;
                    count += 1;
                }
                Ok::<_, jade_collab_sync::Rejection>(count)
            })
            .unwrap()
    };
    let final_view = {
        for _ in 0..130 {
            submitted += edit(&client);
        }
        let prefix = serde_json::to_value(client.view()).unwrap();
        let inspected = client.inspect();
        tokio::pin!(inspected);
        assert!(futures_util::poll!(&mut inspected).is_pending());
        for _ in 0..130 {
            submitted += edit(&client);
        }
        let final_view = serde_json::to_value(client.view()).unwrap();
        let inspection = burst_progress(
            name,
            || (client.seq(), client.pending_len()),
            &mut inspected,
        )
        .await;
        if inspect_supported {
            assert_eq!(
                serde_json::to_value(inspection.unwrap().snapshot.view).unwrap(),
                prefix
            );
        } else {
            assert!(matches!(inspection, Err(ClientError::Inspection(_))));
        }
        burst_progress(
            name,
            || (client.seq(), client.pending_len()),
            client.settle(),
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(client.view()).unwrap(), final_view);
        assert_eq!(server.seq(name).await.unwrap(), submitted);
        assert!(server.faults(name).await.unwrap().is_empty());
        final_view
    };
    let source = client.disconnect().await;
    let resumed = Client::resume(&url, &source, None).await.unwrap();
    assert_eq!(serde_json::to_value(resumed.view()).unwrap(), final_view);
    assert_eq!(resumed.pending_len(), 0);
    drop(resumed.disconnect().await);
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn native_bursts_preserve_inspection_boundaries_and_resume_in_every_format() {
    burst::<DocxModel>("hello.docx", "burst.docx", docx_plan, true).await;
    burst::<XlsxModel>(
        "public/fixtures/grid-operations-workbook.xlsx",
        "burst.xlsx",
        xlsx_plan,
        false,
    )
    .await;
    burst::<PptxModel>(
        "public/fixtures/sample-deck.pptx",
        "burst.pptx",
        pptx_plan,
        true,
    )
    .await;
}
async fn run(source: usize, steps: &[u8]) {
    match source {
        0 => history::<DocxModel>("hello.docx", "再試行.docx", docx_plan, steps).await,
        1 => {
            history::<XlsxModel>(
                "public/fixtures/grid-operations-workbook.xlsx",
                "再試行.xlsx",
                xlsx_plan,
                steps,
            )
            .await
        }
        _ => {
            history::<PptxModel>(
                "public/fixtures/sample-deck.pptx",
                "再試行.pptx",
                pptx_plan,
                steps,
            )
            .await
        }
    }
}

async fn permission_restart<D>(source: &str, name: &str, plan: Plan<D>, checkpoint_every: u64)
where
    D: Document + DeserializeOwned + Serialize + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    use jade_collab_server::StorageOptions;
    use jade_collab_sync::ReplicaEvent;
    let dir = tempfile::tempdir().unwrap();
    let open = || {
        Server::with_storage_options(dir.path(),StorageOptions {checkpoint_every,..StorageOptions::default()}).unwrap().with_tokens(Tokens::from_json(r#"{"viewer":{"name":"writer","role":"viewer"},"editor":{"name":"writer","role":"editor"}}"#).unwrap())
    };
    let mut server = open();
    server.open(name, fixture(source)).unwrap();
    let original_file = server.export(name).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        jade_collab_server::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let viewer = Client::<D>::connect(&url, Some("viewer")).await.unwrap();
    let original_view = serde_json::to_value(viewer.view()).unwrap();
    let mut source_replica = viewer.disconnect().await;
    let view = source_replica.view().clone();
    for op in plan(&view, &mut |n| source_replica.reserve(n)) {
        source_replica.submit(op).unwrap();
    }
    let refused = Client::resume(&url, &source_replica, Some("viewer"))
        .await
        .unwrap();
    refused.settle().await.unwrap();
    assert!(refused.take_events().iter().any(
        |e| matches!(e,ReplicaEvent::Rejected {reason,..} if reason.message().contains("read-only"))
    ));
    drop(refused.disconnect().await);
    assert_eq!(server.seq(name).await.unwrap(), 0);
    assert_eq!(server.export(name).await.unwrap(), original_file);
    serving.abort();
    let _ = serving.await;
    drop(server);
    server = open();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        jade_collab_server::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    // Retry the source that missed the refusal, now with an editor token.
    // A recorded refusal must stay refused after promotion and restart.
    let editor = Client::resume(&url, &source_replica, Some("editor"))
        .await
        .unwrap();
    editor.settle().await.unwrap();
    assert_eq!(editor.snapshot().pending, 0);
    assert!(editor
        .take_events()
        .iter()
        .any(|e| matches!(e, ReplicaEvent::Rejected { .. })));
    assert_eq!(serde_json::to_value(editor.view()).unwrap(), original_view);
    assert_eq!(server.seq(name).await.unwrap(), 0);
    assert_eq!(server.export(name).await.unwrap(), original_file);
    editor
        .edit(None, |context| {
            let view = context.view().clone();
            for op in plan(&view, &mut |n| context.try_reserve(n).unwrap()) {
                context.submit(op).unwrap();
            }
        })
        .unwrap();
    editor.settle().await.unwrap();
    assert_eq!(server.seq(name).await.unwrap(), 1);
    assert!(server.faults(name).await.unwrap().is_empty());
    drop(editor.disconnect().await);
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn viewer_refusals_survive_lost_answers_role_promotion_and_journal_or_checkpoint_restore() {
    for checkpoint_every in [2, 1000] {
        permission_restart::<DocxModel>("hello.docx", "refused.docx", docx_plan, checkpoint_every)
            .await;
        permission_restart::<XlsxModel>(
            "public/fixtures/grid-operations-workbook.xlsx",
            "refused.xlsx",
            xlsx_plan,
            checkpoint_every,
        )
        .await;
        permission_restart::<PptxModel>(
            "public/fixtures/sample-deck.pptx",
            "refused.pptx",
            pptx_plan,
            checkpoint_every,
        )
        .await;
    }
}

#[tokio::test]
async fn failed_or_canceled_resume_retains_pending_edits_for_every_office_format() {
    for source in 0..3 {
        run(source, &[0, 4, 1, 2, 3, 0, 7]).await;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn reconnect_retries_and_lost_answers_match_one_commit_per_submission(
        source in 0usize..3,
        steps in prop::collection::vec(0u8..8,1..18),
    ) {
        tokio::runtime::Runtime::new().unwrap().block_on(run(source,&steps));
    }
}

async fn ownership_restart<D>(source: &str, name: &str, plan: Plan<D>, checkpoint_every: u64)
where
    D: Document + DeserializeOwned + Serialize + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    use jade_collab_server::StorageOptions;
    let dir = tempfile::tempdir().unwrap();
    let grants = || {
        Tokens::from_json(r#"{"owner":{"name":"Ada","role":"editor"},"rotated":{"name":"Ada","role":"editor"},"other":{"name":"Mallory","role":"editor"},"viewer":{"name":"Vic","role":"viewer"}}"#).unwrap()
    };
    let open = || {
        Server::with_storage_options(
            dir.path(),
            StorageOptions {
                checkpoint_every,
                ..StorageOptions::default()
            },
        )
        .unwrap()
        .with_tokens(grants())
    };
    let mut server = open();
    server.open(name, fixture(source)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        jade_collab_server::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let owner = Client::<D>::connect(&url, Some("owner")).await.unwrap();
    let monitor = Client::<D>::connect(&url, Some("viewer")).await.unwrap();
    owner.set_presence(json!({"cursor":1}));
    tokio::time::timeout(
        Duration::from_secs(3),
        monitor.wait_for_presence(|p| p.contains_key(&owner.id())),
    )
    .await
    .unwrap()
    .unwrap();
    for token in ["other", "viewer"] {
        // A replica id and document snapshot are public to other readers.
        // Possession of them is insufficient to take over its connection.
        let forged = Replica::new(owner.id(), owner.seq(), owner.view());
        assert!(
            Client::resume(&url, &forged, Some(token)).await.is_err(),
            "{token} took over another principal's replica"
        );
    }
    owner
        .edit(None, |context| {
            let view = context.view().clone();
            for op in plan(&view, &mut |n| context.try_reserve(n).unwrap()) {
                context.submit(op).unwrap();
            }
        })
        .unwrap();
    owner.settle().await.unwrap();
    assert_eq!(
        server.seq(name).await.unwrap(),
        1,
        "refused takeover did not replace the owner's socket"
    );
    monitor.wait_for_seq(1).await.unwrap();
    assert!(
        monitor.presence().contains_key(&owner.id()),
        "refused takeover must not erase the owner's presence"
    );
    drop(monitor.disconnect().await);
    let replica = owner.disconnect().await;
    serving.abort();
    let _ = serving.await;
    drop(server);
    server = open();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        jade_collab_server::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    for token in ["other", "viewer"] {
        assert!(Client::resume(&url, &replica, Some(token)).await.is_err());
    }
    // Credentials may rotate while their stable principal name stays the same.
    let resumed = Client::resume(&url, &replica, Some("rotated"))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(resumed.view()).unwrap(),
        serde_json::to_value(replica.view()).unwrap()
    );
    assert_eq!(resumed.id(), replica.id());
    assert!(server.faults(name).await.unwrap().is_empty());
    drop(resumed.disconnect().await);
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn another_editor_or_viewer_cannot_take_over_a_replica_before_or_after_restore() {
    for checkpoint_every in [2, 1000] {
        ownership_restart::<DocxModel>("hello.docx", "owner.docx", docx_plan, checkpoint_every)
            .await;
        ownership_restart::<XlsxModel>(
            "public/fixtures/grid-operations-workbook.xlsx",
            "owner.xlsx",
            xlsx_plan,
            checkpoint_every,
        )
        .await;
        ownership_restart::<PptxModel>(
            "public/fixtures/sample-deck.pptx",
            "owner.pptx",
            pptx_plan,
            checkpoint_every,
        )
        .await;
    }
}
