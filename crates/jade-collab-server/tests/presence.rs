//! Presence: ephemeral per-replica state (cursors, selections, "typing…")
//! relayed to the other replicas of a document, never logged, never in the
//! way of the operation stream, and limited in size and rate per
//! connection.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use jade_collab_core::id::ReplicaId;
use jade_collab_server::wire::{ClientMessage, ServerMessage};
use jade_collab_server::{Client, Presence, PresenceLimits, Server, Tokens};
use jade_collab_sync::ReplicaEvent;
use jade_docx_collab::{DocxModel, DocxOp, StoryId};
use proptest::prelude::*;
use serde_json::{json, Value};

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}

async fn start(server: Server) -> (Server, String) {
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.clone().serve(listener));
    (server, format!("ws://{addr}/documents/hello.docx/ws"))
}

async fn until(client: &Client<DocxModel>, done: impl Fn(&BTreeMap<ReplicaId, Presence>) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), client.wait_for_presence(done))
        .await
        .unwrap_or_else(|_| panic!("presence never arrived: {:?}", client.presence()))
        .unwrap();
}

fn state_of(map: &BTreeMap<ReplicaId, Presence>, replica: ReplicaId) -> Option<&Value> {
    map.get(&replica).map(|p| &p.state)
}

#[test]
fn presence_messages_on_the_wire() {
    let message: ClientMessage =
        serde_json::from_str(r#"{"type":"presence","state":{"cursor":[0,4]}}"#).unwrap();
    assert_eq!(
        message,
        ClientMessage::Presence {
            state: json!({"cursor": [0, 4]})
        }
    );
    let relayed = ServerMessage::Presence {
        replica: ReplicaId(3),
        name: Some("Ada".into()),
        state: json!({"cursor": [0, 4]}),
    };
    assert_eq!(
        serde_json::to_value(&relayed).unwrap(),
        json!({"type": "presence", "replica": 3, "name": "Ada", "state": {"cursor": [0, 4]}})
    );
    let anonymous = ServerMessage::Presence {
        replica: ReplicaId(3),
        name: None,
        state: Value::Null,
    };
    assert_eq!(
        serde_json::to_value(&anonymous).unwrap(),
        json!({"type": "presence", "replica": 3, "state": null})
    );
    assert_eq!(
        serde_json::to_value(ServerMessage::PresenceLeft {
            replica: ReplicaId(3)
        })
        .unwrap(),
        json!({"type": "presence_left", "replica": 3})
    );
    assert_eq!(
        serde_json::to_value(ServerMessage::PresenceRejected {
            reason: "too big".into()
        })
        .unwrap(),
        json!({"type": "presence_rejected", "reason": "too big"})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_reaches_the_others_and_newcomers_but_not_the_sender() {
    let (server, url) = start(Server::new()).await;
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&url, None).await.unwrap();

    a.set_presence(json!({"cursor": 1}));
    a.set_presence(json!({"cursor": 2}));
    until(&b, |m| state_of(m, a.id()) == Some(&json!({"cursor": 2}))).await;
    assert_eq!(b.presence()[&a.id()].name, None, "no tokens, no names");
    // b answers; by the time a sees it, a's own presence would have come
    // back too if it were echoed.
    b.set_presence(json!("here"));
    until(&a, |m| state_of(m, b.id()) == Some(&json!("here"))).await;
    assert!(!a.presence().contains_key(&a.id()));

    // Someone who joins later is told what everyone is doing.
    let c: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    until(&c, |m| {
        state_of(m, a.id()) == Some(&json!({"cursor": 2}))
            && state_of(m, b.id()) == Some(&json!("here"))
    })
    .await;

    // Presence is not an operation.
    assert_eq!(server.seq("hello.docx").await.unwrap(), 0);
    assert!(a.take_events().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn leaving_clears_presence_everywhere() {
    let (_server, url) = start(Server::new()).await;
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    a.set_presence(json!({"cursor": 7}));
    until(&b, |m| m.contains_key(&a.id())).await;

    let id = a.id();
    let replica = a.disconnect().await;
    until(&b, |m| !m.contains_key(&id)).await;

    // Coming back starts with no presence until it is set again.
    let a = Client::resume(&url, replica, None).await.unwrap();
    a.set_presence(json!({"cursor": 8}));
    until(&b, |m| state_of(m, id) == Some(&json!({"cursor": 8}))).await;
    drop(a);
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_carries_the_token_name_and_viewers_may_share_it() {
    let tokens = Tokens::from_json(
        r#"{"e": {"name": "Ada", "role": "editor"}, "v": {"name": "Vic", "role": "viewer"}}"#,
    )
    .unwrap();
    let (_server, url) = start(Server::new().with_tokens(tokens)).await;
    let editor: Client<DocxModel> = Client::connect(&url, Some("e")).await.unwrap();
    let viewer: Client<DocxModel> = Client::connect(&url, Some("v")).await.unwrap();
    viewer.set_presence(json!({"looking_at": 0}));
    editor.set_presence(json!({"cursor": 0}));
    until(&editor, |m| m.contains_key(&viewer.id())).await;
    until(&viewer, |m| m.contains_key(&editor.id())).await;
    assert_eq!(editor.presence()[&viewer.id()].name.as_deref(), Some("Vic"));
    assert_eq!(viewer.presence()[&editor.id()].name.as_deref(), Some("Ada"));
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_is_never_stored() {
    let dir = tempfile::tempdir().unwrap();
    let (server, url) = start(Server::with_storage(dir.path()).unwrap()).await;
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    a.set_presence(json!({"secret_cursor": 42}));
    until(&b, |m| m.contains_key(&a.id())).await;
    assert_eq!(server.seq("hello.docx").await.unwrap(), 0);
    for entry in std::fs::read_dir(dir.path().join("hello.docx")).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("secret_cursor"), "{}", path.display());
    }
}

fn type_at_end(client: &Client<DocxModel>, text: &str) {
    let view = client.view();
    let paragraph = &view.paragraphs()[0];
    client
        .submit(DocxOp::InsertText {
            story: StoryId::Body,
            parent: paragraph.chars.last().copied(),
            first: client.reserve(text.chars().count() as u64),
            text: text.into(),
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_presence_flood_does_not_disturb_the_operation_stream() {
    let (server, url) = start(Server::new().with_presence_limits(PresenceLimits::UNLIMITED)).await;
    let flooder: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&url, None).await.unwrap();

    let flood = {
        let flooder = &flooder;
        async move {
            for i in 0..2000 {
                flooder.set_presence(json!({"cursor": i, "padding": "x".repeat(64)}));
                if i % 100 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }
    };
    let edits = async {
        for i in 0..40 {
            type_at_end(&a, &format!("a{i} "));
            type_at_end(&b, &format!("b{i} "));
            if i % 5 == 0 {
                tokio::task::yield_now().await;
            }
        }
    };
    tokio::join!(flood, edits);
    for client in [&a, &b] {
        tokio::time::timeout(Duration::from_secs(20), client.settle())
            .await
            .unwrap()
            .unwrap();
    }
    let seq = server.seq("hello.docx").await.unwrap();
    assert_eq!(seq, 80);
    for client in [&flooder, &a, &b] {
        tokio::time::timeout(Duration::from_secs(20), client.wait_for_seq(seq))
            .await
            .unwrap()
            .unwrap();
        // Every commit arrived once, in log order.
        let applied: Vec<u64> = client
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                ReplicaEvent::Applied { seq, .. } => Some(seq),
                ReplicaEvent::Acknowledged { .. } => None,
                ReplicaEvent::Rejected { .. } => panic!("nothing should be rejected"),
            })
            .collect();
        assert!(applied.windows(2).all(|w| w[0] < w[1]), "{applied:?}");
    }
    assert_eq!(a.view(), b.view());
    assert_eq!(a.view(), flooder.view());
    until(&a, |m| {
        state_of(m, flooder.id()).is_some_and(|s| s["cursor"] == json!(1999))
    })
    .await;
}

async fn settled(client: &Client<DocxModel>) {
    tokio::time::timeout(Duration::from_secs(20), client.settle())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn default_limits_allow_a_cursor_but_not_a_flood() {
    let limits = PresenceLimits::default();
    assert!(limits.max_bytes >= 1024, "{limits:?}");
    assert!(limits.burst >= 16, "{limits:?}");
    assert!(limits.per_second >= 10, "{limits:?}");
    assert!(
        limits.max_bytes < 1 << 20 && limits.per_second < 1000,
        "{limits:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_presence_is_dropped_and_its_sender_told() {
    let limits = PresenceLimits {
        max_bytes: 64,
        ..PresenceLimits::UNLIMITED
    };
    let (_server, url) = start(Server::new().with_presence_limits(limits)).await;
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&url, None).await.unwrap();

    // 64 bytes of JSON pass, 65 do not.
    let fits = json!("x".repeat(62));
    let over = json!("x".repeat(63));
    assert_eq!(fits.to_string().len(), 64);
    a.set_presence(over);
    // Anything the connection sends after it is handled after it.
    type_at_end(&a, "a");
    settled(&a).await;
    let rejected = a.take_presence_rejections();
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert!(rejected[0].contains("64"), "{rejected:?}");

    a.set_presence(fits.clone());
    until(&b, |m| state_of(m, a.id()) == Some(&fits)).await;
    type_at_end(&a, "b");
    settled(&a).await;
    assert!(a.take_presence_rejections().is_empty());
    // b never saw the oversized one: the only presence it got was `fits`.
    assert_eq!(b.presence().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_over_the_rate_is_dropped_but_operations_never_are() {
    // Three updates, and no more ever: the outcome does not depend on time.
    let limits = PresenceLimits {
        burst: 3,
        per_second: 0,
        ..PresenceLimits::UNLIMITED
    };
    let (server, url) = start(Server::new().with_presence_limits(limits)).await;
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    for i in 0..10 {
        a.set_presence(json!({"cursor": i}));
        type_at_end(&a, &format!("{i}"));
    }
    settled(&a).await;
    assert_eq!(server.seq("hello.docx").await.unwrap(), 10);
    let rejected = a.take_presence_rejections();
    assert_eq!(rejected.len(), 7, "{rejected:?}");
    assert!(
        a.take_events()
            .iter()
            .all(|e| !matches!(e, ReplicaEvent::Rejected { .. })),
        "no operation is rejected"
    );
    // b followed every operation, and the presence relayed in between: the
    // last one let through.
    tokio::time::timeout(Duration::from_secs(20), b.wait_for_seq(10))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state_of(&b.presence(), a.id()), Some(&json!({"cursor": 2})));
    assert_eq!(a.view(), b.view());

    // The limit is per connection: a new one starts afresh.
    let replica = a.disconnect().await;
    let a = Client::resume(&url, replica, None).await.unwrap();
    a.set_presence(json!({"cursor": "again"}));
    until(&b, |m| {
        state_of(m, a.id()) == Some(&json!({"cursor": "again"}))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_default_server_limits_a_presence_flood_and_keeps_every_operation() {
    let (server, url) = start(Server::new()).await;
    let flooder: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    for i in 0..500 {
        flooder.set_presence(json!({"cursor": i}));
        if i % 25 == 0 {
            type_at_end(&flooder, "f");
            type_at_end(&a, "a");
        }
    }
    settled(&flooder).await;
    settled(&a).await;
    assert_eq!(server.seq("hello.docx").await.unwrap(), 40);
    let rejected = flooder.take_presence_rejections();
    assert!(rejected.len() > 300, "{} rejected", rejected.len());
    // A huge state is refused whatever the rate.
    flooder.set_presence(json!("x".repeat(PresenceLimits::default().max_bytes)));
    type_at_end(&flooder, "f");
    settled(&flooder).await;
    let rejected = flooder.take_presence_rejections();
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert!(rejected[0].contains("bytes"), "{rejected:?}");
    tokio::time::timeout(Duration::from_secs(20), a.wait_for_seq(41))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.view(), flooder.view());
}

#[derive(Clone, Debug)]
enum Step {
    Set { client: usize, value: u8 },
    Reconnect { client: usize },
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            4 => (0..3usize, any::<u8>()).prop_map(|(client, value)| Step::Set { client, value }),
            1 => (0..3usize).prop_map(|client| Step::Reconnect { client }),
        ],
        0..16,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// Whatever sequence of updates and reconnects, every replica ends up
    /// seeing exactly the latest presence of every other replica that set
    /// one since it last connected, and never its own.
    #[test]
    fn everyone_ends_up_seeing_everyone_elses_latest_presence(steps in steps()) {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let (_server, url) = start(Server::new()).await;
            let mut clients = Vec::new();
            for _ in 0..3 {
                clients.push(Some(Client::<DocxModel>::connect(&url, None).await.unwrap()));
            }
            let ids: Vec<ReplicaId> = clients.iter().map(|c| c.as_ref().unwrap().id()).collect();
            let mut expected: BTreeMap<ReplicaId, Value> = BTreeMap::new();
            for step in &steps {
                match *step {
                    Step::Set { client, value } => {
                        let state = json!({"value": value});
                        clients[client].as_ref().unwrap().set_presence(state.clone());
                        expected.insert(ids[client], state);
                    }
                    Step::Reconnect { client } => {
                        let replica = clients[client].take().unwrap().disconnect().await;
                        clients[client] = Some(Client::resume(&url, replica, None).await.unwrap());
                        expected.remove(&ids[client]);
                    }
                }
            }
            for (index, client) in clients.iter().enumerate() {
                let client = client.as_ref().unwrap();
                let mut others = expected.clone();
                others.remove(&ids[index]);
                let done = tokio::time::timeout(
                    Duration::from_secs(10),
                    client.wait_for_presence(|m| {
                        m.iter().map(|(r, p)| (*r, p.state.clone())).collect::<BTreeMap<_, _>>() == others
                    }),
                )
                .await;
                assert!(
                    matches!(done, Ok(Ok(()))),
                    "client {index}: {:?}, expected {others:?}",
                    client.presence()
                );
            }
        });
    }
}

mod raw {
    //! A bare socket speaking the wire protocol, to hold a replica's old
    //! connection open while it resumes on a new one.
    use futures_util::{SinkExt, StreamExt};
    use jade_collab_server::wire::{ClientMessage, ServerMessage};
    use tokio_tungstenite::tungstenite::Message;

    pub struct Raw(
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    );

    impl Raw {
        pub async fn open(url: &str) -> Self {
            Raw(tokio_tungstenite::connect_async(url).await.unwrap().0)
        }

        pub async fn send(&mut self, message: &ClientMessage) {
            let text = serde_json::to_string(message).unwrap();
            self.0.send(Message::Text(text.into())).await.unwrap();
        }

        pub async fn receive(&mut self) -> ServerMessage {
            loop {
                if let Message::Text(text) = self.0.next().await.unwrap().unwrap() {
                    return serde_json::from_str(&text).unwrap();
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resuming_on_a_new_connection_drops_the_old_connections_presence() {
    use jade_collab_sync::protocol::Submission;
    use jade_collab_sync::Document;

    let (_server, url) = start(Server::new()).await;
    let observer: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let mut old = raw::Raw::open(&url).await;
    old.send(&ClientMessage::Join).await;
    let ServerMessage::Welcome {
        replica, snapshot, ..
    } = old.receive().await
    else {
        panic!("expected a welcome");
    };
    old.send(&ClientMessage::Presence {
        state: json!("old"),
    })
    .await;
    until(&observer, |m| state_of(m, replica) == Some(&json!("old"))).await;

    // The replica comes back on a new connection while the old one is
    // still open: what the old one showed is gone.
    let mut new = raw::Raw::open(&url).await;
    new.send(&ClientMessage::Resume { replica, seq: 0 }).await;
    assert!(matches!(new.receive().await, ServerMessage::Resumed { .. }));
    until(&observer, |m| !m.contains_key(&replica)).await;

    // A late update from the old connection is ignored. It is followed, on
    // the same connection, by an operation; once that is in, the update
    // has been handled.
    old.send(&ClientMessage::Presence {
        state: json!("stale"),
    })
    .await;
    let model: DocxModel = serde_json::from_value(snapshot).unwrap();
    let op = DocxOp::InsertText {
        story: StoryId::Body,
        parent: None,
        first: jade_collab_core::id::Id::new(model.max_lamport() + 1, replica),
        text: "x".into(),
    };
    old.send(&ClientMessage::Submit(Submission {
        client_seq: 1,
        base_seq: 0,
        op: serde_json::to_value(op).unwrap(),
    }))
    .await;
    tokio::time::timeout(Duration::from_secs(10), observer.wait_for_seq(1))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !observer.presence().contains_key(&replica),
        "{:?}",
        observer.presence()
    );
}
