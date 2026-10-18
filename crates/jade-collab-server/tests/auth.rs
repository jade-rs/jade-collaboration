//! Bearer tokens: with tokens configured, every route and the socket need
//! one; viewers read and never write.

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use jade_collab_server::{Client, Role, Server, Tokens};
use jade_collab_sync::ReplicaEvent;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::DocxModel;
use proptest::prelude::*;
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
    "editor-secret": { "name": "Ada", "role": "editor" },
    "viewer-secret": { "name": "Vic", "role": "viewer" }
}"#;

fn guarded() -> Server {
    let server = Server::new().with_tokens(Tokens::from_json(TOKENS).unwrap());
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    server
}

async fn status(server: &Server, method: Method, uri: &str, bearer: Option<&str>) -> StatusCode {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = if uri.ends_with(".docx") || uri.contains(".docx?") {
        Body::from(fixture("hello.docx"))
    } else {
        Body::empty()
    };
    server
        .router()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
        .status()
}

async fn listen(server: &Server) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.clone().serve(listener));
    format!("ws://{addr}/documents/hello.docx/ws")
}

fn type_text(client: &Client<DocxModel>, text: &str) {
    let view = client.view();
    let ops = resolve(
        &view,
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

#[test]
fn tokens_are_read_from_json() {
    let tokens = Tokens::from_json(TOKENS).unwrap();
    let ada = tokens.grant("editor-secret").unwrap();
    assert_eq!(ada.name, "Ada");
    assert_eq!(ada.role, Role::Editor);
    assert_eq!(tokens.grant("viewer-secret").unwrap().role, Role::Viewer);
    assert!(tokens.grant("nope").is_none());
    assert!(Tokens::from_json(r#"{"t": {"name": "x", "role": "owner"}}"#).is_err());
    assert!(Tokens::from_json("[]").is_err());
}

#[tokio::test]
async fn without_a_valid_token_every_route_is_unauthorized() {
    let server = guarded();
    for (method, uri) in [
        (Method::GET, "/documents"),
        (Method::GET, "/documents/hello.docx"),
        (Method::PUT, "/documents/new.docx"),
        (Method::GET, "/documents/hello.docx/ws"),
    ] {
        for bearer in [None, Some("wrong"), Some("")] {
            assert_eq!(
                status(&server, method.clone(), uri, bearer).await,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} with {bearer:?}"
            );
        }
    }
    let response = server
        .router()
        .oneshot(Request::get("/documents").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
        "Bearer"
    );
}

#[tokio::test]
async fn a_token_works_as_a_header_or_a_query_parameter() {
    let server = guarded();
    for token in ["editor-secret", "viewer-secret"] {
        assert_eq!(
            status(&server, Method::GET, "/documents", Some(token)).await,
            StatusCode::OK
        );
        assert_eq!(
            status(
                &server,
                Method::GET,
                &format!("/documents?token={token}"),
                None
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            status(&server, Method::GET, "/documents/hello.docx", Some(token)).await,
            StatusCode::OK
        );
    }
    assert_eq!(
        status(&server, Method::GET, "/documents?token=wrong", None).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn only_editors_upload() {
    let server = guarded();
    assert_eq!(
        status(
            &server,
            Method::PUT,
            "/documents/v.docx",
            Some("viewer-secret")
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(
            &server,
            Method::PUT,
            "/documents/e.docx",
            Some("editor-secret")
        )
        .await,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn a_server_without_tokens_is_open() {
    let server = Server::new();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    assert_eq!(
        status(&server, Method::GET, "/documents", None).await,
        StatusCode::OK
    );
    let url = listen(&server).await;
    let client: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    type_text(&client, "open ");
    client.settle().await.unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_socket_needs_a_token() {
    let server = guarded();
    let url = listen(&server).await;
    for token in [None, Some("wrong")] {
        let refused: Result<Client<DocxModel>, _> = Client::connect(&url, token).await;
        let error = refused.err().expect("refused");
        assert!(error.to_string().contains("401"), "{error}");
    }
    let editor: Client<DocxModel> = Client::connect(&url, Some("editor-secret")).await.unwrap();
    type_text(&editor, "edited ");
    editor.settle().await.unwrap();
    // Browsers cannot set headers on a WebSocket; the query works too.
    let by_query: Client<DocxModel> = Client::connect(&format!("{url}?token=editor-secret"), None)
        .await
        .unwrap();
    assert_eq!(by_query.view(), editor.view());
    // Resuming needs the token as well.
    let replica = by_query.disconnect().await;
    let refused = Client::<DocxModel>::resume(&url, replica, None).await;
    let error = refused.err().expect("resumed without a token");
    assert!(error.to_string().contains("401"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn viewers_read_but_their_edits_are_never_applied() {
    let server = guarded();
    let url = listen(&server).await;
    let editor: Client<DocxModel> = Client::connect(&url, Some("editor-secret")).await.unwrap();
    let viewer: Client<DocxModel> = Client::connect(&url, Some("viewer-secret")).await.unwrap();
    let before = server.export("hello.docx").await.unwrap();

    type_text(&viewer, "viewer ");
    tokio::time::timeout(Duration::from_secs(10), viewer.settle())
        .await
        .unwrap()
        .unwrap();
    let events = viewer.take_events();
    assert!(
        matches!(&events[..], [ReplicaEvent::Rejected { reason, .. }] if reason.message().contains("read-only")),
        "{events:?}"
    );
    assert_eq!(server.seq("hello.docx").await.unwrap(), 0);
    assert_eq!(server.export("hello.docx").await.unwrap(), before);

    // The viewer still follows everything the editor does.
    type_text(&editor, "editor ");
    editor.settle().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), viewer.wait_for_seq(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(viewer.view(), editor.view());
    assert!(viewer.view().paragraph_texts()[0].starts_with("editor "));

    // Resuming does not turn a viewer into an editor.
    let replica = viewer.disconnect().await;
    let viewer = Client::resume(&url, replica, Some("viewer-secret"))
        .await
        .unwrap();
    type_text(&viewer, "again ");
    viewer.settle().await.unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Exactly the configured tokens open the server, whichever way they are
    /// presented and whatever else is configured.
    #[test]
    fn only_configured_tokens_are_accepted(
        configured in prop::collection::btree_map("[a-zA-Z0-9]{1,12}", any::<bool>(), 0..4),
        presented in "[a-zA-Z0-9]{0,12}",
        pick in any::<prop::sample::Index>(),
        use_configured in any::<bool>(),
        in_query in any::<bool>(),
    ) {
        let json = serde_json::to_string(
            &configured
                .iter()
                .map(|(token, editor)| {
                    (
                        token.clone(),
                        serde_json::json!({
                            "name": token,
                            "role": if *editor { "editor" } else { "viewer" },
                        }),
                    )
                })
                .collect::<serde_json::Map<_, _>>(),
        )
        .unwrap();
        let tokens: Vec<&String> = configured.keys().collect();
        let presented = if use_configured && !tokens.is_empty() {
            pick.get(&tokens).to_string()
        } else {
            presented
        };
        let valid = configured.contains_key(&presented);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let code = runtime.block_on(async {
            let server = Server::new().with_tokens(Tokens::from_json(&json).unwrap());
            if in_query {
                status(&server, Method::GET, &format!("/documents?token={presented}"), None).await
            } else {
                status(&server, Method::GET, "/documents", Some(&presented)).await
            }
        });
        prop_assert_eq!(code == StatusCode::OK, valid, "{} -> {}", presented, code);
        prop_assert!(code == StatusCode::OK || code == StatusCode::UNAUTHORIZED);
    }
}
