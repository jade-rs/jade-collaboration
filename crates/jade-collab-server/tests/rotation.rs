//! Token rotation without a restart: new tokens apply to the next request,
//! and open connections follow them — a revoked token's sockets are closed
//! with a message saying why, a changed role applies at once.

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use jade_collab_server::wire::ServerMessage;
use jade_collab_server::{Client, ClientError, Server, ServerError, Tokens};
use jade_collab_sync::ReplicaEvent;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::DocxModel;
use serde_json::json;
use tower::ServiceExt;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}

fn tokens(json: serde_json::Value) -> Tokens {
    Tokens::from_json(&json.to_string()).unwrap()
}

const SECOND: Duration = Duration::from_secs(10);

async fn status(server: &Server, uri: &str, bearer: &str) -> StatusCode {
    let request = Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    server.router().oneshot(request).await.unwrap().status()
}

/// Serves `server` with hello.docx and notes.docx; returns the socket URL
/// of each.
async fn start(server: &Server) -> (String, String) {
    for name in ["hello.docx", "notes.docx"] {
        server.open(name, fixture("hello.docx")).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.clone().serve(listener));
    (
        format!("ws://{addr}/documents/hello.docx/ws"),
        format!("ws://{addr}/documents/notes.docx/ws"),
    )
}

fn type_text(client: &Client<DocxModel>, text: &str) {
    let ops = resolve(
        &client.view(),
        &IntentOp::InsertText {
            paragraph: 0,
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

/// Waits for the connection to end and returns why.
async fn closed(client: &Client<DocxModel>) -> ClientError {
    tokio::time::timeout(SECOND, client.wait_for_seq(u64::MAX))
        .await
        .expect("the connection was not closed")
        .expect_err("the connection was not closed")
}

#[tokio::test]
async fn new_tokens_apply_to_the_next_request() {
    let server = Server::new().with_tokens(tokens(json!({
        "old": {"name": "Old", "role": "editor"},
    })));
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    assert_eq!(status(&server, "/documents", "old").await, StatusCode::OK);
    assert_eq!(
        status(&server, "/documents", "new").await,
        StatusCode::UNAUTHORIZED
    );
    server
        .set_tokens(tokens(json!({
            "new": {"name": "New", "role": "viewer", "documents": ["notes.docx"]},
        })))
        .unwrap();
    assert_eq!(
        status(&server, "/documents", "old").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(status(&server, "/documents", "new").await, StatusCode::OK);
    assert_eq!(
        status(&server, "/documents/hello.docx", "new").await,
        StatusCode::FORBIDDEN
    );
}

#[test]
fn a_server_without_tokens_takes_none_later() {
    let error = Server::new()
        .set_tokens(Tokens::default())
        .expect_err("an open server stays open");
    assert!(matches!(error, ServerError::NoTokens), "{error}");
}

#[test]
fn revocations_travel_as_their_own_message() {
    assert_eq!(
        serde_json::to_value(ServerMessage::Revoked {
            message: "access revoked: the token was withdrawn".into()
        })
        .unwrap(),
        json!({"type": "revoked", "message": "access revoked: the token was withdrawn"})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_token_closes_its_connections_and_no_others() {
    let server = Server::new().with_tokens(tokens(json!({
        "a": {"name": "Ann", "role": "editor"},
        "b": {"name": "Bob", "role": "editor"},
    })));
    let (hello, notes) = start(&server).await;
    let ann: Client<DocxModel> = Client::connect(&hello, Some("a")).await.unwrap();
    let ann_notes: Client<DocxModel> = Client::connect(&notes, Some("a")).await.unwrap();
    let bob: Client<DocxModel> = Client::connect(&hello, Some("b")).await.unwrap();
    ann.set_presence(json!({"cursor": 1}));
    tokio::time::timeout(SECOND, bob.wait_for_presence(|m| m.contains_key(&ann.id())))
        .await
        .unwrap()
        .unwrap();

    server
        .set_tokens(tokens(json!({"b": {"name": "Bob", "role": "editor"}})))
        .unwrap();
    for client in [&ann, &ann_notes] {
        let error = closed(client).await;
        assert!(matches!(&error, ClientError::Revoked(_)), "{error:?}");
        assert!(error.to_string().contains("revoked"), "{error}");
    }
    // The others carry on and see Ann leave.
    tokio::time::timeout(
        SECOND,
        bob.wait_for_presence(|m| !m.contains_key(&ann.id())),
    )
    .await
    .unwrap()
    .unwrap();
    type_text(&bob, "still here ");
    tokio::time::timeout(SECOND, bob.settle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), 1);
    // And the revoked token cannot come back.
    let replica = ann.disconnect().await;
    let error = Client::resume(&hello, replica, Some("a"))
        .await
        .err()
        .expect("refused");
    assert!(error.to_string().contains("401"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_a_document_closes_only_that_documents_connections() {
    let server = Server::new().with_tokens(tokens(json!({
        "t": {"name": "Tia", "role": "editor"},
    })));
    let (hello, notes) = start(&server).await;
    let on_hello: Client<DocxModel> = Client::connect(&hello, Some("t")).await.unwrap();
    let on_notes: Client<DocxModel> = Client::connect(&notes, Some("t")).await.unwrap();
    server
        .set_tokens(tokens(json!({
            "t": {"name": "Tia", "role": "editor", "documents": ["notes.docx"]},
        })))
        .unwrap();
    let error = closed(&on_hello).await;
    assert!(matches!(&error, ClientError::Revoked(_)), "{error:?}");
    assert!(error.to_string().contains("hello.docx"), "{error}");
    type_text(&on_notes, "kept ");
    tokio::time::timeout(SECOND, on_notes.settle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.seq("notes.docx").await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_role_applies_to_open_connections() {
    let editor = json!({"t": {"name": "Tia", "role": "editor"}});
    let viewer = json!({"t": {"name": "Tia", "role": "viewer"}});
    let server = Server::new().with_tokens(tokens(editor.clone()));
    let (hello, _) = start(&server).await;
    let client: Client<DocxModel> = Client::connect(&hello, Some("t")).await.unwrap();

    // The change reaches the connection before anything sent after it.
    server.set_tokens(tokens(viewer)).unwrap();
    type_text(&client, "denied ");
    tokio::time::timeout(SECOND, client.settle())
        .await
        .unwrap()
        .unwrap();
    let events = client.take_events();
    assert!(
        matches!(&events[..], [ReplicaEvent::Rejected { reason, .. }] if reason.message().contains("read-only")),
        "{events:?}"
    );
    assert_eq!(server.seq("hello.docx").await.unwrap(), 0);

    server.set_tokens(tokens(editor)).unwrap();
    type_text(&client, "allowed ");
    tokio::time::timeout(SECOND, client.settle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watched_tokens_file_is_taken_when_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tokens.json");
    std::fs::write(
        &path,
        json!({"old": {"name": "Old", "role": "editor"}}).to_string(),
    )
    .unwrap();
    let server = Server::new().with_tokens(Tokens::load(&path).unwrap());
    let watcher = server.watch_tokens(path.clone(), Duration::from_millis(20));

    // Written the way rotation tools do: a new file renamed into place.
    let next = dir.path().join("tokens.json.next");
    std::fs::write(
        &next,
        json!({"new": {"name": "New", "role": "editor"}}).to_string(),
    )
    .unwrap();
    std::fs::rename(&next, &path).unwrap();
    // Polled every 20 ms; the generous bound only matters on a loaded machine.
    tokio::time::timeout(10 * SECOND, async {
        while status(&server, "/documents", "old").await != StatusCode::UNAUTHORIZED {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the new file was never taken");
    assert_eq!(status(&server, "/documents", "new").await, StatusCode::OK);

    // A file that does not parse is reported and changes nothing.
    std::fs::write(&path, "{ not json").unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(status(&server, "/documents", "new").await, StatusCode::OK);
    assert_eq!(
        status(&server, "/documents", "old").await,
        StatusCode::UNAUTHORIZED
    );
    // Once it is fixed, it is taken.
    std::fs::write(
        &path,
        json!({"fixed": {"name": "F", "role": "viewer"}}).to_string(),
    )
    .unwrap();
    tokio::time::timeout(SECOND, async {
        while status(&server, "/documents", "fixed").await != StatusCode::OK {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the fixed file was never taken");
    watcher.abort();
}

#[test]
fn reloading_from_a_file_reports_what_is_wrong_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tokens.json");
    std::fs::write(
        &path,
        json!({"t": {"name": "T", "role": "editor"}}).to_string(),
    )
    .unwrap();
    let server = Server::new().with_tokens(Tokens::load(&path).unwrap());
    std::fs::write(
        &path,
        json!({"u": {"name": "U", "role": "editor"}}).to_string(),
    )
    .unwrap();
    server.reload_tokens(&path).unwrap();
    std::fs::write(&path, "[]").unwrap();
    let error = server.reload_tokens(&path).expect_err("not a token list");
    assert!(matches!(error, ServerError::Tokens(_)), "{error}");
    let error = server
        .reload_tokens(&dir.path().join("missing.json"))
        .expect_err("no file");
    assert!(error.to_string().contains("missing.json"), "{error}");
}
