//! Native stages must preserve both file bytes and pending retry metadata on
//! refusal, while exact-fit causes and their derived suffix remain writable.
use std::path::Path;
use std::time::Duration;

use jade_collab_core::grid::GridOp;
use jade_collab_core::id::Id;
use jade_collab_server::{Client, Server, StorageOptions};
use jade_collab_sync::{Document, ReplicaEvent};
use jade_docx_collab::{CommentContent, DocxModel, DocxOp, StoryId};
use jade_xlsx_collab::{XlsxModel, XlsxOp};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}
fn comment(model: &DocxModel, id: Id) -> DocxOp {
    let paragraph = &model.paragraphs()[0];
    DocxOp::AddComment {
        story: StoryId::Body,
        start: paragraph.chars[0],
        end: paragraph.chars[2],
        id,
        content: CommentContent::new("staged comment"),
    }
}
fn column(model: &XlsxModel, id: Id) -> XlsxOp {
    let (sheet, _, column) = model.locate("Operations", "A1").unwrap();
    XlsxOp::Structure {
        sheet,
        op: GridOp::InsertColumn {
            id,
            after: Some(column),
        },
    }
}
fn header(model: &XlsxModel, stamp: Id, text: &str) -> XlsxOp {
    let (sheet, row, column) = model.locate("Operations", "A1").unwrap();
    XlsxOp::SetHeader {
        sheet,
        row,
        column,
        stamp,
        text: text.into(),
    }
}
fn blank_header(model: &XlsxModel, stamp: Id) -> XlsxOp {
    header(model, stamp, "")
}
fn unique_header(model: &XlsxModel, stamp: Id) -> XlsxOp {
    header(model, stamp, "UniqueAtomicHeader")
}

#[derive(Clone, Copy)]
struct Case {
    seq: u64,
    stamp: Option<u64>,
    accepted: bool,
    children: u64,
}

async fn native<D>(
    path: &str,
    name: &str,
    case: Case,
    plan: fn(&D, Id) -> D::Op,
    checkpoint_every: u64,
) where
    D: Document + DeserializeOwned + Serialize + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let Case {
        seq,
        stamp,
        accepted,
        children,
    } = case;
    let root = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every,
        keep_tail: 8,
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(root.path(), options).unwrap();
    server.open(name, fixture(path)).unwrap();
    drop(server);
    let snapshot = root.path().join(name).join("snapshot-1.json");
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&snapshot).unwrap()).unwrap();
    saved["checkpoint"]["seq"] = seq.into();
    std::fs::write(snapshot, serde_json::to_vec(&saved).unwrap()).unwrap();
    let server = Server::with_storage_options(root.path(), options).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        jade_collab_server::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let client = Client::<D>::connect(&url, None).await.unwrap();
    let before = serde_json::to_value(client.view()).unwrap();
    let file = server.export(name).await.unwrap();
    let id = Id::new(
        stamp.unwrap_or_else(|| client.view().max_lamport() + 1),
        client.id(),
    );
    let op = plan(&client.view(), id);
    // Keep the origin as if every verdict were lost, even though a separate
    // live client lets this test observe when the server finishes the batch.
    let mut unanswered = jade_collab_sync::Replica::new(client.id(), seq, client.view());
    unanswered.submit(op.clone()).unwrap();
    client.submit(op).unwrap();
    tokio::time::timeout(Duration::from_secs(10), client.settle())
        .await
        .unwrap()
        .unwrap();
    let committed_seq = if accepted { seq + 1 + children } else { seq };
    client.wait_for_seq(committed_seq).await.unwrap();
    let events = client.take_events();
    assert_eq!(
        events
            .iter()
            .any(|event| matches!(event, ReplicaEvent::Rejected { .. })),
        !accepted,
        "{name}: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ReplicaEvent::Applied { .. }))
            .count() as u64,
        if accepted { children } else { 0 }
    );
    if !accepted {
        assert_eq!(serde_json::to_value(client.view()).unwrap(), before);
        assert_eq!(
            server.export(name).await.unwrap(),
            file,
            "a discarded stage changed the live package"
        );
    }
    assert_eq!(server.seq(name).await.unwrap(), committed_seq);
    assert!(server.faults(name).await.unwrap().is_empty());
    let after = serde_json::to_value(client.view()).unwrap();
    let exported = server.export(name).await.unwrap();
    drop(client.disconnect().await);
    serving.abort();
    let _ = serving.await;
    drop(server);
    let server = Server::with_storage_options(root.path(), options).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}{}",
        listener.local_addr().unwrap(),
        jade_collab_server::document_socket_path(name)
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let resumed = Client::<D>::resume(&url, &unanswered, None).await.unwrap();
    assert_eq!(serde_json::to_value(resumed.view()).unwrap(), after);
    assert_eq!(server.export(name).await.unwrap(), exported);
    assert_eq!(resumed.pending_len(), 0);
    assert_eq!(
        unanswered.pending_len(),
        1,
        "opening a retry preserves its caller's lost-verdict source"
    );
    // A retry of the same issued ticket is already processed even for a
    // refused stage, both in a journal-only restart and a checkpoint restore.
    assert_eq!(resumed.seq(), committed_seq);
    assert!(server.faults(name).await.unwrap().is_empty());
    drop(resumed.disconnect().await);
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn native_numbering_and_table_changes_commit_all_or_none_at_log_boundaries() {
    for checkpoint_every in [1, 1000] {
        for remaining in [1, 2] {
            native::<DocxModel>(
                "hello.docx",
                "stage.docx",
                Case {
                    seq: u64::MAX - remaining,
                    stamp: None,
                    accepted: remaining == 2,
                    children: 1,
                },
                comment,
                checkpoint_every,
            )
            .await;
            native::<XlsxModel>(
                "public/fixtures/grid-operations-workbook.xlsx",
                "stage.xlsx",
                Case {
                    seq: u64::MAX - remaining,
                    stamp: None,
                    accepted: remaining == 2,
                    children: 1,
                },
                column,
                checkpoint_every,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn native_derived_identifiers_refuse_overflow_and_accept_exact_or_zero_allocations() {
    for checkpoint_every in [1, 1000] {
        for (stamp, accepted) in [(u64::MAX, false), (u64::MAX - 1, true)] {
            native::<XlsxModel>(
                "public/fixtures/grid-operations-workbook.xlsx",
                "stage.xlsx",
                Case {
                    seq: 0,
                    stamp: Some(stamp),
                    accepted,
                    children: 1,
                },
                column,
                checkpoint_every,
            )
            .await;
            native::<XlsxModel>(
                "public/fixtures/grid-operations-workbook.xlsx",
                "stage.xlsx",
                Case {
                    seq: 0,
                    stamp: Some(stamp),
                    accepted,
                    children: 1,
                },
                blank_header,
                checkpoint_every,
            )
            .await;
        }
        // Classification may require a stage, but it must not conservatively
        // reject a valid edit whose actual derived suffix is empty.
        native::<XlsxModel>(
            "public/fixtures/grid-operations-workbook.xlsx",
            "stage.xlsx",
            Case {
                seq: u64::MAX - 1,
                stamp: Some(u64::MAX),
                accepted: true,
                children: 0,
            },
            unique_header,
            checkpoint_every,
        )
        .await;
    }
}
