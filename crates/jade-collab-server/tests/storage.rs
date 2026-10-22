//! A server with a storage directory survives a restart: every replica
//! resumes where it was and the file is what it was.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jade_collab_server::{Client, ClientError, Fsync, Server, StorageOptions};
use jade_collab_sync::Document;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{DocxModel, JadeDocx};
use jade_xlsx_collab::intent::{resolve as resolve_cell, XlsxIntent};
use jade_xlsx_collab::{CellInput, JadeXlsx, XlsxModel};
use proptest::prelude::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::task::JoinHandle;

#[test]
fn one_storage_directory_has_only_one_server_owner() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::with_storage(dir.path()).unwrap();
    let clone = server.clone();
    assert!(
        Server::with_storage(dir.path()).is_err(),
        "a second writer was admitted"
    );
    drop(server);
    assert!(
        Server::with_storage(dir.path().join(".")).is_err(),
        "a clone still owns storage"
    );
    drop(clone);
    Server::with_storage(dir.path()).unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// The OS lease follows the server's shared ownership across arbitrary
    /// opens, clones, drops and path aliases, without manual stale-lock cleanup.
    #[test]
    fn storage_ownership_follows_live_server_clones(
        steps in prop::collection::vec((0u8..3, any::<usize>(), any::<bool>()), 1..70),
    ) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("shared");
        let mut owners: Vec<Server> = Vec::new();
        for (kind, choice, alias) in steps {
            match kind {
                0 => {
                    let path = if alias { dir.join(".") } else { dir.clone() };
                    let opened = Server::with_storage(path);
                    if owners.is_empty() { owners.push(opened.unwrap()); }
                    else { prop_assert!(opened.is_err(), "storage already has an owner"); }
                }
                1 if !owners.is_empty() => {
                    owners.push(owners[choice % owners.len()].clone());
                }
                2 if !owners.is_empty() => { owners.swap_remove(choice % owners.len()); }
                _ => {}
            }
            // Independent storage remains available, even while this root is leased.
            Server::with_storage(root.path().join("other")).unwrap();
        }
        drop(owners);
        Server::with_storage(dir).unwrap();
    }
}

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}

const XLSX: &str = "public/fixtures/grid-operations-workbook.xlsx";

/// Serves `server` until the handle is aborted; returns the documents' base URL.
async fn listen(server: &Server) -> (String, JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(server.clone().serve(listener));
    (format!("ws://{addr}/documents"), task)
}

async fn catch_up<D>(server: &Server, name: &str, client: &Client<D>)
where
    D: Document + Serialize + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let seq = server.seq(name).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), client.wait_for_seq(seq))
        .await
        .expect("caught up in time")
        .unwrap();
}

fn type_text(client: &Client<DocxModel>, paragraph: usize, text: &str) {
    let view = client.view();
    let ops = resolve(
        &view,
        &IntentOp::InsertText {
            paragraph,
            offset: 0,
            text: text.into(),
        },
        &mut |n| client.reserve(n),
    )
    .unwrap();
    for op in ops {
        client.submit(op).unwrap();
    }
}

fn set_cell(client: &Client<XlsxModel>, reference: &str, value: CellInput) {
    let op = resolve_cell(
        &client.view(),
        &XlsxIntent::SetCell {
            sheet: "Operations".into(),
            reference: reference.into(),
            value,
        },
        &mut |n| client.reserve(n),
    )
    .unwrap();
    client.submit(op).unwrap();
}

