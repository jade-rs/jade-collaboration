use std::sync::{Arc, Mutex};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::protocol::{Committed, Downstream, Resume, Submission};
use jade_collab_sync::{Document, Rejection, Replica, ReplicaEvent, SyncError};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::wire::{ClientMessage, ServerMessage};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect: {0}")]
    Connect(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("the server cannot resume this replica: {0}")]
    Rejoin(String),
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error("the connection closed")]
    Closed,
}

struct Shared<D: Document> {
    replica: Mutex<Replica<D>>,
    events: Mutex<Vec<ReplicaEvent<D::Op>>>,
    error: Mutex<Option<String>>,
    changed: Notify,
}

/// A replica of one document, connected to a server. Local operations apply
/// at once; the connection keeps the replica in step with the server's log
/// in the background.
pub struct Client<D: Document> {
    shared: Arc<Shared<D>>,
    outgoing: UnboundedSender<String>,
    tasks: Vec<JoinHandle<()>>,
}

fn text(message: &ClientMessage) -> Message {
    Message::Text(
        serde_json::to_string(message)
            .expect("client messages serialize")
            .into(),
    )
}

/// Opens the socket, presenting `token` as a bearer token if there is one.
async fn open(url: &str, token: Option<&str>) -> Result<Socket, ClientError> {
    let mut request = url
        .into_client_request()
        .map_err(|e| ClientError::Connect(e.to_string()))?;
    if let Some(token) = token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| ClientError::Connect(format!("unusable token: {e}")))?;
        request.headers_mut().insert(header::AUTHORIZATION, value);
    }
    let (socket, _) = connect_async(request)
        .await
        .map_err(|e| ClientError::Connect(e.to_string()))?;
    Ok(socket)
}

async fn next_message(stream: &mut SplitStream<Socket>) -> Result<ServerMessage, ClientError> {
    while let Some(message) = stream.next().await {
        match message.map_err(|e| ClientError::Protocol(e.to_string()))? {
            Message::Text(text) => {
                return serde_json::from_str(&text)
                    .map_err(|e| ClientError::Protocol(e.to_string()))
            }
            Message::Close(_) => return Err(ClientError::Closed),
            _ => continue,
        }
    }
    Err(ClientError::Closed)
}

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, ClientError> {
    serde_json::from_value(value).map_err(|e| ClientError::Protocol(e.to_string()))
}

fn decode_committed<Op: DeserializeOwned>(
    c: Committed<serde_json::Value>,
) -> Result<Committed<Op>, ClientError> {
    Ok(Committed {
        seq: c.seq,
        origin: c.origin,
        client_seq: c.client_seq,
        base_seq: c.base_seq,
        op: decode(c.op)?,
    })
}

