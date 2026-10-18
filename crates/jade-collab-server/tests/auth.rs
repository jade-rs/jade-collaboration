//! Bearer tokens: with tokens configured, every route and the socket need
//! one; viewers read and never write; a token may be limited to some
//! documents, with a role of its own on each.

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use jade_collab_server::{Client, Grant, Role, Server, Tokens};
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
    response(server, method, uri, bearer).await.status()
}

async fn response(
    server: &Server,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
) -> axum::response::Response {
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
}

/// The names `GET /documents` lists.
async fn listed(server: &Server, bearer: &str) -> Vec<String> {
    let response = response(server, Method::GET, "/documents", Some(bearer)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    let mut names: Vec<String> = list
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
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

const SCOPED: &str = r#"{
    "everything": { "name": "Eve", "role": "editor" },
    "two": { "name": "Tom", "role": "editor", "documents": ["hello.docx", "deck-?.docx"] },
    "mixed": { "name": "Mia", "role": "viewer",
               "documents": ["notes.docx", { "document": "hello.docx", "role": "editor" }, "*.docx"] },
    "nothing": { "name": "Nil", "role": "editor", "documents": [] }
}"#;

fn scoped() -> Server {
    let server = Server::new().with_tokens(Tokens::from_json(SCOPED).unwrap());
    for name in ["hello.docx", "deck-1.docx", "deck-10.docx", "notes.docx"] {
        server.open(name, fixture("hello.docx")).unwrap();
    }
    server
}

#[test]
fn documents_and_per_document_roles_are_read_from_json() {
    let tokens = Tokens::from_json(SCOPED).unwrap();
    let everything = tokens.grant("everything").unwrap();
    assert_eq!(everything.role_for("anything.xlsx"), Some(Role::Editor));
    let two = tokens.grant("two").unwrap();
    assert_eq!(two.role_for("hello.docx"), Some(Role::Editor));
    assert_eq!(two.role_for("deck-1.docx"), Some(Role::Editor));
    assert_eq!(two.role_for("deck-10.docx"), None, "? is one character");
    assert_eq!(two.role_for("Hello.docx"), None, "names are case-sensitive");
    let mixed = tokens.grant("mixed").unwrap();
    assert_eq!(mixed.role_for("hello.docx"), Some(Role::Editor));
    assert_eq!(mixed.role_for("notes.docx"), Some(Role::Viewer));
    assert_eq!(mixed.role_for("deck-1.docx"), Some(Role::Viewer));
    assert_eq!(mixed.role_for("book.xlsx"), None);
    assert_eq!(
        tokens.grant("nothing").unwrap().role_for("hello.docx"),
        None
    );

    for invalid in [
        r#"{"t": {"name": "x", "role": "editor", "documents": "hello.docx"}}"#,
        r#"{"t": {"name": "x", "role": "editor", "documents": [1]}}"#,
        r#"{"t": {"name": "x", "role": "editor", "documents": [""]}}"#,
        r#"{"t": {"name": "x", "role": "editor", "documents": [{"document": "a", "role": "owner"}]}}"#,
        r#"{"t": {"name": "x", "role": "editor", "documents": [{"document": "", "role": "viewer"}]}}"#,
        r#"{"t": {"name": "x", "role": "editor", "documents": [{"name": "a", "role": "viewer"}]}}"#,
    ] {
        assert!(Tokens::from_json(invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn a_token_sees_and_opens_only_its_documents() {
    let server = scoped();
    assert_eq!(
        listed(&server, "everything").await,
        ["deck-1.docx", "deck-10.docx", "hello.docx", "notes.docx"]
    );
    assert_eq!(listed(&server, "two").await, ["deck-1.docx", "hello.docx"]);
    assert!(listed(&server, "nothing").await.is_empty());

    for (token, name, expected) in [
        ("two", "hello.docx", StatusCode::OK),
        ("two", "deck-10.docx", StatusCode::FORBIDDEN),
        ("two", "notes.docx", StatusCode::FORBIDDEN),
        // No access is no access, whether or not the document exists.
        ("two", "missing.docx", StatusCode::FORBIDDEN),
        ("everything", "missing.docx", StatusCode::NOT_FOUND),
        ("nothing", "hello.docx", StatusCode::FORBIDDEN),
        ("mixed", "notes.docx", StatusCode::OK),
    ] {
        let uri = format!("/documents/{name}");
        assert_eq!(
            status(&server, Method::GET, &uri, Some(token)).await,
            expected,
            "GET {uri} with {token}"
        );
        // The socket answers the same way before any upgrade.
        let socket = status(&server, Method::GET, &format!("{uri}/ws"), Some(token)).await;
        assert_eq!(
            socket == StatusCode::FORBIDDEN,
            expected == StatusCode::FORBIDDEN,
            "GET {uri}/ws with {token}: {socket}"
        );
    }
    // Uploading needs the editor role on that very name.
    for (token, name, expected) in [
        ("two", "deck-2.docx", StatusCode::CREATED),
        ("two", "deck-20.docx", StatusCode::FORBIDDEN),
        ("mixed", "new.docx", StatusCode::FORBIDDEN),
        ("mixed", "hello.docx", StatusCode::CONFLICT),
        ("nothing", "new.docx", StatusCode::FORBIDDEN),
    ] {
        let uri = format!("/documents/{name}");
        assert_eq!(
            status(&server, Method::PUT, &uri, Some(token)).await,
            expected,
            "PUT {uri} with {token}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_socket_applies_the_role_a_token_has_on_that_document() {
    let server = scoped();
    let addr = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(server.clone().serve(listener));
        addr
    };
    let url = |name: &str| format!("ws://{addr}/documents/{name}/ws");

    let refused = Client::<DocxModel>::connect(&url("notes.docx"), Some("two")).await;
    let error = refused.err().expect("no access to notes.docx");
    assert!(error.to_string().contains("403"), "{error}");

    // An editor on hello.docx, a viewer on notes.docx.
    let hello: Client<DocxModel> = Client::connect(&url("hello.docx"), Some("mixed"))
        .await
        .unwrap();
    type_text(&hello, "mixed ");
    hello.settle().await.unwrap();
    assert!(hello
        .take_events()
        .iter()
        .all(|e| !matches!(e, ReplicaEvent::Rejected { .. })));
    assert_eq!(server.seq("hello.docx").await.unwrap(), 1);

    let notes: Client<DocxModel> = Client::connect(&url("notes.docx"), Some("mixed"))
        .await
        .unwrap();
    type_text(&notes, "mixed ");
    tokio::time::timeout(Duration::from_secs(10), notes.settle())
        .await
        .unwrap()
        .unwrap();
    let events = notes.take_events();
    assert!(
        matches!(&events[..], [ReplicaEvent::Rejected { reason, .. }] if reason.message().contains("read-only")),
        "{events:?}"
    );
    assert_eq!(server.seq("notes.docx").await.unwrap(), 0);
}

/// A token's documents, as the test writes them.
#[derive(Clone, Debug)]
enum Rule {
    Pattern(String),
    WithRole(String, bool),
}

#[derive(Clone, Debug)]
struct Spec {
    editor: bool,
    documents: Option<Vec<Rule>>,
}

fn role(editor: bool) -> Role {
    if editor {
        Role::Editor
    } else {
        Role::Viewer
    }
}

fn role_name(editor: bool) -> &'static str {
    if editor {
        "editor"
    } else {
        "viewer"
    }
}

fn tokens_json(table: &std::collections::BTreeMap<String, Spec>) -> String {
    let object: serde_json::Map<String, serde_json::Value> = table
        .iter()
        .map(|(token, spec)| {
            let mut grant = serde_json::json!({ "name": token, "role": role_name(spec.editor) });
            if let Some(documents) = &spec.documents {
                grant["documents"] = documents
                    .iter()
                    .map(|rule| match rule {
                        Rule::Pattern(p) => serde_json::json!(p),
                        Rule::WithRole(p, editor) => {
                            serde_json::json!({ "document": p, "role": role_name(*editor) })
                        }
                    })
                    .collect();
            }
            (token.clone(), grant)
        })
        .collect();
    serde_json::to_string(&object).unwrap()
}

/// Whole-name glob matching, written independently of the server's: `*` is
/// any run of characters, `?` exactly one.
fn glob(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|skip| glob(rest, &name[skip..])),
        Some(('?', rest)) => !name.is_empty() && glob(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
}

/// What a token may do with a document: `None` for an unknown token,
/// `Some(None)` for no access.
fn reference(
    table: &std::collections::BTreeMap<String, Spec>,
    token: &str,
    document: &str,
) -> Option<Option<Role>> {
    let spec = table.get(token)?;
    let Some(rules) = &spec.documents else {
        return Some(Some(role(spec.editor)));
    };
    let name: Vec<char> = document.chars().collect();
    for rule in rules {
        let (pattern, editor) = match rule {
            Rule::Pattern(p) => (p, spec.editor),
            Rule::WithRole(p, editor) => (p, *editor),
        };
        if glob(&pattern.chars().collect::<Vec<_>>(), &name) {
            return Some(Some(role(editor)));
        }
    }
    Some(None)
}

const NAMES: [&str; 4] = ["a.docx", "ab.docx", "b.docx", "ba.docx"];

fn pattern() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec!["a", "b", "*", "?", ".docx", "."]),
        1..5,
    )
    .prop_map(|pieces| pieces.concat())
}

fn table() -> impl Strategy<Value = std::collections::BTreeMap<String, Spec>> {
    let rule = prop_oneof![
        pattern().prop_map(Rule::Pattern),
        (pattern(), any::<bool>()).prop_map(|(p, editor)| Rule::WithRole(p, editor)),
    ];
    let spec = (
        any::<bool>(),
        prop::option::of(prop::collection::vec(rule, 0..4)),
    )
        .prop_map(|(editor, documents)| Spec { editor, documents });
    prop::collection::btree_map("[a-c]{1,2}", spec, 0..5)
}

fn document() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(NAMES.to_vec()).prop_map(str::to_string),
        prop::collection::vec(
            prop::sample::select(vec!["a", "b", ".docx", ".", "*"]),
            0..5
        )
        .prop_map(|pieces| pieces.concat()),
    ]
}