fn insert_row(client: &Client<XlsxModel>, before: u32) {
    let op = resolve_cell(
        &client.view(),
        &XlsxIntent::InsertRow {
            sheet: "Operations".into(),
            before,
        },
        &mut |n| client.reserve(n),
    )
    .unwrap();
    client.submit(op).unwrap();
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_server_resumes_every_replica_where_it_was() {
    let dir = tempfile::tempdir().unwrap();
    // Restarts from a checkpoint taken mid-session plus the journal after it.
    let options = StorageOptions {
        checkpoint_every: 5,
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    server.open("book.xlsx", fixture(XLSX)).unwrap();
    let (base, task) = listen(&server).await;
    let docx = format!("{base}/hello.docx/ws");
    let xlsx = format!("{base}/book.xlsx/ws");

    let a: Client<DocxModel> = Client::connect(&docx, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&docx, None).await.unwrap();
    let c: Client<XlsxModel> = Client::connect(&xlsx, None).await.unwrap();
    let d: Client<XlsxModel> = Client::connect(&xlsx, None).await.unwrap();
    type_text(&a, 0, "[a] ");
    type_text(&b, 0, "[b] ");
    set_cell(&c, "E6", CellInput::Number(7.0));
    insert_row(&d, 6);
    set_cell(&d, "D9", CellInput::Formula("E6*2".into()));
    for client in [&a, &b] {
        client.settle().await.unwrap();
    }
    for client in [&c, &d] {
        client.settle().await.unwrap();
    }
    catch_up(&server, "hello.docx", &a).await;
    catch_up(&server, "hello.docx", &b).await;
    catch_up(&server, "book.xlsx", &c).await;
    catch_up(&server, "book.xlsx", &d).await;
    let docx_before = server.export("hello.docx").await.unwrap();
    let xlsx_before = server.export("book.xlsx").await.unwrap();
    let docx_seq = server.seq("hello.docx").await.unwrap();
    let view_before = a.view();

    // Everyone drops; the server goes away without saving anything.
    let (a, b, c, d) = (
        a.disconnect().await,
        b.disconnect().await,
        c.disconnect().await,
        d.disconnect().await,
    );
    task.abort();
    let _ = task.await;
    drop(server);

    let server = Server::with_storage_options(dir.path(), options).unwrap();
    let mut documents: Vec<String> = server.documents().into_iter().map(|(n, _)| n).collect();
    documents.sort();
    assert_eq!(documents, ["book.xlsx", "hello.docx"]);
    assert_eq!(server.seq("hello.docx").await.unwrap(), docx_seq);
    assert_eq!(server.export("hello.docx").await.unwrap(), docx_before);
    assert_eq!(server.export("book.xlsx").await.unwrap(), xlsx_before);
    assert!(server.faults("hello.docx").await.unwrap().is_empty());

    let (base, _task) = listen(&server).await;
    let docx = format!("{base}/hello.docx/ws");
    let xlsx = format!("{base}/book.xlsx/ws");
    let a = Client::resume(&docx, &a, None).await.unwrap();
    let b = Client::resume(&docx, &b, None).await.unwrap();
    let c = Client::resume(&xlsx, &c, None).await.unwrap();
    let d = Client::resume(&xlsx, &d, None).await.unwrap();
    assert_eq!(a.view(), view_before, "the model itself was restored");

    // Editing goes on, and a newcomer gets a fresh replica id.
    type_text(&b, 1, "after ");
    set_cell(&c, "E7", CellInput::Text("after".into()));
    let e: Client<DocxModel> = Client::connect(&docx, None).await.unwrap();
    assert!(
        e.id() > a.id() && e.id() > b.id(),
        "replica ids are not reused"
    );
    type_text(&e, 0, "[e] ");
    // Everyone's edits are in before anyone catches up.
    for client in [&a, &b, &e] {
        client.settle().await.unwrap();
    }
    for client in [&c, &d] {
        client.settle().await.unwrap();
    }
    for client in [&a, &b, &e] {
        catch_up(&server, "hello.docx", client).await;
    }
    for client in [&c, &d] {
        catch_up(&server, "book.xlsx", client).await;
    }
    assert_eq!(a.view(), b.view());
    assert_eq!(a.view(), e.view());
    assert_eq!(c.view(), d.view());
    let exported = JadeDocx::load(&server.export("hello.docx").await.unwrap()).unwrap();
    assert_eq!(exported.body_paragraph_texts(), a.view().paragraph_texts());
    let book = JadeXlsx::open(&server.export("book.xlsx").await.unwrap()).unwrap();
    let cell = book.workbook().cell("Operations", "E7").unwrap();
    assert_eq!(cell.text.as_deref(), Some("after"));
    assert!(server.faults("hello.docx").await.unwrap().is_empty());
    assert!(server.faults("book.xlsx").await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn checkpoints_bound_the_log_and_replicas_behind_them_must_rejoin() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every: 3,
        keep_tail: 0,
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&server).await;
    let url = format!("{base}/hello.docx/ws");

    let writer: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let idle: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let idle = idle.disconnect().await;
    let idle_live: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let idle_live = idle_live.disconnect().await;
    for i in 0..10 {
        type_text(&writer, 0, &format!("{i}"));
        writer.settle().await.unwrap();
    }
    catch_up(&server, "hello.docx", &writer).await;
    // A running server forgets the log before its checkpoint too.
    let error = Client::resume(&url, &idle_live, None)
        .await
        .err()
        .expect("refused");
    assert!(matches!(error, ClientError::Rejoin(_)), "{error}");
    let before = server.export("hello.docx").await.unwrap();
    let writer = writer.disconnect().await;
    task.abort();
    let _ = task.await;
    drop(server);

    // Only the newest checkpoint is kept, and the log after it is short.
    let document = dir.path().join("hello.docx");
    let names = files(&document);
    assert_eq!(
        names
            .iter()
            .filter(|n| n.starts_with("snapshot-") && n.ends_with(".json"))
            .count(),
        1,
        "{names:?}"
    );
    let log = names.iter().find(|n| n.starts_with("log-")).unwrap();
    let lines = std::fs::read_to_string(document.join(log)).unwrap();
    assert!(lines.lines().count() < 3, "{log}: {lines}");

    let server = Server::with_storage_options(dir.path(), options).unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), 10);
    assert_eq!(server.export("hello.docx").await.unwrap(), before);
    let (base, _task) = listen(&server).await;
    let url = format!("{base}/hello.docx/ws");

    let writer = Client::resume(&url, &writer, None).await.unwrap();
    assert_eq!(writer.seq(), 10);
    // The idle replica was at log position 0, long compacted away.
    let error = Client::resume(&url, &idle, None)
        .await
        .err()
        .expect("refused");
    assert!(matches!(error, ClientError::Rejoin(_)), "{error}");
    assert!(error.to_string().contains("join again"), "{error}");
    let rejoined: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    assert_eq!(rejoined.view(), writer.view());
}

