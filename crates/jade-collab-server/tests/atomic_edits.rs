use jade_collab_server::{Client, ClientEdit, Server};
use jade_docx_collab::{DocxModel, DocxOp, StoryId};
use std::sync::{Arc, Barrier};

async fn host() -> (Server, String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let server = Server::new();
    server
        .open(
            "hello.docx",
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx"),
            )
            .unwrap(),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/hello.docx/ws",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(server.clone().serve(listener));
    (server, url, task)
}

fn insert(edit: &mut ClientEdit<'_, DocxModel>, text: &str) -> u64 {
    let paragraph = edit.view().paragraphs().remove(1);
    let op = DocxOp::InsertText {
        story: StoryId::Body,
        parent: paragraph.chars.last().copied(),
        first: edit.try_reserve(text.chars().count() as u64).unwrap(),
        text: text.into(),
    };
    edit.submit(op).unwrap()
}

#[tokio::test]
async fn waiting_for_a_submission_prefix_validates_tickets_and_keeps_verdict_events() {
    let (_server, url, task) = host().await;
    let client: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    client.settle_through(0).await.unwrap();
    assert!(matches!(
        client.settle_through(1).await,
        Err(jade_collab_server::ClientError::Protocol(_))
    ));
    let last = client
        .edit(None, |edit| {
            insert(edit, "x");
            insert(edit, "y")
        })
        .unwrap();
    assert!(matches!(
        client.settle_through(last + 1).await,
        Err(jade_collab_server::ClientError::Protocol(_))
    ));
    client.settle_through(last).await.unwrap();
    let answers: Vec<_> = client
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            jade_collab_sync::ReplicaEvent::Acknowledged { client_seq } => Some(client_seq),
            _ => None,
        })
        .collect();
    assert_eq!(answers, vec![last - 1, last]);
    client.settle_through(last - 1).await.unwrap();
    client.disconnect().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_one_competing_edit_can_use_a_snapshot_and_foreign_snapshots_are_refused() {
    let (server, url, task) = host().await;
    let client: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let other: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let foreign = other.snapshot();
    assert!(client
        .edit(Some(&foreign), |_| panic!("foreign planner must not run"))
        .is_err());
    let snapshot = client.snapshot();
    let barrier = Arc::new(Barrier::new(2));
    let applied = std::thread::scope(|scope| {
        let jobs: Vec<_> = ["x", "y"]
            .into_iter()
            .map(|text| {
                let (barrier, snapshot, client) = (barrier.clone(), snapshot.clone(), &client);
                scope.spawn(move || {
                    barrier.wait();
                    client
                        .edit(Some(&snapshot), |edit| insert(edit, text))
                        .is_ok()
                })
            })
            .collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .filter(|applied| *applied)
            .count()
    });
    assert_eq!(applied, 1);
    client.settle().await.unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), 1);
    assert!(client
        .edit(Some(&snapshot), |_| panic!("stale planner must not run"))
        .is_err());
    let fresh = client.snapshot();
    client
        .edit(Some(&fresh), |edit| {
            insert(edit, "a");
            insert(edit, "b");
        })
        .unwrap();
    client.settle().await.unwrap();
    assert_eq!(server.seq("hello.docx").await.unwrap(), 3);
    assert!(server.faults("hello.docx").await.unwrap().is_empty());
    client.disconnect().await;
    other.disconnect().await;
    task.abort();
    let _ = task.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_client_closes_its_connection_even_if_a_read_snapshot_is_retained() {
    let (_server, url, task) = host().await;
    let observer: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let client: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let id = client.id();
    client.set_presence(serde_json::json!({"cursor":1}));
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        observer.wait_for_presence(|p| p.contains_key(&id)),
    )
    .await
    .unwrap()
    .unwrap();
    let retained = client.snapshot();
    drop(client);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        observer.wait_for_presence(|p| !p.contains_key(&id)),
    )
    .await
    .expect("dropping the client closes its socket")
    .unwrap();
    assert_eq!(retained.view.paragraph_texts()[1], "World");
    observer.disconnect().await;
    task.abort();
    let _ = task.await;
}
