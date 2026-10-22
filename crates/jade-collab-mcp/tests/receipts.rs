mod common;

use common::{statuses, Agent};
use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::ReplicaId;
use jade_collab_server::wire::{ClientMessage, Format, ServerMessage};
use jade_collab_sync::protocol::{Committed, Rejected};
use jade_docx_collab::DocxModel;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn timed_out_receipts_keep_late_verdicts_and_wait_only_for_their_own_batch() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/a.docx/ws",
        listener.local_addr().unwrap()
    );
    let (release_first, first) = tokio::sync::oneshot::channel();
    let (release_second, second) = tokio::sync::oneshot::channel();
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
                    snapshot: serde_json::to_value(DocxModel::from_paragraphs(&["Hello"])).unwrap(),
                }
                .to_json()
                .into(),
            ))
            .await
            .unwrap();
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let ClientMessage::Submit(submission) = serde_json::from_str(&message).unwrap() else {
            panic!("expected an edit")
        };
        first.await.unwrap();
        socket
            .send(Message::Text(
                ServerMessage::Committed(Committed {
                    seq: 1,
                    origin: ReplicaId(1),
                    client_seq: submission.client_seq,
                    base_seq: submission.base_seq,
                    op: submission.op,
                })
                .to_json()
                .into(),
            ))
            .await
            .unwrap();
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let ClientMessage::Submit(submission) = serde_json::from_str(&message).unwrap() else {
            panic!("expected an edit")
        };
        second.await.unwrap();
        socket
            .send(Message::Text(
                ServerMessage::Rejected(Rejected {
                    client_seq: submission.client_seq,
                    reason: jade_collab_sync::Rejection::new("conflict after the timeout"),
                })
                .to_json()
                .into(),
            ))
            .await
            .unwrap();
        // Keep the socket live until leave_document, so a close cannot stand
        // in for the rejection notification (which does not advance seq).
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_))) {
                break;
            }
        }
    });
    let mut agent = Agent::new();
    let session = agent.join(&url).await;
    let first = agent.tool("apply_edits",json!({"session_id":session,"timeout_ms":0,"edits":[{"type":"insert_text","paragraph":0,"offset":0,"text":"x"}]})).await;
    assert_eq!(statuses(&first), ["pending"]);
    assert!(
        first["receipt"].is_string(),
        "a pending result must be queryable: {first}"
    );
    assert_eq!(first["complete"], false);
    release_first.send(()).unwrap();
    agent
        .tool(
            "wait_for_changes",
            json!({"session_id":session,"after_seq":0}),
        )
        .await;
    let second = agent.tool("apply_edits",json!({"session_id":session,"timeout_ms":0,"edits":[{"type":"insert_text","paragraph":0,"offset":0,"text":"y"}]})).await;
    assert_eq!(statuses(&second), ["pending"]);
    let confirmed = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.tool(
            "get_edit_status",
            json!({"session_id":session,"receipt":first["receipt"],"timeout_ms":10_000}),
        ),
    )
    .await
    .expect("later pending edits must not delay an answered receipt");
    assert_eq!(statuses(&confirmed), ["applied"]);
    assert_eq!(confirmed["complete"], true);
    assert_eq!(confirmed["pending"], 1);
    release_second.send(()).unwrap();
    let rejected = agent
        .tool(
            "get_edit_status",
            json!({"session_id":session,"receipt":second["receipt"],"timeout_ms":1_000}),
        )
        .await;
    assert_eq!(statuses(&rejected), ["rejected"]);
    assert_eq!(
        rejected["results"][0]["reason"],
        "conflict after the timeout"
    );
    assert_eq!(rejected["complete"], true);
    assert_eq!(
        rejected["seq"], 1,
        "rejections can finish receipts without advancing the document log"
    );
    assert_eq!(
        agent.read_all(&session).await["paragraphs"][0]["text"],
        "xHello"
    );
    let again = agent
        .tool(
            "get_edit_status",
            json!({"session_id":session,"receipt":first["receipt"]}),
        )
        .await;
    assert_eq!(statuses(&again), ["applied"]);
    agent
        .tool("leave_document", json!({"session_id":session}))
        .await;
    peer.await.unwrap();
}

