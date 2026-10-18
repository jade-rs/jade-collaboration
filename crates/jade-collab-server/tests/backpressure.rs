//! A connection's outgoing queue is bounded: a client that stops reading is
//! dropped (told `overloaded`, then closed) instead of making the server
//! hold everything it is sent, and the others never wait for it. It comes
//! back by resuming, like after any disconnect.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use jade_collab_server::wire::{ClientMessage, ServerMessage};
use jade_collab_server::{Client, PresenceLimits, Server};
use jade_docx_collab::{DocxModel, DocxOp, StoryId};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}

async fn start(server: Server) -> String {
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.serve(listener));
    format!("ws://{addr}/documents/hello.docx/ws")
}

async fn send(socket: &mut Socket, message: &ClientMessage) {
    let text = serde_json::to_string(message).unwrap();
    socket.send(Message::Text(text.into())).await.unwrap();
}

/// Every server message until the socket ends.
async fn drain(socket: &mut Socket) -> Vec<ServerMessage> {
    let mut messages = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(20), socket.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                messages.push(serde_json::from_str(&text).unwrap())
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => return messages,
            Ok(Some(Ok(_))) => {}
            Err(_) => panic!("the socket stayed open after {} messages", messages.len()),
        }
    }
}

#[test]
fn overloaded_on_the_wire() {
    assert_eq!(
        serde_json::to_value(ServerMessage::Overloaded {
            message: "behind".into()
        })
        .unwrap(),
        json!({"type": "overloaded", "message": "behind"})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stops_reading_is_dropped_and_resumes() {
    let url = start(
        Server::new()
            .with_presence_limits(PresenceLimits::UNLIMITED)
            .with_outgoing_limit(8),
    )
    .await;
    // The slow client joins, then reads nothing.
    let (mut slow, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(&mut slow, &ClientMessage::Join).await;
    let (replica, snapshot) = loop {
        let Some(Ok(Message::Text(text))) = slow.next().await else {
            panic!("no welcome")
        };
        if let ServerMessage::Welcome {
            replica, snapshot, ..
        } = serde_json::from_str(&text).unwrap()
        {
            break (replica, snapshot);
        }
    };

    // Another floods it with presence far beyond what socket buffers hold.
    let busy: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let big = "x".repeat(64 * 1024);
    for k in 0..400 {
        busy.set_presence(json!({ "k": k, "padding": big }));
    }
    // The busy client is never held up: its operation commits.
    let _: DocxModel = serde_json::from_value(snapshot).unwrap();
    let first = busy.reserve(1);
    busy.submit(DocxOp::InsertText {
        story: StoryId::Body,
        parent: None,
        first,
        text: "a".into(),
    })
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), busy.settle())
        .await
        .expect("the busy client was held up")
        .unwrap();

    // The slow client finds it was dropped: told why, then closed.
    let messages = drain(&mut slow).await;
    assert!(
        matches!(messages.last(), Some(ServerMessage::Overloaded { .. })),
        "last of {} messages: {:?}",
        messages.len(),
        messages.last()
    );
    let presence = messages
        .iter()
        .filter(|m| matches!(m, ServerMessage::Presence { .. }))
        .count();
    assert!(presence < 400, "it was sent everything ({presence})");

    // It resumes and catches up on what it missed.
    let (mut back, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(&mut back, &ClientMessage::Resume { replica, seq: 0 }).await;
    let resumed = loop {
        let Some(Ok(Message::Text(text))) = back.next().await else {
            panic!("no answer to resume")
        };
        if let message @ ServerMessage::Resumed { .. } = serde_json::from_str(&text).unwrap() {
            break message;
        }
    };
    let ServerMessage::Resumed { missed, .. } = resumed else {
        unreachable!()
    };
    assert_eq!(missed.len(), 1);
    // And it is a subscriber again: the next commit reaches it.
    let second = busy.reserve(1);
    busy.submit(DocxOp::InsertText {
        story: StoryId::Body,
        parent: Some(first),
        first: second,
        text: "b".into(),
    })
    .unwrap();
    loop {
        let Some(Ok(Message::Text(text))) = back.next().await else {
            panic!("closed")
        };
        if let ServerMessage::Committed(committed) = serde_json::from_str(&text).unwrap() {
            assert_eq!(committed.seq, 2);
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_keeps_reading_is_not_dropped_by_the_default_limit() {
    let url = start(Server::new().with_presence_limits(PresenceLimits::UNLIMITED)).await;
    let reader: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let busy: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let mut parent = None;
    for _ in 0..50 {
        let first = busy.reserve(1);
        busy.submit(DocxOp::InsertText {
            story: StoryId::Body,
            parent,
            first,
            text: "z".into(),
        })
        .unwrap();
        parent = Some(first);
        busy.set_presence(json!({ "at": first }));
    }
    busy.settle().await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), reader.wait_for_seq(50))
        .await
        .expect("the reader fell behind")
        .unwrap();
    assert_eq!(reader.view(), busy.view());
}
