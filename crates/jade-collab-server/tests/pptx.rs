//! Decks over real sockets: hosted, edited concurrently, exported, and
//! surviving a restart.

use std::path::PathBuf;
use std::time::Duration;

use jade_collab_server::{Client, Format, Server};
use jade_pptx_collab::intent::{resolve, PptxIntent, Target};
use jade_pptx_collab::{JadePptx, PptxModel};

fn deck() -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core/public/fixtures/sample-deck.pptx"),
    )
    .unwrap()
}

async fn listen(server: &Server) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(server.clone().serve(listener));
    (format!("ws://{addr}/documents/deck.pptx/ws"), task)
}

async fn catch_up(server: &Server, client: &Client<PptxModel>) {
    let seq = server.seq("deck.pptx").await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), client.wait_for_seq(seq))
        .await
        .expect("caught up in time")
        .unwrap();
}

/// The first slide's first shape with editable text, as (slide, shape number).
fn editable(model: &PptxModel) -> (usize, u32) {
    for (s, slide) in model.slides().iter().enumerate() {
        if let Some(shape) = slide.shapes.iter().find(|shape| shape.editable) {
            return (s + 1, shape.number);
        }
    }
    panic!("the deck has an editable shape");
}

fn apply(client: &Client<PptxModel>, intent: PptxIntent) {
    let view = client.view();
    for op in resolve(&view, &intent, &mut |n| client.reserve(n)).unwrap() {
        client.submit(op).unwrap();
    }
}

fn texts(model: &PptxModel) -> Vec<Vec<Option<String>>> {
    model
        .slides()
        .iter()
        .map(|slide| {
            slide
                .shapes
                .iter()
                .map(|shape| shape.text.clone())
                .collect()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn two_people_edit_a_deck_and_the_file_matches() {
    let server = Server::new();
    assert_eq!(server.open("deck.pptx", deck()).unwrap(), Format::Pptx);
    let (url, _task) = listen(&server).await;
    let a: Client<PptxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<PptxModel> = Client::connect(&url, None).await.unwrap();
    let (slide, shape) = editable(&a.view());

    apply(
        &a,
        PptxIntent::InsertText {
            slide,
            target: Target::Shape(shape),
            paragraph: 0,
            offset: 0,
            text: "A: ".into(),
        },
    );
    apply(
        &b,
        PptxIntent::InsertText {
            slide,
            target: Target::Notes,
            paragraph: 0,
            offset: 0,
            text: "speaker note from B".into(),
        },
    );
    apply(&b, PptxIntent::DuplicateSlide { slide });
    a.settle().await.unwrap();
    b.settle().await.unwrap();
    catch_up(&server, &a).await;
    catch_up(&server, &b).await;

    let view = a.view();
    assert_eq!(view, b.view());
    assert!(texts(&view)[slide - 1]
        .iter()
        .flatten()
        .any(|t| t.starts_with("A: ")));
    assert!(server.faults("deck.pptx").await.unwrap().is_empty());

    let exported = JadePptx::open(&server.export("deck.pptx").await.unwrap()).unwrap();
    let reloaded = PptxModel::from_pptx(exported.bytes()).unwrap();
    assert_eq!(texts(&reloaded), texts(&view));
    assert_eq!(reloaded.slides().len(), view.slides().len());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hosted_deck_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::with_storage(dir.path()).unwrap();
    server.open("deck.pptx", deck()).unwrap();
    let (url, task) = listen(&server).await;
    let a: Client<PptxModel> = Client::connect(&url, None).await.unwrap();
    let (slide, shape) = editable(&a.view());
    apply(
        &a,
        PptxIntent::InsertText {
            slide,
            target: Target::Shape(shape),
            paragraph: 0,
            offset: 0,
            text: "kept ".into(),
        },
    );
    a.settle().await.unwrap();
    let before = server.export("deck.pptx").await.unwrap();
    let replica = a.disconnect().await;
    task.abort();
    let _ = task.await;
    drop(server);

    let server = Server::with_storage(dir.path()).unwrap();
    let (url, _task) = listen(&server).await;
    let a = Client::resume(&url, &replica, None).await.unwrap();
    catch_up(&server, &a).await;
    let after = server.export("deck.pptx").await.unwrap();
    let texts_of = |bytes: &[u8]| texts(&PptxModel::from_pptx(bytes).unwrap());
    assert_eq!(texts_of(&after), texts_of(&before));
    assert_eq!(texts(&a.view()), texts_of(&after));
}
