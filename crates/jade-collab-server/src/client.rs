use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use jade_collab_core::id::{Id, IdError, ReplicaId};
use jade_collab_sync::protocol::{Committed, Downstream, Resume, Submission};
use jade_collab_sync::{Document, Rejection, Replica, ReplicaEvent, SyncError};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Notify};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};
use tokio_tungstenite::{client_async_with_config, MaybeTlsStream, WebSocketStream};

use crate::wire::{ClientMessage, ServerMessage};
mod pump;
#[cfg(test)]
mod tests;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

// Welcomes and inspections are sent as single frames. Keep the existing
// 64 MiB aggregate message budget, but do not reject a valid snapshot merely
// because tungstenite's default per-frame budget is only 16 MiB.
const MAX_SERVER_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// One deadline covering TCP/HTTP upgrade, the initial sync reply and resends.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect: {0}")]
    Connect(String),
    #[error("the connection did not become ready within {0:?}")]
    Timeout(Duration),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("the server cannot resume this replica: {0}")]
    Rejoin(String),
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error("the connection closed")]
    Closed,
    /// The server withdrew the token, or its access to the document, and
    /// closed the connection.
    #[error("{0}")]
    Revoked(String),
    /// The client read too slowly and the server dropped the connection;
    /// [`Client::resume`] catches up.
    #[error("{0}")]
    Overloaded(String),
    #[error("cannot inspect the document: {0}")]
    Inspection(String),
}

/// What another replica shares about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Presence {
    /// Its token's name, on a server with tokens.
    pub name: Option<String>,
    pub state: serde_json::Value,
}

struct Shared<D: Document> {
    identity: Arc<()>,
    replica: Mutex<Replica<D>>,
    events: Mutex<Vec<ReplicaEvent<D::Op>>>,
    presence: Mutex<BTreeMap<ReplicaId, Presence>>,
    presence_rejections: Mutex<Vec<String>>,
    error: Mutex<Option<ClientError>>,
    inspections: Mutex<InspectionRequests<D>>,
    changed: Notify,
    outgoing: Notify,
    latest_presence: Mutex<Option<String>>,
}

struct InspectionRequests<D: Document> {
    next: u64,
    waiting: BTreeMap<u64, PendingInspection<D>>,
}
impl<D: Document> Default for InspectionRequests<D> {
    fn default() -> Self {
        Self {
            next: 0,
            waiting: BTreeMap::new(),
        }
    }
}
struct PendingInspection<D: Document> {
    next_client_seq: u64,
    sent: bool,
    answer: oneshot::Sender<Result<Inspection<D>, ClientError>>,
}
struct InspectionGuard<D: Document> {
    shared: Arc<Shared<D>>,
    request: u64,
}
impl<D: Document> Drop for InspectionGuard<D> {
    fn drop(&mut self) {
        self.shared
            .inspections
            .lock()
            .unwrap()
            .waiting
            .remove(&self.request);
    }
}

/// The server's committed model and native formatting at one log position.
/// This excludes local operations submitted after the inspection request.
/// Its snapshot may be used as an edit precondition for this connection.
#[derive(Clone, Debug)]
pub struct Inspection<D> {
    pub snapshot: ClientSnapshot<D>,
    pub formatting: serde_json::Value,
}

/// A replica of one document, connected to a server. Local operations apply
/// at once; the connection keeps the replica in step with the server's log
/// in the background.
/// Dropping the client closes the connection. Settle first to await verdicts,
/// or use [`Client::disconnect`] to retain pending operations for resumption.
pub struct Client<D: Document> {
    shared: Arc<Shared<D>>,
    tasks: ConnectionTasks,
}

struct ConnectionTasks(Vec<JoinHandle<()>>);
impl Drop for ConnectionTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// A view and its replication metadata captured under the same lock.
/// The view includes exactly this snapshot's pending optimistic operations.
#[derive(Clone, Debug)]
pub struct ClientSnapshot<D> {
    pub view: D,
    pub seq: u64,
    pub pending: usize,
    identity: Arc<()>,
    next_client_seq: u64,
}

impl<D: PartialEq> PartialEq for ClientSnapshot<D> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
            && self.seq == other.seq
            && self.pending == other.pending
            && self.next_client_seq == other.next_client_seq
            && self.view == other.view
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the replica changed since this snapshot; read again before editing")]
pub struct StaleSnapshot;

