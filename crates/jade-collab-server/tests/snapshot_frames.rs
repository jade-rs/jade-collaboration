//! Large welcomes may be one WebSocket frame, below the message budget.
use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::ReplicaId;
use jade_collab_server::{
    wire::{Format, ServerMessage},
    Client,
};
use jade_docx_collab::DocxModel;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn a_valid_welcome_above_the_default_frame_limit_is_accepted() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/large.docx/ws",
        listener.local_addr().unwrap()
    );
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        let model = DocxModel::from_paragraphs(&["large"]);
        let mut welcome = serde_json::to_value(ServerMessage::Welcome {
            format: Format::Docx,
            replica: ReplicaId(1),
            seq: 0,
            snapshot: serde_json::to_value(model).unwrap(),
        })
        .unwrap();
        // Unknown fields are allowed; keep the model small while exercising a
        // valid >16 MiB frame without allocating hundreds of thousands of atoms.
        welcome["padding"] = "x".repeat(17 * 1024 * 1024).into();
        socket
            .send(Message::Text(welcome.to_string().into()))
            .await
            .unwrap();
        let _ = socket.next().await;
    });
    let client = Client::<DocxModel>::connect(&url, None)
        .await
        .expect("a valid welcome below the 64 MiB message budget");
    assert_eq!(client.view().paragraph_texts(), ["large"]);
    client.disconnect().await;
    peer.await.unwrap();
}

#[tokio::test]
async fn fragmented_messages_still_have_the_64_mib_aggregate_limit() {
    use tokio_tungstenite::tungstenite::protocol::frame::{
        coding::{Data, OpCode},
        Frame,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/large.docx/ws",
        listener.local_addr().unwrap()
    );
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        for n in 0..3 {
            let opcode = if n == 0 { Data::Text } else { Data::Continue };
            let frame = Frame::message(vec![b'x'; 22 * 1024 * 1024], OpCode::Data(opcode), n == 2);
            if socket.send(Message::Frame(frame)).await.is_err() {
                break;
            }
        }
    });
    let error = Client::<DocxModel>::connect(&url, None)
        .await
        .err()
        .expect("66 MiB fragmented message must fail");
    assert!(
        error.to_string().contains("Space limit exceeded"),
        "{error}"
    );
    peer.await.unwrap();
}

#[tokio::test]
async fn an_oversized_frame_is_refused_from_its_header_without_waiting_for_its_body() {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/large.docx/ws",
        listener.local_addr().unwrap()
    );
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        let mut header = vec![0x81, 127];
        header.extend_from_slice(&((64u64 << 20) + 1).to_be_bytes());
        socket.get_mut().write_all(&header).await.unwrap();
        let _ = socket.next().await;
    });
    let error = Client::<DocxModel>::connect_with_timeout(&url, None, Duration::from_secs(2))
        .await
        .err()
        .expect("oversized frame header must fail");
    assert!(
        error.to_string().contains("Space limit exceeded"),
        "{error}"
    );
    peer.await.unwrap();
}
