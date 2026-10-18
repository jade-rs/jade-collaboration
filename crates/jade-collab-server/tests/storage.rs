//! A server with a storage directory survives a restart: every replica
//! resumes where it was and the file is what it was.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jade_collab_server::{Client, ClientError, Server, StorageOptions};
use jade_collab_sync::Document;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{DocxModel, JadeDocx};
use jade_xlsx_collab::intent::{resolve as resolve_cell, XlsxIntent};
use jade_xlsx_collab::{CellInput, JadeXlsx, XlsxModel};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::task::JoinHandle;

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
    let a = Client::resume(&docx, a, None).await.unwrap();
    let b = Client::resume(&docx, b, None).await.unwrap();
    let c = Client::resume(&xlsx, c, None).await.unwrap();
    let d = Client::resume(&xlsx, d, None).await.unwrap();
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
    let error = Client::resume(&url, idle_live, None)
        .await
        .err()
        .expect("refused");
    assert!(matches!(error, ClientError::Rejoin(_)), "{error}");
    let before = server.export("hello.docx").await.unwrap();
    let writer = writer.disconnect().await;
    task.abort();
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

    let writer = Client::resume(&url, writer, None).await.unwrap();
    assert_eq!(writer.seq(), 10);
    // The idle replica was at log position 0, long compacted away.
    let error = Client::resume(&url, idle, None)
        .await
        .err()
        .expect("refused");
    assert!(matches!(error, ClientError::Rejoin(_)), "{error}");
    assert!(error.to_string().contains("join again"), "{error}");
    let rejoined: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    assert_eq!(rejoined.view(), writer.view());
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
    let (base, _task) = listen(&server).await;
    let writer: Client<DocxModel> = Client::connect(&format!("{base}/hello.docx/ws"), None)
        .await
        .unwrap();
    type_text(&writer, 0, "more ");
    writer.settle().await.unwrap();
    drop(writer);
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
    std::fs::write(document.join(log), "not json\n{}\n").unwrap();
    let error = Server::with_storage(dir.path()).err().expect("refused");
    assert!(error.to_string().contains("hello.docx"), "{error}");
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
    assert!(files(dir.path()).is_empty());
}
