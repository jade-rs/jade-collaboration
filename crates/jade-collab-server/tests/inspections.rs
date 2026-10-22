use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::ReplicaId;
use jade_collab_server::wire::{ClientMessage, Format, ServerMessage};
use jade_collab_server::{Client, ClientError, Server, Tokens};
use jade_docx_collab::{DocxFormatting, DocxModel};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn losing_the_connection_finishes_inspection_and_future_requests_fail_promptly() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/a.docx/ws",
        listener.local_addr().unwrap()
    );
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(
                ServerMessage::Welcome {
                    format: Format::Docx,
                    replica: ReplicaId(1),
                    seq: 0,
                    snapshot: serde_json::to_value(DocxModel::from_paragraphs(&["x"])).unwrap(),
                }
                .to_json()
                .into(),
            ))
            .await
            .unwrap();
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(matches!(
            serde_json::from_str::<ClientMessage>(&message).unwrap(),
            ClientMessage::Inspect { .. }
        ));
        socket.close(None).await.unwrap();
    });
    let client: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), client.inspect())
            .await
            .unwrap(),
        Err(ClientError::Closed)
    ));
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), client.inspect())
            .await
            .unwrap(),
        Err(ClientError::Closed)
    ));
    client.disconnect().await;
    peer.await.unwrap();
}

#[tokio::test]
async fn inspection_uses_the_existing_viewer_authorization_and_does_not_advance_the_log() {
    let server = Server::new().with_tokens(
        Tokens::from_json(
            r#"{
      "view-secret":{"name":"reader","role":"viewer","documents":["a.docx","deck.pptx"]}
    }"#,
        )
        .unwrap(),
    );
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx"),
    )
    .unwrap();
    server.open("a.docx", bytes).unwrap();
    server
        .open(
            "deck.pptx",
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../jade-core/public/fixtures/inherited-deck.pptx"),
            )
            .unwrap(),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/a.docx/ws",
        listener.local_addr().unwrap()
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    assert!(Client::<DocxModel>::connect(&url, None).await.is_err());
    let viewer: Client<DocxModel> = Client::connect(&url, Some("view-secret")).await.unwrap();
    let initial = viewer.snapshot();
    let inspected = viewer.inspect().await.unwrap();
    assert_eq!(inspected.snapshot, initial);
    let formatting: DocxFormatting = serde_json::from_value(inspected.formatting).unwrap();
    formatting.validate(&inspected.snapshot.view).unwrap();
    assert_eq!(server.seq("a.docx").await.unwrap(), 0);
    let pptx_url = url.replace("a.docx/ws", "deck.pptx/ws");
    assert!(
        Client::<jade_pptx_collab::PptxModel>::connect(&pptx_url, None)
            .await
            .is_err()
    );
    let deck: Client<jade_pptx_collab::PptxModel> = Client::connect(&pptx_url, Some("view-secret"))
        .await
        .unwrap();
    let inspected = deck.inspect().await.unwrap();
    assert_eq!(inspected.snapshot, deck.snapshot());
    let format: jade_pptx_collab::PptxFormatting =
        serde_json::from_value(inspected.formatting).unwrap();
    format.validate(&inspected.snapshot.view).unwrap();
    assert_eq!(
        format.slides[0].shapes[0].paragraphs.as_ref().unwrap()[0].runs[0]
            .style
            .size_pt,
        30.0
    );
    assert_eq!(server.seq("deck.pptx").await.unwrap(), 0);
    deck.disconnect().await;
    viewer.disconnect().await;
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn inspection_follows_queued_edits_and_unsupported_formats_keep_their_connection() {
    let server = Server::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    server
        .open("a.docx", std::fs::read(root.join("hello.docx")).unwrap())
        .unwrap();
    server
        .open(
            "b.xlsx",
            std::fs::read(root.join("public/fixtures/grid-operations-workbook.xlsx")).unwrap(),
        )
        .unwrap();
    server
        .open(
            "deck.pptx",
            std::fs::read(root.join("public/fixtures/inherited-deck.pptx")).unwrap(),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}/documents", listener.local_addr().unwrap());
    let serving = tokio::spawn(server.clone().serve(listener));
    let docx: Client<DocxModel> = Client::connect(&format!("{base}/a.docx/ws"), None)
        .await
        .unwrap();
    docx.edit(None, |context| {
        let first = context.try_reserve(1).unwrap();
        context
            .submit(jade_docx_collab::DocxOp::InsertText {
                story: jade_docx_collab::StoryId::Body,
                parent: None,
                first,
                text: "x".into(),
            })
            .unwrap();
    })
    .unwrap();
    let inspected = docx.inspect().await.unwrap();
    assert_eq!(inspected.snapshot, docx.snapshot());
    assert!(inspected.snapshot.view.paragraph_texts()[0].starts_with('x'));
    assert_eq!(inspected.snapshot.seq, 1);
    docx.edit(Some(&inspected.snapshot), |_| ()).unwrap();
    let xlsx: Client<jade_xlsx_collab::XlsxModel> =
        Client::connect(&format!("{base}/b.xlsx/ws"), None)
            .await
            .unwrap();
    assert!(matches!(
        xlsx.inspect().await,
        Err(ClientError::Inspection(_))
    ));
    let deck: Client<jade_pptx_collab::PptxModel> =
        Client::connect(&format!("{base}/deck.pptx/ws"), None)
            .await
            .unwrap();
    deck.try_edit(None, |context| {
        let view = context.view().clone();
        let ops = jade_pptx_collab::intent::try_resolve(
            &view,
            &jade_pptx_collab::intent::PptxIntent::InsertText {
                slide: 1,
                target: jade_pptx_collab::intent::Target::Shape(2),
                paragraph: 0,
                offset: 0,
                text: "日🙂".into(),
            },
            &mut |n| context.try_reserve(n),
        )
        .map_err(|e| e.to_string())?;
        for op in ops {
            context.submit(op).map_err(|e| e.to_string())?;
        }
        Ok::<(), String>(())
    })
    .unwrap();
    let inspected = deck.inspect().await.unwrap();
    assert_eq!(inspected.snapshot, deck.snapshot());
    assert_eq!(inspected.snapshot.seq, 1);
    assert_eq!(inspected.snapshot.pending, 0);
    assert!(inspected.snapshot.view.slides()[0].shapes[0]
        .text
        .as_deref()
        .unwrap()
        .starts_with("日🙂"));
    let format: jade_pptx_collab::PptxFormatting =
        serde_json::from_value(inspected.formatting).unwrap();
    format.validate(&inspected.snapshot.view).unwrap();
    deck.edit(Some(&inspected.snapshot), |_| ()).unwrap();
    deck.disconnect().await;
    xlsx.try_edit(None, |context| {
        let view = context.view().clone();
        let op = jade_xlsx_collab::intent::try_resolve(
            &view,
            &jade_xlsx_collab::intent::XlsxIntent::SetCell {
                sheet: "Operations".into(),
                reference: "E6".into(),
                value: jade_xlsx_collab::CellInput::Number(123.0),
            },
            &mut |n| context.try_reserve(n),
        )
        .map_err(|e| e.to_string())?;
        context.submit(op).map_err(|e| e.to_string())
    })
    .unwrap();
    xlsx.settle().await.unwrap();
    assert_eq!(server.seq("b.xlsx").await.unwrap(), 1);
    assert!(matches!(
        xlsx.inspect().await,
        Err(ClientError::Inspection(_))
    ));
    docx.disconnect().await;
    xlsx.disconnect().await;
    serving.abort();
    let _ = serving.await;
}
