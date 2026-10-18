//! Tracked changes over the socket: the server names each revision's author
//! from the token that made it, before the first tracked edit, and no
//! client can name anyone.

use std::path::PathBuf;
use std::time::Duration;

use jade_collab_core::id::ReplicaId;
use jade_collab_server::{Client, Server, Tokens};
use jade_collab_sync::ReplicaEvent;
use jade_docx_collab::intent::{resolve_with, Context, IntentOp};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx, RevisionKind};

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
    "agent-secret": { "name": "claude", "role": "editor" }
}"#;

async fn listen(server: &Server) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.clone().serve(listener));
    format!("ws://{addr}/documents/hello.docx/ws")
}

fn edit(client: &Client<DocxModel>, intent: IntentOp) {
    let view = client.view();
    let context = Context {
        replica: Some(client.id()),
        date: Some("2026-10-18T12:00:00Z".into()),
    };
    for op in resolve_with(&view, &intent, &context, &mut |n| client.reserve(n)).unwrap() {
        client.submit(op).unwrap();
    }
}

async fn settle(client: &Client<DocxModel>) {
    tokio::time::timeout(Duration::from_secs(20), client.settle())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn revisions_are_authored_by_the_token_that_made_them() {
    let server = Server::new().with_tokens(Tokens::from_json(TOKENS).unwrap());
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let url = listen(&server).await;
    let ada: Client<DocxModel> = Client::connect(&url, Some("ada-secret")).await.unwrap();
    let agent: Client<DocxModel> = Client::connect(&url, Some("agent-secret")).await.unwrap();
    // Joining names nobody: untracked editing logs nothing extra.
    assert_eq!(server.seq("hello.docx").await.unwrap(), 0);

    edit(&ada, IntentOp::SetTracking { on: true });
    settle(&ada).await;
    edit(
        &ada,
        IntentOp::InsertText {
            paragraph: 1,
            offset: 0,
            text: "Ada ".into(),
        },
    );
    settle(&ada).await;
    let seq = server.seq("hello.docx").await.unwrap();
    agent.wait_for_seq(seq).await.unwrap();
    edit(
        &agent,
        IntentOp::DeleteText {
            paragraph: 1,
            offset: 4,
            direction: Default::default(),
            count: 5,
        },
    );
    settle(&agent).await;
    let seq = server.seq("hello.docx").await.unwrap();
    ada.wait_for_seq(seq).await.unwrap();
    assert_eq!(ada.view(), agent.view());

    let view = ada.view();
    let authors: Vec<(RevisionKind, Option<String>, String)> = view
        .revisions()
        .into_iter()
        .map(|r| (r.revision.kind, r.revision.author, r.text))
        .collect();
    assert_eq!(
        authors,
        [
            (RevisionKind::Insertion, Some("Ada".into()), "Ada ".into()),
            (
                RevisionKind::Deletion,
                Some("claude".into()),
                "World".into()
            ),
        ]
    );
    assert_eq!(view.author(ada.id()), Some("Ada"));

    // The file says the same.
    let jade = JadeDocx::load(&server.export("hello.docx").await.unwrap()).unwrap();
    let reloaded = DocxModel::from_word_document(jade.word()).unwrap();
    let in_file: Vec<(RevisionKind, Option<String>, String)> = reloaded
        .revisions()
        .into_iter()
        .map(|r| (r.revision.kind, r.revision.author, r.text))
        .collect();
    assert_eq!(in_file, authors);
    assert!(server.faults("hello.docx").await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_cannot_name_an_author() {
    let server = Server::new().with_tokens(Tokens::from_json(TOKENS).unwrap());
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let url = listen(&server).await;
    let ada: Client<DocxModel> = Client::connect(&url, Some("ada-secret")).await.unwrap();
    // Submitted directly (the client's own view refuses it first).
    let forged = DocxOp::SetAuthor {
        replica: ReplicaId(ada.id().0),
        name: "Somebody else".into(),
    };
    assert!(ada.submit(forged).is_err());
    settle(&ada).await;
    assert_eq!(server.seq("hello.docx").await.unwrap(), 0);
    let events = ada.take_events();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ReplicaEvent::Acknowledged { .. })),
        "{events:?}"
    );
}
