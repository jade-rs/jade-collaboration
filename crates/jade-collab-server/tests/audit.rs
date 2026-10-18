//! The audit log: one JSON line per access — who (the token's name, never
//! the token) connected to which document with which role, which replica
//! it became, when it left and why, what was refused, and which files were
//! uploaded or downloaded.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use jade_collab_server::{Client, Server, Tokens};
use jade_docx_collab::DocxModel;
use serde_json::Value;
use tower::ServiceExt;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}

const TOKENS: &str = r#"{
    "ada-secret": { "name": "Ada", "role": "editor" },
    "vic-secret": { "name": "Vic", "role": "viewer", "documents": ["other.docx"] }
}"#;

fn entries(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect()
}

async fn until(path: &Path, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    for _ in 0..200 {
        let found = entries(path);
        if done(&found) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the audit log never got there: {:#?}", entries(path));
}

fn event<'a>(entries: &'a [Value], name: &str) -> Vec<&'a Value> {
    entries.iter().filter(|e| e["event"] == name).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn sockets_are_audited_by_token_name_and_never_by_token() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("audit.jsonl");
    let server = Server::new()
        .with_tokens(Tokens::from_json(TOKENS).unwrap())
        .with_audit_log(&log)
        .unwrap();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.clone().serve(listener));
    let url = format!("ws://{addr}/documents/hello.docx/ws");

    let ada: Client<DocxModel> = Client::connect(&url, Some("ada-secret")).await.unwrap();
    let replica = ada.id();
    // Vic's token does not open this document; an unknown token opens
    // nothing.
    assert!(Client::<DocxModel>::connect(&url, Some("vic-secret"))
        .await
        .is_err());
    assert!(Client::<DocxModel>::connect(&url, Some("guess"))
        .await
        .is_err());
    let replica_back = ada.disconnect().await;

    let found = until(&log, |found| !event(found, "closed").is_empty()).await;
    let connected = event(&found, "connected");
    assert_eq!(connected.len(), 1, "{found:#?}");
    assert_eq!(connected[0]["document"], "hello.docx");
    assert_eq!(connected[0]["token"], "Ada");
    assert_eq!(connected[0]["role"], "editor");
    let joined = event(&found, "joined");
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0]["replica"], replica.0);
    assert_eq!(joined[0]["token"], "Ada");
    let closed = event(&found, "closed");
    assert_eq!(closed[0]["replica"], replica.0);
    assert_eq!(closed[0]["reason"], "client");

    let refused = event(&found, "refused");
    assert_eq!(refused.len(), 2, "{found:#?}");
    assert!(refused
        .iter()
        .any(|e| e["token"] == "Vic" && e["status"] == 403 && e["document"] == "hello.docx"));
    assert!(refused
        .iter()
        .any(|e| e["token"].is_null() && e["status"] == 401));

    // Every entry says when.
    assert!(found
        .iter()
        .all(|e| e["at"].as_u64().is_some_and(|at| at > 0)));
    // Never the secrets themselves.
    let text = std::fs::read_to_string(&log).unwrap();
    for secret in ["ada-secret", "vic-secret", "guess"] {
        assert!(!text.contains(secret), "{secret} leaked into the audit log");
    }

    // Coming back is audited too.
    let _back = Client::resume(&url, replica_back, Some("ada-secret"))
        .await
        .unwrap();
    let found = until(&log, |found| !event(found, "resumed").is_empty()).await;
    assert_eq!(event(&found, "resumed")[0]["replica"], replica.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_and_downloads_are_audited() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("audit.jsonl");
    let server = Server::new()
        .with_tokens(Tokens::from_json(TOKENS).unwrap())
        .with_audit_log(&log)
        .unwrap();
    let request = |method: Method, uri: &str, token: &str, body: Body| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(body)
            .unwrap()
    };
    let put = server
        .router()
        .oneshot(request(
            Method::PUT,
            "/documents/new.docx",
            "ada-secret",
            Body::from(fixture("hello.docx")),
        ))
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::CREATED);
    let get = server
        .router()
        .oneshot(request(
            Method::GET,
            "/documents/new.docx",
            "ada-secret",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);

    let found = entries(&log);
    let uploaded = event(&found, "uploaded");
    assert_eq!(uploaded.len(), 1, "{found:#?}");
    assert_eq!(uploaded[0]["document"], "new.docx");
    assert_eq!(uploaded[0]["token"], "Ada");
    let downloaded = event(&found, "downloaded");
    assert_eq!(downloaded.len(), 1);
    assert_eq!(downloaded[0]["token"], "Ada");
}

#[test]
fn an_audit_log_that_cannot_be_opened_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Server::new()
        .with_audit_log(dir.path().join("missing").join("audit.jsonl"))
        .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rotated_audit_log_is_reopened_at_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("audit.jsonl");
    let rotated = dir.path().join("audit.jsonl.1");
    let server = Server::new()
        .with_tokens(Tokens::from_json(TOKENS).unwrap())
        .with_audit_log(&log)
        .unwrap();
    let upload = |name: &str| {
        Request::builder()
            .method(Method::PUT)
            .uri(format!("/documents/{name}"))
            .header(header::AUTHORIZATION, "Bearer ada-secret")
            .body(Body::from(fixture("hello.docx")))
            .unwrap()
    };
    let put = server.router().oneshot(upload("first.docx")).await.unwrap();
    assert_eq!(put.status(), StatusCode::CREATED);
    // logrotate moves the file away, then tells the server.
    std::fs::rename(&log, &rotated).unwrap();
    server.reopen_audit_log().unwrap();
    let put = server
        .router()
        .oneshot(upload("second.docx"))
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::CREATED);

    let old = entries(&rotated);
    assert_eq!(event(&old, "uploaded").len(), 1);
    assert_eq!(event(&old, "uploaded")[0]["document"], "first.docx");
    let new = entries(&log);
    assert_eq!(event(&new, "uploaded").len(), 1, "{new:#?}");
    assert_eq!(event(&new, "uploaded")[0]["document"], "second.docx");
}

#[test]
fn reopening_without_an_audit_log_does_nothing() {
    Server::new().reopen_audit_log().unwrap();
}