#[tokio::test]
async fn closed_connections_keep_unanswered_receipts_unknown_instead_of_claiming_rejection() {
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
                    snapshot: serde_json::to_value(DocxModel::from_paragraphs(&["Hello"])).unwrap(),
                }
                .to_json()
                .into(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        socket.close(None).await.unwrap();
    });
    let mut agent = Agent::new();
    let session = agent.join(&url).await;
    let submitted = agent.tool("apply_edits",json!({"session_id":session,"edits":[{"type":"insert_text","paragraph":0,"offset":0,"text":"x"}]})).await;
    assert!(submitted["receipt"].is_string());
    assert_eq!(statuses(&submitted), ["pending"]);
    assert_eq!(submitted["complete"], false);
    assert!(submitted["connection_error"].is_string());
    let status = agent
        .tool(
            "get_edit_status",
            json!({"session_id":session,"receipt":submitted["receipt"]}),
        )
        .await;
    assert_eq!(statuses(&status), ["pending"]);
    assert_eq!(status["complete"], false);
    assert!(status["connection_error"].is_string());
    let before = agent.read_all(&session).await;
    assert!(agent.tool_failure("apply_edits",json!({"session_id":session,"edits":[{"type":"insert_text","paragraph":0,"offset":0,"text":"must not queue"}]})).await.contains("closed"));
    let after = agent.read_all(&session).await;
    assert_eq!(after["paragraphs"], before["paragraphs"]);
    assert_eq!(after["pending"], before["pending"]);
    peer.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn receipts_are_scoped_bounded_and_cleaned_up_for_every_format() {
    let hosted = common::Hosted::start().await;
    hosted
        .server
        .open(
            "deck.pptx",
            common::fixture("public/fixtures/sample-deck.pptx"),
        )
        .unwrap();
    for (name, edit) in [
        (
            common::DOCX,
            json!({"type":"insert_text","paragraph":1,"offset":0,"text":"r"}),
        ),
        (
            common::XLSX,
            json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"number","value":9}}),
        ),
        (
            "deck.pptx",
            json!({"type":"set_slide_hidden","slide":1,"hidden":true}),
        ),
    ] {
        let mut agent = Agent::new();
        let session = agent.join(&hosted.url(name)).await;
        let other = agent.join(&hosted.url(name)).await;
        let accepted = agent.apply(&session, json!([edit])).await;
        assert_eq!(statuses(&accepted), ["applied"]);
        assert_eq!(accepted["complete"], true);
        assert_eq!(accepted["unresolved"], 0);
        assert!(agent
            .tool_failure(
                "get_edit_status",
                json!({"session_id":other,"receipt":accepted["receipt"]})
            )
            .await
            .contains("session"));
        let mut restarted = Agent::new();
        let restarted_session = restarted.join(&hosted.url(name)).await;
        let new = restarted.apply(&restarted_session, json!([])).await;
        assert_ne!(new["receipt"], accepted["receipt"]);
        assert!(restarted
            .tool_failure(
                "get_edit_status",
                json!({"session_id":restarted_session,"receipt":accepted["receipt"]})
            )
            .await
            .contains("unavailable"));
        let seq = hosted.server.seq(name).await.unwrap();
        let mut last = accepted.clone();
        for _ in 0..64 {
            last = agent.apply(&session, json!([])).await;
        }
        assert!(agent
            .tool_failure(
                "get_edit_status",
                json!({"session_id":session,"receipt":accepted["receipt"]})
            )
            .await
            .contains("expired"));
        assert_eq!(
            agent
                .tool(
                    "get_edit_status",
                    json!({"session_id":session,"receipt":last["receipt"]})
                )
                .await["complete"],
            true
        );
        assert_eq!(
            hosted.server.seq(name).await.unwrap(),
            seq,
            "queries/no-op receipts do not change the document log"
        );
        agent
            .tool("leave_document", json!({"session_id":session}))
            .await;
        assert_eq!(
            agent
                .rpc_error(
                    "get_edit_status",
                    json!({"session_id":session,"receipt":last["receipt"]})
                )
                .await
                .0,
            -32602
        );
        assert!(hosted.server.faults(name).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn unresolved_receipt_capacity_refuses_before_allocating_or_submitting_more_edits() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/a.docx/ws",
        listener.local_addr().unwrap()
    );
    let (release, ready) = tokio::sync::oneshot::channel();
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
                    snapshot: serde_json::to_value(DocxModel::from_paragraphs(&["Hello"])).unwrap(),
                }
                .to_json()
                .into(),
            ))
            .await
            .unwrap();
        let mut submissions = Vec::new();
        for _ in 0..64 {
            let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let ClientMessage::Submit(submission) = serde_json::from_str(&text).unwrap() else {
                panic!("expected edit")
            };
            submissions.push(submission);
        }
        ready.await.unwrap();
        let first = &submissions[0];
        socket
            .send(Message::Text(
                ServerMessage::Committed(Committed {
                    seq: 1,
                    origin: ReplicaId(1),
                    client_seq: first.client_seq,
                    base_seq: first.base_seq,
                    op: first.op.clone(),
                })
                .to_json()
                .into(),
            ))
            .await
            .unwrap();
        let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let ClientMessage::Submit(next) = serde_json::from_str(&text).unwrap() else {
            panic!("expected edit")
        };
        assert_eq!(next.client_seq, 65);
        let last: jade_docx_collab::DocxOp =
            serde_json::from_value(submissions[63].op.clone()).unwrap();
        let next: jade_docx_collab::DocxOp = serde_json::from_value(next.op).unwrap();
        let (
            jade_docx_collab::DocxOp::InsertText { first: old, .. },
            jade_docx_collab::DocxOp::InsertText { first: new, .. },
        ) = (last, next)
        else {
            panic!("text edits")
        };
        assert_eq!(
            new.lamport,
            old.lamport + 1,
            "capacity refusal must not allocate identifiers"
        );
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_))) {
                break;
            }
        }
    });
    let mut agent = Agent::new();
    let session = agent.join(&url).await;
    for _ in 0..64 {
        let pending = agent.tool("apply_edits",json!({"session_id":session,"timeout_ms":0,"edits":[{"type":"insert_text","paragraph":0,"offset":0,"text":"x"}]})).await;
        assert_eq!(statuses(&pending), ["pending"]);
    }
    let before = agent.read_all(&session).await;
    assert_eq!(before["pending"], 64);
    assert!(agent.tool_failure("apply_edits",json!({"session_id":session,"timeout_ms":0,"edits":[{"type":"insert_text","paragraph":0,"offset":0,"text":"blocked"}]})).await.contains("unresolved"));
    assert_eq!(
        agent.read_all(&session).await["paragraphs"],
        before["paragraphs"]
    );
    release.send(()).unwrap();
    agent
        .tool(
            "wait_for_changes",
            json!({"session_id":session,"after_seq":0}),
        )
        .await;
    let next = agent.tool("apply_edits",json!({"session_id":session,"timeout_ms":0,"edits":[{"type":"insert_text","paragraph":0,"offset":0,"text":"y"}]})).await;
    assert_eq!(statuses(&next), ["pending"]);
    agent
        .tool("leave_document", json!({"session_id":session}))
        .await;
    peer.await.unwrap();
}
