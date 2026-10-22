use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_server::wire::{ClientMessage, ServerMessage};
use jade_collab_server::{Server, StorageOptions};
use jade_collab_sync::protocol::Submission;
use jade_docx_collab::{Anchor, DocxModel, DocxOp};
use std::time::Duration;
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
fn table(replica: ReplicaId, rows: usize, columns: usize) -> DocxOp {
    let id = |n| Id::new(1_000_000 + n, replica);
    DocxOp::InsertTable {
        anchor: Anchor::Start,
        id: id(0),
        rows: (1..=rows as u64).map(id).collect(),
        columns: (rows as u64 + 1..=rows as u64 + columns as u64)
            .map(id)
            .collect(),
    }
}
async fn send(socket: &mut Socket, message: ClientMessage) {
    socket
        .send(Message::Text(
            serde_json::to_string(&message).unwrap().into(),
        ))
        .await
        .unwrap();
}
async fn receive(socket: &mut Socket) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = socket.next().await.unwrap().unwrap();
            if let Message::Text(text) = message {
                return serde_json::from_str(&text).unwrap();
            }
        }
    })
    .await
    .expect("a resource refusal must not stop the room")
}
async fn start(server: &Server) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/a.docx/ws",
        listener.local_addr().unwrap()
    );
    (url, tokio::spawn(server.clone().serve(listener)))
}
#[tokio::test]
async fn table_expansion_refusal_is_durable_and_a_valid_retry_can_reuse_every_id() {
    for checkpoint_every in [1, 1000] {
        let root = tempfile::tempdir().unwrap();
        let options = StorageOptions {
            checkpoint_every,
            keep_tail: 8,
            ..StorageOptions::default()
        };
        let server = Server::with_storage_options(root.path(), options).unwrap();
        server
            .open(
                "a.docx",
                std::fs::read(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../jade-core/hello.docx"),
                )
                .unwrap(),
            )
            .unwrap();
        let before = server.export("a.docx").await.unwrap();
        let (url, serving) = start(&server).await;
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        send(&mut socket, ClientMessage::Join).await;
        let ServerMessage::Welcome { replica, .. } = receive(&mut socket).await else {
            panic!("welcome")
        };
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 1,
                base_seq: 0,
                op: serde_json::to_value(table(replica, 101, 100)).unwrap(),
            }),
        )
        .await;
        let ServerMessage::Rejected(rejected) = receive(&mut socket).await else {
            panic!("resource refusal")
        };
        assert!(rejected.reason.message().contains("10000 cell positions"));
        assert_eq!(server.seq("a.docx").await.unwrap(), 0);
        assert_eq!(server.export("a.docx").await.unwrap(), before);
        assert!(server.faults("a.docx").await.unwrap().is_empty());
        drop(socket);
        serving.abort();
        let _ = serving.await;
        drop(server);
        let server = Server::with_storage_options(root.path(), options).unwrap();
        let (url, serving) = start(&server).await;
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        send(&mut socket, ClientMessage::Resume { replica, seq: 0 }).await;
        assert!(matches!(
            receive(&mut socket).await,
            ServerMessage::Resumed {
                processed_client_seq: 1,
                ..
            }
        ));
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 1,
                base_seq: 0,
                op: serde_json::to_value(table(replica, 2, 3)).unwrap(),
            }),
        )
        .await;
        send(&mut socket, ClientMessage::Inspect { request: 7 }).await;
        assert!(
            matches!(
                receive(&mut socket).await,
                ServerMessage::Inspected {
                    request: 7,
                    seq: 0,
                    ..
                }
            ),
            "retrying an answered ticket cannot revive it"
        );
        send(
            &mut socket,
            ClientMessage::Submit(Submission {
                client_seq: 2,
                base_seq: 0,
                op: serde_json::to_value(table(replica, 2, 3)).unwrap(),
            }),
        )
        .await;
        assert!(matches!(receive(&mut socket).await,ServerMessage::Committed(c) if c.seq==1));
        send(&mut socket, ClientMessage::Inspect { request: 8 }).await;
        let ServerMessage::Inspected { snapshot, .. } = receive(&mut socket).await else {
            panic!("inspection")
        };
        let model: DocxModel = serde_json::from_value(snapshot).unwrap();
        assert_eq!(
            (
                model.tables()[0].rows.len(),
                model.tables()[0].columns.len()
            ),
            (2, 3)
        );
        assert!(server.faults("a.docx").await.unwrap().is_empty());
        let exported = server.export("a.docx").await.unwrap();
        drop(socket);
        serving.abort();
        let _ = serving.await;
        drop(server);
        let restored = Server::with_storage_options(root.path(), options).unwrap();
        assert_eq!(restored.seq("a.docx").await.unwrap(), 1);
        assert_eq!(restored.export("a.docx").await.unwrap(), exported);
    }
}