/// Mostly a configured token, sometimes any.
fn presented(
    table: &std::collections::BTreeMap<String, Spec>,
    pick: prop::sample::Index,
    configured: bool,
    any: String,
) -> String {
    let tokens: Vec<&String> = table.keys().collect();
    if configured && !tokens.is_empty() {
        pick.get(&tokens).to_string()
    } else {
        any
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// A grant gives exactly the role an independent reading of the
    /// token table gives, for any table, token and document name.
    #[test]
    fn grants_agree_with_the_reference(
        table in table(),
        pick in any::<prop::sample::Index>(),
        configured in prop::bool::weighted(0.8),
        any_token in "[a-c]{1,2}",
        document in document(),
    ) {
        let token = presented(&table, pick, configured, any_token);
        let tokens = Tokens::from_json(&tokens_json(&table)).unwrap();
        let actual = tokens.grant(&token).map(|grant: &Grant| grant.role_for(&document));
        prop_assert_eq!(actual, reference(&table, &token, &document));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Every route enforces the reference: unknown tokens get 401, known
    /// tokens without access 403, the list shows exactly the accessible
    /// documents, and only editors of that name may upload.
    #[test]
    fn every_route_enforces_the_reference(
        table in table(),
        pick_token in any::<prop::sample::Index>(),
        configured in prop::bool::weighted(0.8),
        any_token in "[a-c]{1,2}",
        pick in any::<prop::sample::Index>(),
    ) {
        let token = presented(&table, pick_token, configured, any_token);
        let document = *pick.get(&NAMES);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let server = Server::new().with_tokens(Tokens::from_json(&tokens_json(&table)).unwrap());
            for name in NAMES {
                server.open(name, fixture("hello.docx")).unwrap();
            }
            let uri = format!("/documents/{document}");
            let get = status(&server, Method::GET, &uri, Some(&token)).await;
            let socket = status(&server, Method::GET, &format!("{uri}/ws"), Some(&token)).await;
            let put = status(&server, Method::PUT, &uri, Some(&token)).await;
            match reference(&table, &token, document) {
                None => {
                    for code in [get, socket, put] {
                        prop_assert_eq!(code, StatusCode::UNAUTHORIZED);
                    }
                    prop_assert_eq!(
                        status(&server, Method::GET, "/documents", Some(&token)).await,
                        StatusCode::UNAUTHORIZED
                    );
                    return Ok(());
                }
                Some(None) => {
                    prop_assert_eq!(get, StatusCode::FORBIDDEN);
                    prop_assert_eq!(socket, StatusCode::FORBIDDEN);
                    prop_assert_eq!(put, StatusCode::FORBIDDEN);
                }
                Some(Some(role)) => {
                    prop_assert_eq!(get, StatusCode::OK);
                    prop_assert_ne!(socket, StatusCode::FORBIDDEN);
                    prop_assert_eq!(put, if role == Role::Editor {
                        StatusCode::CONFLICT
                    } else {
                        StatusCode::FORBIDDEN
                    });
                }
            }
            let expected: Vec<String> = NAMES
                .iter()
                .filter(|name| matches!(reference(&table, &token, name), Some(Some(_))))
                .map(|name| name.to_string())
                .collect();
            prop_assert_eq!(listed(&server, &token).await, expected);
            Ok(())
        })?;
    }
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
        tweak in 0..3u8,
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
        // A configured token, or one a character longer or shorter.
        let presented = if use_configured && !tokens.is_empty() {
            let mut token = pick.get(&tokens).to_string();
            match tweak {
                1 => token.push('x'),
                2 => {
                    token.pop();
                }
                _ => {}
            }
            token
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