#[tokio::test(flavor = "multi_thread")]
async fn replicas_just_behind_a_checkpoint_resume_from_the_kept_tail() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every: 3,
        keep_tail: 4,
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&server).await;
    let url = format!("{base}/hello.docx/ws");

    let far: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let far_live = Client::<DocxModel>::connect(&url, None).await.unwrap();
    let (far, far_live) = (far.disconnect().await, far_live.disconnect().await);
    let writer: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    for i in 0..6 {
        type_text(&writer, 0, &format!("{i}"));
        writer.settle().await.unwrap();
    }
    let near: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let near_live = Client::<DocxModel>::connect(&url, None).await.unwrap();
    let (near, near_live) = (near.disconnect().await, near_live.disconnect().await);
    assert_eq!(near.seq(), 6);
    for i in 6..9 {
        type_text(&writer, 0, &format!("{i}"));
        writer.settle().await.unwrap();
    }
    catch_up(&server, "hello.docx", &writer).await;

    // Three behind, across checkpoints: resumed, not asked to join again.
    let near_live = Client::resume(&url, &near_live, None).await.unwrap();
    assert_eq!(near_live.seq(), 9);
    assert_eq!(near_live.view(), writer.view());
    // Nine behind is further than the four kept.
    let error = Client::resume(&url, &far_live, None)
        .await
        .err()
        .expect("refused");
    assert!(matches!(error, ClientError::Rejoin(_)), "{error}");

    let view = writer.view();
    drop(near_live);
    drop(writer);
    task.abort();
    let _ = task.await;
    drop(server);

    // The tail is on disk: after a restart the same replica still resumes.
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    let (base, _task) = listen(&server).await;
    let url = format!("{base}/hello.docx/ws");
    let near = Client::resume(&url, &near, None).await.unwrap();
    assert_eq!(near.seq(), 9);
    assert_eq!(near.view(), view);
    let error = Client::resume(&url, &far, None)
        .await
        .err()
        .expect("refused");
    assert!(matches!(error, ClientError::Rejoin(_)), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_syncs_every_record_restarts_like_any_other() {
    assert_eq!(StorageOptions::default().sync, Fsync::Checkpoints);
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        sync: Fsync::EveryRecord,
        checkpoint_every: 4,
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&server).await;
    let url = format!("{base}/hello.docx/ws");
    let writer: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    for i in 0..5 {
        type_text(&writer, 0, &format!("{i}"));
        writer.settle().await.unwrap();
    }
    catch_up(&server, "hello.docx", &writer).await;
    let before = server.export("hello.docx").await.unwrap();
    let writer = writer.disconnect().await;
    task.abort();
    let _ = task.await;
    drop(server);

    let server = Server::with_storage_options(dir.path(), options).unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), 5);
    assert_eq!(server.export("hello.docx").await.unwrap(), before);
    let (base, _task) = listen(&server).await;
    let writer = Client::resume(&format!("{base}/hello.docx/ws"), &writer, None)
        .await
        .unwrap();
    assert_eq!(writer.seq(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_storage_failure_closes_waiting_clients_and_their_pending_edit_can_resume() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every: 2, // join, then the first edit triggers a checkpoint
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&server).await;
    let client: Client<DocxModel> = Client::connect(&format!("{base}/hello.docx/ws"), None)
        .await
        .unwrap();
    let document = dir.path().join("hello.docx");
    let moved = dir.path().join("temporarily-unavailable");
    // The open journal remains writable, but the checkpoint path is gone.
    std::fs::rename(&document, &moved).unwrap();
    type_text(&client, 0, "recover me ");
    let optimistic = client.view();
    let result = tokio::time::timeout(Duration::from_secs(2), client.settle())
        .await
        .expect("a stopped document must wake clients waiting for an acknowledgement");
    assert!(
        result.is_err(),
        "the failed checkpoint was not acknowledged"
    );
    let replica = client.disconnect().await;
    assert_eq!(replica.pending_len(), 1);
    task.abort();
    let _ = task.await;
    drop(server);
    std::fs::rename(&moved, &document).unwrap();

    let server = Server::with_storage_options(dir.path(), options).unwrap();
    let (base, task) = listen(&server).await;
    let client = Client::resume(&format!("{base}/hello.docx/ws"), &replica, None)
        .await
        .unwrap();
    client.settle().await.unwrap();
    assert_eq!(client.pending_len(), 0);
    assert_eq!(
        client.view(),
        optimistic,
        "the journal kept exactly one copy of the edit"
    );
    assert_eq!(client.seq(), 1);
    type_text(&client, 0, "after recovery ");
    client.settle().await.unwrap();
    assert!(server.faults("hello.docx").await.unwrap().is_empty());
    drop(client);
    task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_last_server_stops_connected_writers_before_releasing_storage() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::with_storage(dir.path()).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&server).await;
    let client: Client<DocxModel> = Client::connect(&format!("{base}/hello.docx/ws"), None)
        .await
        .unwrap();
    type_text(&client, 0, "before shutdown ");
    client.settle().await.unwrap();
    let view = client.view();
    let seq = client.seq();
    // An upgraded WebSocket owns a document sender but no Server clone.
    // It must not keep the old actor writing after a new server takes over.
    task.abort();
    let _ = task.await;
    drop(server);
    let restarted = Server::with_storage(dir.path()).unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), client.wait_for_seq(seq + 1))
        .await
        .expect("dropping the server must stop its document connections");
    assert!(closed.is_err());
    let replica = client.disconnect().await;
    let (base, task) = listen(&restarted).await;
    let client = Client::resume(&format!("{base}/hello.docx/ws"), &replica, None)
        .await
        .unwrap();
    assert_eq!(client.view(), view);
    type_text(&client, 0, "after shutdown ");
    client.settle().await.unwrap();
    assert_eq!(client.seq(), seq + 1);
    assert!(restarted.faults("hello.docx").await.unwrap().is_empty());
    drop(client);
    task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_torn_last_journal_line_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::with_storage(dir.path()).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&server).await;
    let writer: Client<DocxModel> = Client::connect(&format!("{base}/hello.docx/ws"), None)
        .await
        .unwrap();
    type_text(&writer, 0, "kept ");
    writer.settle().await.unwrap();
    let before = server.export("hello.docx").await.unwrap();
    let seq = server.seq("hello.docx").await.unwrap();
    drop(writer);
    task.abort();
    let _ = task.await;
    drop(server);

    // The process died halfway through appending a record.
    let document = dir.path().join("hello.docx");
    let log = files(&document)
        .into_iter()
        .find(|n| n.starts_with("log-"))
        .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(document.join(&log))
        .unwrap();
    std::io::Write::write_all(&mut file, br#"{"type":"committed","seq":"#).unwrap();
    drop(file);

    let server = Server::with_storage(dir.path()).unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), seq);
    assert_eq!(server.export("hello.docx").await.unwrap(), before);
    // And the journal goes on from there.
    let (base, task) = listen(&server).await;
    let writer: Client<DocxModel> = Client::connect(&format!("{base}/hello.docx/ws"), None)
        .await
        .unwrap();
    type_text(&writer, 0, "more ");
    writer.settle().await.unwrap();
    drop(writer);
    task.abort();
    let _ = task.await;
    drop(server);
    let server = Server::with_storage(dir.path()).unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), seq + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_journal_is_reported_rather_than_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::with_storage(dir.path()).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    drop(server);
    let document = dir.path().join("hello.docx");
    let log = files(&document)
        .into_iter()
        .find(|n| n.starts_with("log-"))
        .unwrap();
    let log = document.join(log);
    std::fs::write(&log, "not json\n{}\n").unwrap();
    let error = Server::with_storage(dir.path()).err().expect("refused");
    assert!(error.to_string().contains("hello.docx"), "{error}");
    // Failed startup releases ownership too, so repaired storage can be opened.
    std::fs::write(log, "").unwrap();
    Server::with_storage(dir.path()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn documents_with_unsafe_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::with_storage(dir.path()).unwrap();
    for name in ["../escape.docx", "a/b.docx", ".hidden.docx", "a\\b.docx"] {
        assert!(
            server.open(name, fixture("hello.docx")).is_err(),
            "{name} was accepted"
        );
    }
    assert_eq!(files(dir.path()), [".lock"]);
}