impl<D> Client<D>
where
    D: Document + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    /// Joins the document at `url` (`ws://host/documents/{name}/ws`) as a
    /// new replica. `token` is sent as a bearer token; a server with tokens
    /// also takes one in the URL (`…/ws?token=…`).
    pub async fn connect(url: &str, token: Option<&str>) -> Result<Self, ClientError> {
        let socket = open(url, token).await?;
        let (mut sink, mut stream) = socket.split();
        sink.send(text(&ClientMessage::Join))
            .await
            .map_err(|e| ClientError::Connect(e.to_string()))?;
        let replica = match next_message(&mut stream).await? {
            ServerMessage::Welcome {
                replica,
                seq,
                snapshot,
                ..
            } => Replica::new(replica, seq, decode(snapshot)?),
            ServerMessage::Error { message } => return Err(ClientError::Protocol(message)),
            other => {
                return Err(ClientError::Protocol(format!(
                    "expected a welcome, got {other:?}"
                )))
            }
        };
        Ok(Self::start(replica, Vec::new(), sink, stream))
    }

    /// Reconnects a replica taken from [`Client::disconnect`]: catches up
    /// on what it missed and resends what the server never answered. Fails
    /// with [`ClientError::Rejoin`] when the server can no longer resume it;
    /// [`Client::connect`] then joins as a new replica.
    pub async fn resume(
        url: &str,
        mut replica: Replica<D>,
        token: Option<&str>,
    ) -> Result<Self, ClientError> {
        let socket = open(url, token).await?;
        let (mut sink, mut stream) = socket.split();
        sink.send(text(&ClientMessage::Resume {
            replica: replica.id(),
            seq: replica.seq(),
        }))
        .await
        .map_err(|e| ClientError::Connect(e.to_string()))?;
        let events = match next_message(&mut stream).await? {
            ServerMessage::Resumed {
                processed_client_seq,
                missed,
            } => {
                let missed = missed
                    .into_iter()
                    .map(decode_committed)
                    .collect::<Result<Vec<_>, _>>()?;
                replica.resume(Resume {
                    processed_client_seq,
                    missed,
                })?
            }
            ServerMessage::Rejoin { message } => return Err(ClientError::Rejoin(message)),
            ServerMessage::Error { message } => return Err(ClientError::Protocol(message)),
            other => {
                return Err(ClientError::Protocol(format!(
                    "expected resumed, got {other:?}"
                )))
            }
        };
        for submission in replica.pending_submissions() {
            sink.send(Self::submit_message(submission))
                .await
                .map_err(|e| ClientError::Connect(e.to_string()))?;
        }
        Ok(Self::start(replica, events, sink, stream))
    }

    fn submit_message(submission: Submission<D::Op>) -> Message {
        text(&ClientMessage::Submit(Submission {
            client_seq: submission.client_seq,
            base_seq: submission.base_seq,
            op: serde_json::to_value(&submission.op).expect("operations serialize"),
        }))
    }

    fn start(
        replica: Replica<D>,
        events: Vec<ReplicaEvent<D::Op>>,
        mut sink: SplitSink<Socket, Message>,
        mut stream: SplitStream<Socket>,
    ) -> Self {
        let shared = Arc::new(Shared {
            replica: Mutex::new(replica),
            events: Mutex::new(events),
            error: Mutex::new(None),
            changed: Notify::new(),
        });
        let (outgoing, mut queue) = unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(message) = queue.recv().await {
                if sink.send(Message::Text(message.into())).await.is_err() {
                    break;
                }
            }
        });
        let reader_shared = shared.clone();
        let reader = tokio::spawn(async move {
            let shared = reader_shared;
            loop {
                let result = match next_message(&mut stream).await {
                    Ok(message) => Self::handle(&shared, message),
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    *shared.error.lock().unwrap() = Some(error.to_string());
                    shared.changed.notify_waiters();
                    break;
                }
                shared.changed.notify_waiters();
            }
        });
        Self {
            shared,
            outgoing,
            tasks: vec![writer, reader],
        }
    }

    fn handle(shared: &Shared<D>, message: ServerMessage) -> Result<(), ClientError> {
        let downstream = match message {
            ServerMessage::Committed(committed) => {
                Downstream::Committed(decode_committed(committed)?)
            }
            ServerMessage::Rejected(rejected) => Downstream::Rejected(rejected),
            ServerMessage::Error { message } => return Err(ClientError::Protocol(message)),
            other => return Err(ClientError::Protocol(format!("unexpected {other:?}"))),
        };
        let event = shared.replica.lock().unwrap().receive(downstream)?;
        shared.events.lock().unwrap().push(event);
        Ok(())
    }

    pub fn id(&self) -> ReplicaId {
        self.shared.replica.lock().unwrap().id()
    }

    pub fn seq(&self) -> u64 {
        self.shared.replica.lock().unwrap().seq()
    }

    /// The optimistic view: everything received plus local pending edits.
    pub fn view(&self) -> D {
        self.shared.replica.lock().unwrap().view().clone()
    }

    pub fn pending_len(&self) -> usize {
        self.shared.replica.lock().unwrap().pending_len()
    }

    /// Reserves `n` consecutive fresh identifiers and returns the first.
    pub fn reserve(&self, n: u64) -> Id {
        self.shared.replica.lock().unwrap().reserve(n)
    }

    /// Applies `op` locally and sends it; returns its client sequence number.
    /// An operation that does not apply to the view is refused here.
    pub fn submit(&self, op: D::Op) -> Result<u64, Rejection> {
        let submission = self.shared.replica.lock().unwrap().submit(op)?;
        let client_seq = submission.client_seq;
        let Message::Text(message) = Self::submit_message(submission) else {
            unreachable!()
        };
        // If the connection is gone the submission stays pending and is
        // resent by `resume`.
        let _ = self.outgoing.send(message.to_string());
        Ok(client_seq)
    }

    /// Acknowledgements, rejections and remote operations received so far.
    pub fn take_events(&self) -> Vec<ReplicaEvent<D::Op>> {
        std::mem::take(&mut *self.shared.events.lock().unwrap())
    }

    async fn wait_until(&self, done: impl Fn(&Replica<D>) -> bool) -> Result<(), ClientError> {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if done(&self.shared.replica.lock().unwrap()) {
                return Ok(());
            }
            if let Some(error) = self.shared.error.lock().unwrap().clone() {
                return Err(ClientError::Protocol(error));
            }
            notified.await;
        }
    }

    /// Waits until the server has answered every local operation.
    pub async fn settle(&self) -> Result<(), ClientError> {
        self.wait_until(|replica| replica.pending_len() == 0).await
    }

    /// Waits until the replica has received the log up to `seq`.
    pub async fn wait_for_seq(&self, seq: u64) -> Result<(), ClientError> {
        self.wait_until(|replica| replica.seq() >= seq).await
    }

    /// Closes the connection and hands back the replica, pending operations
    /// included, for [`Client::resume`].
    pub async fn disconnect(mut self) -> Replica<D> {
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
        let shared = Arc::try_unwrap(self.shared)
            .ok()
            .expect("the connection tasks have stopped");
        shared.replica.into_inner().unwrap()
    }
}