/// A failed local edit plan. No model, allocation or submission from that
/// plan is retained. Server verdicts are reported later through replica events.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditError<E> {
    #[error(transparent)]
    Stale(#[from] StaleSnapshot),
    #[error("edit planning failed: {0}")]
    Refused(E),
}

/// A synchronous editing context. Resolve, reserve and submit through this
/// context; calling the parent client's locking methods here would deadlock.
pub struct ClientEdit<'a, D: Document> {
    replica: &'a mut Replica<D>,
    outgoing: Option<&'a Notify>,
}

impl<D> ClientEdit<'_, D>
where
    D: Document + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    pub fn view(&self) -> &D {
        self.replica.view()
    }
    pub fn id(&self) -> ReplicaId {
        self.replica.id()
    }
    pub fn try_reserve(&mut self, n: u64) -> Result<Id, IdError> {
        self.replica.try_reserve(n)
    }
    pub fn submit(&mut self, op: D::Op) -> Result<u64, Rejection> {
        let client_seq = self.replica.submit(op)?.client_seq;
        if let Some(outgoing) = self.outgoing {
            outgoing.notify_one();
        }
        Ok(client_seq)
    }
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
    if !matches!(request.uri().scheme_str(), Some("ws" | "wss")) {
        return Err(ClientError::Connect(
            "expected a ws:// or wss:// document socket".into(),
        ));
    }
    let address =
        crate::transport::TransportAddress::parse(request.uri()).map_err(ClientError::Connect)?;
    let stream = address.connect().await.map_err(ClientError::Connect)?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_SERVER_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_SERVER_MESSAGE_BYTES));
    let (socket, _) = client_async_with_config(request, stream, Some(config))
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
        Self::connect_with_timeout(url, token, DEFAULT_CONNECT_TIMEOUT).await
    }

    /// Joins with one overall network deadline. Expiration/cancellation closes
    /// the opening socket; no background connection tasks survive it.
    pub async fn connect_with_timeout(
        url: &str,
        token: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        tokio::time::timeout(timeout, Self::connect_inner(url, token))
            .await
            .map_err(|_| ClientError::Timeout(timeout))?
    }

    async fn connect_inner(url: &str, token: Option<&str>) -> Result<Self, ClientError> {
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
            ServerMessage::Revoked { message } => return Err(ClientError::Revoked(message)),
            ServerMessage::Overloaded { message } => return Err(ClientError::Overloaded(message)),
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
    /// [`Client::connect`] then joins as a new replica. The source replica is
    /// borrowed: errors and cancellation preserve its view, pending operations,
    /// identifiers and tickets for retry. Discard it after successful resumption;
    /// rejoining under a new replica id does not replay its pending edits.
    ///
    /// The source cannot be edited while its opening future is live:
    /// ```compile_fail,E0502
    /// use jade_collab_server::Client;
    /// use jade_collab_sync::Replica;
    /// use jade_docx_collab::DocxModel;
    /// fn edit_during_resume(mut source: Replica<DocxModel>) {
    ///     let opening = Client::resume("ws://localhost/documents/a.docx/ws", &source, None);
    ///     source.reserve(1);
    ///     drop(opening);
    /// }
    /// ```
    pub fn resume<'a>(
        url: &'a str,
        replica: &'a Replica<D>,
        token: Option<&'a str>,
    ) -> impl Future<Output = Result<Self, ClientError>> + Send + 'a {
        Self::resume_with_timeout(url, replica, token, DEFAULT_CONNECT_TIMEOUT)
    }

    /// Resumes with an overall deadline, retaining the caller's source replica.
    /// The returned future owns its staging copy and is Send even for a model
    /// that is Send without being Sync.
    pub fn resume_with_timeout<'a>(
        url: &'a str,
        replica: &'a Replica<D>,
        token: Option<&'a str>,
        timeout: Duration,
    ) -> impl Future<Output = Result<Self, ClientError>> + Send + 'a {
        let replica = replica.clone();
        async move {
            tokio::time::timeout(timeout, Self::resume_inner(url, replica, token))
                .await
                .map_err(|_| ClientError::Timeout(timeout))?
        }
    }

    async fn resume_inner(
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
            ServerMessage::Revoked { message } => return Err(ClientError::Revoked(message)),
            ServerMessage::Overloaded { message } => return Err(ClientError::Overloaded(message)),
            other => {
                return Err(ClientError::Protocol(format!(
                    "expected resumed, got {other:?}"
                )))
            }
        };
        if let Some(first) = replica.first_pending_client_seq() {
            for ticket in first..replica.next_client_seq() {
                let submission = replica
                    .pending_submission(ticket)
                    .expect("pending FIFO ticket");
                sink.send(Self::submit_message(submission)?)
                    .await
                    .map_err(|e| ClientError::Connect(e.to_string()))?;
            }
        }
        Ok(Self::start(replica, events, sink, stream))
    }

    fn submit_message(submission: Submission<D::Op>) -> Result<Message, ClientError> {
        let message = ClientMessage::Submit(Submission {
            client_seq: submission.client_seq,
            base_seq: submission.base_seq,
            op: serde_json::to_value(&submission.op)
                .map_err(|e| ClientError::Protocol(format!("cannot encode operation: {e}")))?,
        });
        Ok(text(&message))
    }

    fn start(
        replica: Replica<D>,
        events: Vec<ReplicaEvent<D::Op>>,
        sink: SplitSink<Socket, Message>,
        mut stream: SplitStream<Socket>,
    ) -> Self {
        let cursor = pump::Cursor::new(replica.next_client_seq());
        let shared = Arc::new(Shared {
            identity: Arc::new(()),
            replica: Mutex::new(replica),
            events: Mutex::new(events),
            presence: Mutex::new(BTreeMap::new()),
            presence_rejections: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            inspections: Mutex::new(InspectionRequests::default()),
            changed: Notify::new(),
            outgoing: Notify::new(),
            latest_presence: Mutex::new(None),
        });
        let writer_shared = shared.clone();
        let writer = tokio::spawn(pump::write(writer_shared, cursor, sink));
        let reader_shared = shared.clone();
        let reader = tokio::spawn(async move {
            let shared = reader_shared;
            loop {
                let message = tokio::select! {
                    biased;
                    _ = shared.failed() => break,
                    message = next_message(&mut stream) => message,
                };
                let result = match message {
                    Ok(message) => Self::handle(&shared, message),
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    shared.fail(error);
                    break;
                }
                shared.changed.notify_waiters();
            }
        });
        Self {
            shared,
            tasks: ConnectionTasks(vec![writer, reader]),
        }
    }

    fn handle(shared: &Shared<D>, message: ServerMessage) -> Result<(), ClientError> {
        let downstream = match message {
            ServerMessage::Inspected {
                request,
                seq,
                snapshot,
                formatting,
            } => {
                let pending = shared.inspections.lock().unwrap().waiting.remove(&request);
                if let Some(pending) = pending {
                    let answer = decode(snapshot).map(|view| Inspection {
                        snapshot: ClientSnapshot {
                            view,
                            seq,
                            pending: 0,
                            identity: shared.identity.clone(),
                            next_client_seq: pending.next_client_seq,
                        },
                        formatting,
                    });
                    let _ = pending.answer.send(answer);
                }
                return Ok(());
            }
            ServerMessage::InspectionFailed { request, message } => {
                if let Some(pending) = shared.inspections.lock().unwrap().waiting.remove(&request) {
                    let _ = pending.answer.send(Err(ClientError::Inspection(message)));
                }
                return Ok(());
            }
            ServerMessage::Committed(committed) => {
                Downstream::Committed(decode_committed(committed)?)
            }
            ServerMessage::Rejected(rejected) => Downstream::Rejected(rejected),
            ServerMessage::Presence {
                replica,
                name,
                state,
            } => {
                let presence = Presence { name, state };
                shared.presence.lock().unwrap().insert(replica, presence);
                return Ok(());
            }
            ServerMessage::PresenceLeft { replica } => {
                shared.presence.lock().unwrap().remove(&replica);
                return Ok(());
            }
            ServerMessage::PresenceRejected { reason } => {
                shared.presence_rejections.lock().unwrap().push(reason);
                return Ok(());
            }
            ServerMessage::Error { message } => return Err(ClientError::Protocol(message)),
            ServerMessage::Revoked { message } => return Err(ClientError::Revoked(message)),
            ServerMessage::Overloaded { message } => return Err(ClientError::Overloaded(message)),
            other => return Err(ClientError::Protocol(format!("unexpected {other:?}"))),
        };
        // Publish the verdict before zero pending becomes observable, so
        // settle() followed by take_events() sees the acknowledgement.
        let mut replica = shared.replica.lock().unwrap();
        let event = replica.receive(downstream)?;
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

    /// Captures a consistent version even while socket messages arrive.
    pub fn snapshot(&self) -> ClientSnapshot<D> {
        let replica = self.shared.replica.lock().unwrap();
        ClientSnapshot {
            view: replica.view().clone(),
            seq: replica.seq(),
            pending: replica.pending_len(),
            identity: self.shared.identity.clone(),
            next_client_seq: replica.next_client_seq(),
        }
    }

    /// Requests the authoritative model and native formatting through this
    /// authenticated connection. Earlier queued edits are processed first.
    /// Supported for DOCX and PPTX shape text. Cancellation removes the pending request;
    /// an inspection failure leaves editing and future requests available.
    pub async fn inspect(&self) -> Result<Inspection<D>, ClientError> {
        let (answer, guard) = {
            // Same lock order as edits: request insertion is ordered with
            // submissions, and the captured ticket detects later local edits.
            let replica = self.shared.replica.lock().unwrap();
            let error = self.shared.error.lock().unwrap();
            // Keep the error guard until registration, so a terminal error
            // cannot drain waiters immediately before this one is installed.
            if let Some(error) = error.as_ref() {
                return Err(error.clone());
            }
            let mut inspections = self.shared.inspections.lock().unwrap();
            let request = inspections.next.checked_add(1).ok_or_else(|| {
                ClientError::Inspection("request identifier space exhausted".into())
            })?;
            let (reply, answer) = oneshot::channel();
            inspections.next = request;
            inspections.waiting.insert(
                request,
                PendingInspection {
                    next_client_seq: replica.next_client_seq(),
                    sent: false,
                    answer: reply,
                },
            );
            self.shared.outgoing.notify_one();
            (
                answer,
                InspectionGuard {
                    shared: self.shared.clone(),
                    request,
                },
            )
        };
        let result = answer.await.map_err(|_| ClientError::Closed)?;
        drop(guard);
        result
    }

    /// Validates an optional captured version and performs the synchronous
    /// edit without interleaving incoming operations or other local edits.
    /// A stale/foreign snapshot refuses the whole callback before allocation.
    pub fn edit<R>(
        &self,
        expected: Option<&ClientSnapshot<D>>,
        action: impl FnOnce(&mut ClientEdit<'_, D>) -> R,
    ) -> Result<R, StaleSnapshot> {
        let mut replica = self.shared.replica.lock().unwrap();
        self.validate_snapshot(&replica, expected)?;
        Ok(action(&mut ClientEdit {
            replica: &mut replica,
            outgoing: Some(&self.shared.outgoing),
        }))
    }

    fn validate_snapshot(
        &self,
        replica: &Replica<D>,
        expected: Option<&ClientSnapshot<D>>,
    ) -> Result<(), StaleSnapshot> {
        if expected.is_some_and(|s| {
            !Arc::ptr_eq(&s.identity, &self.shared.identity)
                || s.seq != replica.seq()
                || s.pending != replica.pending_len()
                || s.next_client_seq != replica.next_client_seq()
        }) {
            return Err(StaleSnapshot);
        }
        Ok(())
    }

    /// Plans a fallible edit on a private copy, then commits its local state
    /// and queues its submissions only on success. Validation, planning and
    /// queueing hold one replica lock. An error discards all changes made
    /// through the context, including reserved IDs and client tickets.
    ///
    /// This cannot undo external effects performed by the callback. It is a
    /// local planning boundary: the server still judges each operation, and
    /// may accept a prefix or reject operations after concurrent edits.
    /// Like [`Client::edit`], the callback must use the context's methods.
    pub fn try_edit<R, E>(
        &self,
        expected: Option<&ClientSnapshot<D>>,
        action: impl FnOnce(&mut ClientEdit<'_, D>) -> Result<R, E>,
    ) -> Result<R, EditError<E>> {
        let mut replica = self.shared.replica.lock().unwrap();
        self.validate_snapshot(&replica, expected)?;
        let mut planned = replica.clone();
        let result = action(&mut ClientEdit {
            replica: &mut planned,
            outgoing: None,
        })
        .map_err(EditError::Refused)?;
        *replica = planned;
        // A closed connection retains pending operations for resume.
        self.shared.outgoing.notify_one();
        Ok(result)
    }

    pub fn pending_len(&self) -> usize {
        self.shared.replica.lock().unwrap().pending_len()
    }

    /// Reserves `n` consecutive fresh identifiers and returns the first.
    /// Panics if empty or exhausted; prefer [`Client::try_reserve`] in applications.
    pub fn reserve(&self, n: u64) -> Id {
        self.shared.replica.lock().unwrap().reserve(n)
    }

    /// Refuses an exhausted reservation without panicking or poisoning the replica.
    pub fn try_reserve(&self, n: u64) -> Result<Id, IdError> {
        self.shared.replica.lock().unwrap().try_reserve(n)
    }

    /// Applies `op` locally and sends it; returns its client sequence number.
    /// An operation that does not apply to the view is refused here.
    pub fn submit(&self, op: D::Op) -> Result<u64, Rejection> {
        let mut replica = self.shared.replica.lock().unwrap();
        let ticket = replica.submit(op)?.client_seq;
        self.shared.outgoing.notify_one();
        Ok(ticket)
    }

    /// Acknowledgements, rejections and remote operations received so far.
    pub fn take_events(&self) -> Vec<ReplicaEvent<D::Op>> {
        std::mem::take(&mut *self.shared.events.lock().unwrap())
    }

    /// The terminal connection failure, if one has been observed. An individual
    /// operation rejection or failed inspection does not fail the connection.
    pub fn connection_error(&self) -> Option<ClientError> {
        self.shared.error.lock().unwrap().clone()
    }

    async fn wait_until(&self, done: impl Fn(&Shared<D>) -> bool) -> Result<(), ClientError> {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if done(&self.shared) {
                return Ok(());
            }
            if let Some(error) = self.shared.error.lock().unwrap().clone() {
                return Err(error);
            }
            notified.await;
        }
    }

    /// Shares `state` with the other replicas, replacing what this one
    /// shared before. Unsent updates coalesce to the latest value. Ephemeral:
    /// never logged, gone when the connection is.
    pub fn set_presence(&self, state: serde_json::Value) {
        let message = serde_json::to_string(&ClientMessage::Presence { state })
            .expect("client messages serialize");
        *self.shared.latest_presence.lock().unwrap() = Some(message);
        self.shared.outgoing.notify_one();
    }

    /// Why the server dropped presence updates of this connection (over its
    /// size or rate limit) since the last call.
    pub fn take_presence_rejections(&self) -> Vec<String> {
        std::mem::take(&mut *self.shared.presence_rejections.lock().unwrap())
    }

    /// What the other replicas currently share.
    pub fn presence(&self) -> BTreeMap<ReplicaId, Presence> {
        self.shared.presence.lock().unwrap().clone()
    }

    /// Waits until the others' presence satisfies `done`.
    pub async fn wait_for_presence(
        &self,
        done: impl Fn(&BTreeMap<ReplicaId, Presence>) -> bool,
    ) -> Result<(), ClientError> {
        self.wait_until(|shared| done(&shared.presence.lock().unwrap()))
            .await
    }

    /// Waits until the server has answered every local operation.
    pub async fn settle(&self) -> Result<(), ClientError> {
        self.wait_until(|shared| shared.replica.lock().unwrap().pending_len() == 0)
            .await
    }

    /// Waits for local submissions through `client_seq`, a ticket returned by
    /// submit/edit (or zero for no operations). Later submissions do not delay
    /// this wait. Verdict events are available before it returns successfully.
    pub async fn settle_through(&self, client_seq: u64) -> Result<(), ClientError> {
        if client_seq >= self.shared.replica.lock().unwrap().next_client_seq() {
            return Err(ClientError::Protocol(
                "the requested submission ticket was not issued by this connection".into(),
            ));
        }
        self.wait_until(|shared| !shared.replica.lock().unwrap().pending_through(client_seq))
            .await
    }

    /// Waits until the replica has received the log up to `seq`.
    pub async fn wait_for_seq(&self, seq: u64) -> Result<(), ClientError> {
        self.wait_until(|shared| shared.replica.lock().unwrap().seq() >= seq)
            .await
    }

    /// Closes the connection and hands back the replica, pending operations
    /// included, for [`Client::resume`].
    pub async fn disconnect(mut self) -> Replica<D> {
        for task in self.tasks.0.drain(..) {
            task.abort();
            let _ = task.await;
        }
        let shared = Arc::try_unwrap(self.shared)
            .ok()
            .expect("the connection tasks have stopped");
        shared.replica.into_inner().unwrap()
    }
}