/// The bold characters of the second body paragraph of a written file.
fn bold_in_second_paragraph(bytes: &[u8]) -> String {
    let jade = JadeDocx::load(bytes).unwrap();
    let surface = jade.body_surfaces()[1];
    jade.word()
        .paragraph(surface)
        .unwrap()
        .runs
        .iter()
        .filter(|r| r.properties.as_ref().and_then(|p| p.bold) == Some(true))
        .map(|r| r.text.clone())
        .collect()
}

fn edit(client: &Client<DocxModel>, op: IntentOp) {
    let ops = resolve(&client.view(), &op, &mut |n| client.reserve(n)).unwrap();
    for op in ops {
        client.submit(op).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn imported_spreadsheet_styles_survive_socket_edits_checkpoints_and_resume() {
    use jade_xlsx_collab::FormatChange;
    let bytes = fixture("public/fixtures/number-formats-workbook.xlsx");
    let native = JadeXlsx::open(&bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every: 2,
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    server.open("book.xlsx", bytes).unwrap();
    let (base, task) = listen(&server).await;
    let writer: Client<XlsxModel> = Client::connect(&format!("{base}/book.xlsx/ws"), None)
        .await
        .unwrap();
    let sheet_name = writer.view().sheets()[0].name.clone();
    let (sheet, row, column) = writer.view().locate(&sheet_name, "B2").unwrap();
    let mut expected = native.workbook().format_at(&sheet_name, "B2").unwrap();
    assert_eq!(writer.view().format(sheet, row, column), expected);
    assert!(!writer.view().default_format(sheet).is_empty());
    let op = resolve_cell(
        &writer.view(),
        &XlsxIntent::SetFormat {
            sheet: sheet_name.clone(),
            range: "B2".into(),
            change: FormatChange {
                fill_color: Some("F0F000".into()),
                ..Default::default()
            },
        },
        &mut |n| writer.reserve(n),
    )
    .unwrap();
    writer.submit(op).unwrap();
    writer.settle().await.unwrap();
    expected.fill_color = Some("F0F000".into());
    assert_eq!(writer.view().format(sheet, row, column), expected);
    let before = writer.view();
    let bytes = server.export("book.xlsx").await.unwrap();
    let replica = writer.disconnect().await;
    task.abort();
    let _ = task.await;
    drop(server);

    let server = Server::with_storage_options(dir.path(), options).unwrap();
    assert_eq!(server.export("book.xlsx").await.unwrap(), bytes);
    let (base, task) = listen(&server).await;
    let writer = Client::resume(&format!("{base}/book.xlsx/ws"), &replica, None)
        .await
        .unwrap();
    assert_eq!(writer.view(), before);
    let op = resolve_cell(
        &writer.view(),
        &XlsxIntent::SetFormat {
            sheet: sheet_name.clone(),
            range: "B2".into(),
            change: FormatChange {
                italic: Some(true),
                ..Default::default()
            },
        },
        &mut |n| writer.reserve(n),
    )
    .unwrap();
    writer.submit(op).unwrap();
    writer.settle().await.unwrap();
    expected.italic = Some(true);
    assert_eq!(writer.view().format(sheet, row, column), expected);
    let jade = JadeXlsx::open(&server.export("book.xlsx").await.unwrap()).unwrap();
    let file = jade.workbook().format_at(&sheet_name, "B2").unwrap();
    assert_eq!(file.number_format, expected.number_format);
    assert_eq!(file.fill_color, expected.fill_color);
    assert_eq!(file.italic, expected.italic);
    assert!(server.faults("book.xlsx").await.unwrap().is_empty());
    writer.disconnect().await;
    task.abort();
    let _ = task.await;
}

/// Downloaded Word settings, the collaborative model and new revisions agree
/// after importing a file, replaying a journal, restoring a checkpoint and
/// resuming the same replica over a new socket.
#[tokio::test(flavor = "multi_thread")]
async fn tracking_mode_survives_file_import_and_durable_restarts() {
    let seed = Server::new();
    seed.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&seed).await;
    let author: Client<DocxModel> = Client::connect(&format!("{base}/hello.docx/ws"), None)
        .await
        .unwrap();
    edit(&author, IntentOp::SetTracking { on: true });
    author.settle().await.unwrap();
    let imported = seed.export("hello.docx").await.unwrap();
    author.disconnect().await;
    task.abort();
    let _ = task.await;
    drop(seed);

    for checkpoint_every in [1, 100] {
        let dir = tempfile::tempdir().unwrap();
        let options = StorageOptions {
            checkpoint_every,
            ..StorageOptions::default()
        };
        let mut server = Server::with_storage_options(dir.path(), options).unwrap();
        server.open("hello.docx", imported.clone()).unwrap();
        let (mut base, mut task) = listen(&server).await;
        let mut writer: Client<DocxModel> = Client::connect(&format!("{base}/hello.docx/ws"), None)
            .await
            .unwrap();
        assert!(
            writer.view().tracking(),
            "the imported file enables tracking"
        );

        for (on, text) in [(true, "tracked日本"), (false, "plain🙂"), (true, "again")] {
            edit(&writer, IntentOp::SetTracking { on });
            writer.settle().await.unwrap();
            type_text(&writer, 1, text);
            writer.settle().await.unwrap();
            catch_up(&server, "hello.docx", &writer).await;
            let view = writer.view();
            assert_eq!(view.tracking(), on);
            let mut revisions: Vec<_> = view.revisions().into_iter().map(|r| r.text).collect();
            revisions.sort();
            assert_eq!(revisions.iter().any(|r| r.contains(text)), on);
            let bytes = server.export("hello.docx").await.unwrap();
            let jade = JadeDocx::load(&bytes).unwrap();
            let in_file = DocxModel::from_word_document(jade.word()).unwrap();
            assert_eq!(in_file.tracking(), on);
            let mut file_revisions: Vec<_> =
                in_file.revisions().into_iter().map(|r| r.text).collect();
            file_revisions.sort();
            assert_eq!(file_revisions, revisions);

            let replica = writer.disconnect().await;
            task.abort();
            let _ = task.await;
            drop(server);
            server = Server::with_storage_options(dir.path(), options).unwrap();
            assert_eq!(server.export("hello.docx").await.unwrap(), bytes);
            (base, task) = listen(&server).await;
            writer = Client::resume(&format!("{base}/hello.docx/ws"), &replica, None)
                .await
                .unwrap();
            assert_eq!(writer.view(), view);
            assert!(server.faults("hello.docx").await.unwrap().is_empty());
        }
        writer.disconnect().await;
        task.abort();
        let _ = task.await;
        drop(server);
    }
}

/// A comment commits its jade-assigned number immediately after its cause.
/// Checkpointing the cause must not put that number in both the snapshot
/// and the journal that follows it.
#[tokio::test(flavor = "multi_thread")]
async fn a_checkpoint_at_a_comments_cause_restarts_without_replaying_its_number_twice() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every: 2, // join, then the comment's cause
        keep_tail: 4,
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&server).await;
    let url = format!("{base}/hello.docx/ws");
    let writer: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    edit(
        &writer,
        IntentOp::AddComment {
            start_paragraph: 0,
            start_offset: None,
            end_paragraph: 0,
            end_offset: None,
            text: "durable comment".into(),
            author: None,
            initials: None,
            date: None,
        },
    );
    writer.settle().await.unwrap();
    catch_up(&server, "hello.docx", &writer).await;
    let before = server.export("hello.docx").await.unwrap();
    let seq = server.seq("hello.docx").await.unwrap();
    assert_eq!(seq, 2, "the comment and its assigned number");
    let writer = writer.disconnect().await;
    task.abort();
    let _ = task.await;
    drop(server);

    let server = Server::with_storage_options(dir.path(), options).unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), seq);
    assert_eq!(server.export("hello.docx").await.unwrap(), before);
    let (base, task) = listen(&server).await;
    let writer = Client::resume(&format!("{base}/hello.docx/ws"), &writer, None)
        .await
        .unwrap();
    type_text(&writer, 0, "after restart ");
    writer.settle().await.unwrap();
    assert!(server.faults("hello.docx").await.unwrap().is_empty());
    drop(writer);
    task.abort();
}

/// Character formatting is a set of marks in the model; a checkpoint keeps
/// them in the model and the file without them, so a restarted server
/// writes the same file, and text typed next to a mark afterwards follows
/// the mark's expand rule rather than a bold run baked into the file.
#[tokio::test(flavor = "multi_thread")]
async fn marks_survive_a_restart_and_keep_their_expand_rules() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        checkpoint_every: 1,
        ..StorageOptions::default()
    };
    let server = Server::with_storage_options(dir.path(), options).unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let (base, task) = listen(&server).await;
    let url = format!("{base}/hello.docx/ws");
    let writer: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    assert_eq!(writer.view().paragraph_texts()[1], "World");
    edit(
        &writer,
        IntentOp::Format {
            start_paragraph: 1,
            start_offset: 0,
            end_paragraph: 1,
            end_offset: 3,
            change: jade_docx_collab::FormatChange::Bold { value: true },
        },
    );
    writer.settle().await.unwrap();
    catch_up(&server, "hello.docx", &writer).await;
    let before = server.export("hello.docx").await.unwrap();
    assert_eq!(bold_in_second_paragraph(&before), "Wor");
    let writer = writer.disconnect().await;
    task.abort();
    let _ = task.await;
    drop(server);

    let server = Server::with_storage_options(dir.path(), options).unwrap();
    assert_eq!(server.export("hello.docx").await.unwrap(), before);
    let (base, _task) = listen(&server).await;
    let writer = Client::resume(&format!("{base}/hello.docx/ws"), &writer, None)
        .await
        .unwrap();
    // Right before the bold text, at the paragraph's start, and right after it.
    edit(
        &writer,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 0,
            text: "x".into(),
        },
    );
    edit(
        &writer,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 4,
            text: "y".into(),
        },
    );
    writer.settle().await.unwrap();
    catch_up(&server, "hello.docx", &writer).await;
    assert_eq!(writer.view().paragraph_texts()[1], "xWoryld");
    let after = server.export("hello.docx").await.unwrap();
    assert_eq!(bold_in_second_paragraph(&after), "Wory");
    assert!(server.faults("hello.docx").await.unwrap().is_empty());
}
