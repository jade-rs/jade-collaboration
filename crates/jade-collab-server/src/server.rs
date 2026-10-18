use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::audit::Audit;
use crate::auth::{
    access, authenticate, forbidden, recheck, Grant, Presented, Role, Tokens, TokensError,
};
use crate::limits::{Bucket, PresenceLimits};
use crate::outbox::{outbox, DEFAULT_OUTGOING_LIMIT};
use crate::room::{self, Command, Source};
use crate::storage::{newest_generation, StorageOptions};
use crate::wire::{ClientMessage, Format, ServerMessage};
use jade_collab_sync::protocol::Rejected;
use jade_collab_sync::{Rejection, SyncError};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("no document named {0}")]
    NotFound(String),
    #[error("a document named {0} is already open")]
    Exists(String),
    #[error("{0} is neither a .docx nor a .xlsx name")]
    UnknownFormat(String),
    #[error("{0:?} is not a usable document name")]
    InvalidName(String),
    #[error("storage directory {dir}: {message}")]
    Storage { dir: PathBuf, message: String },
    #[error("cannot open {name}: {message}")]
    Open { name: String, message: String },
    #[error("document {0} stopped")]
    Stopped(String),
    #[error("cannot export {name}: {message}")]
    Export { name: String, message: String },
    #[error("this server was started without tokens and cannot take any")]
    NoTokens,
    #[error(transparent)]
    Tokens(#[from] TokensError),
    #[error("audit log {path}: {message}")]
    Audit { path: PathBuf, message: String },
}

struct RoomHandle {
    format: Format,
    commands: mpsc::Sender<Command>,
}

#[derive(Clone, Debug)]
struct Storage {
    dir: PathBuf,
    options: StorageOptions,
}

/// Hosts named documents. Cheap to clone; clones share the documents.
#[derive(Clone, Default)]
pub struct Server {
    rooms: Arc<Mutex<BTreeMap<String, RoomHandle>>>,
    storage: Option<Arc<Storage>>,
    /// The tokens in force; every connection watches them.
    tokens: Option<Arc<watch::Sender<Arc<Tokens>>>>,
    presence: PresenceLimits,
    /// How many messages may wait for one connection; the default when
    /// `None`.
    outgoing: Option<usize>,
    audit: Audit,
}

/// A name that is a single plain file name, so it can name a directory too.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
}

impl Server {
    /// A server that keeps documents in memory only.
    pub fn new() -> Self {
        Self::default()
    }

    /// A server whose documents are durable in `dir`, with every document
    /// already there reopened as it was.
    pub fn with_storage(dir: impl Into<PathBuf>) -> Result<Self, ServerError> {
        Self::with_storage_options(dir, StorageOptions::default())
    }

    pub fn with_storage_options(
        dir: impl Into<PathBuf>,
        options: StorageOptions,
    ) -> Result<Self, ServerError> {
        let dir = dir.into();
        let storage_error = |message: String| ServerError::Storage {
            dir: dir.clone(),
            message,
        };
        std::fs::create_dir_all(&dir).map_err(|e| storage_error(e.to_string()))?;
        let server = Self {
            rooms: Arc::default(),
            storage: Some(Arc::new(Storage {
                dir: dir.clone(),
                options,
            })),
            tokens: None,
            presence: PresenceLimits::default(),
            outgoing: None,
            audit: Audit::default(),
        };
        let entries = std::fs::read_dir(&dir).map_err(|e| storage_error(e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| storage_error(e.to_string()))?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(format) = Format::of(&name).filter(|_| valid_name(&name)) else {
                continue;
            };
            // A directory without a snapshot is an open that never finished.
            if newest_generation(&entry.path())
                .map_err(storage_error)?
                .is_none()
            {
                continue;
            }
            server.start(
                &name,
                format,
                Source::Stored {
                    dir: entry.path(),
                    options,
                },
            )?;
        }
        Ok(server)
    }

    /// Requires one of `tokens` on every route and socket from now on.
    pub fn with_tokens(mut self, tokens: Tokens) -> Self {
        self.tokens = Some(Arc::new(watch::Sender::new(Arc::new(tokens))));
        self
    }

    /// Limits what each connection may share as presence (instead of
    /// [`PresenceLimits::default`]).
    pub fn with_presence_limits(mut self, limits: PresenceLimits) -> Self {
        self.presence = limits;
        self
    }

    /// Lets at most `limit` messages wait for one connection (instead of
    /// [`DEFAULT_OUTGOING_LIMIT`]). A connection with more waiting reads too
    /// slowly: it is told `overloaded` and closed, and the others never wait
    /// for it.
    pub fn with_outgoing_limit(mut self, limit: usize) -> Self {
        self.outgoing = Some(limit.max(1));
        self
    }

    /// Appends a line to the file at `path` for every access: sockets
    /// connected, replicas joined, resumed and closed (and why), requests
    /// refused, files uploaded and downloaded — by token name, never by
    /// token.
    pub fn with_audit_log(
        mut self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, ServerError> {
        let path = path.as_ref();
        self.audit = Audit::open(path).map_err(|e| ServerError::Audit {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        Ok(self)
    }

    pub(crate) fn audit(&self) -> &Audit {
        &self.audit
    }

    pub(crate) fn tokens(&self) -> Option<Arc<Tokens>> {
        self.tokens.as_ref().map(|tokens| tokens.borrow().clone())
    }

    fn watch(&self) -> Option<watch::Receiver<Arc<Tokens>>> {
        self.tokens.as_ref().map(|tokens| tokens.subscribe())
    }

    /// Replaces the tokens of a server started with tokens. The next request
    /// is checked against the new ones, and every open socket at once: one
    /// whose token is gone, or no longer opens its document, is told why
    /// (`revoked`) and closed; one whose role changed has the new role for
    /// whatever it sends next.
    pub fn set_tokens(&self, tokens: Tokens) -> Result<(), ServerError> {
        let current = self.tokens.as_ref().ok_or(ServerError::NoTokens)?;
        current.send_replace(Arc::new(tokens));
        Ok(())
    }

    /// Reads the tokens file at `path` and takes its tokens.
    pub fn reload_tokens(&self, path: &std::path::Path) -> Result<(), ServerError> {
        if self.tokens.is_none() {
            return Err(ServerError::NoTokens);
        }
        self.set_tokens(Tokens::load(path)?)
    }

    /// Reads the tokens file at `path` every `every` and takes its tokens
    /// whenever its contents change. A file that cannot be read or does not
    /// parse is reported on stderr and the tokens in force stay. Runs until
    /// the handle is aborted; needs a Tokio runtime.
    pub fn watch_tokens(&self, path: PathBuf, every: Duration) -> JoinHandle<()> {
        let server = self.clone();
        // The file as it was last looked at, so each change is acted on (and
        // each problem reported) once. Read now, not in the task: a change
        // made before the task first runs must still count as one.
        let mut seen = std::fs::read(&path).map_err(|e| e.to_string());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let now = std::fs::read(&path).map_err(|e| e.to_string());
                if now == seen {
                    continue;
                }
                seen = now;
                match &seen {
                    Ok(_) => match server.reload_tokens(&path) {
                        Ok(()) => eprintln!("took new tokens from {}", path.display()),
                        Err(error) => eprintln!("kept the tokens in force: {error}"),
                    },
                    Err(error) => eprintln!(
                        "kept the tokens in force: cannot read {}: {error}",
                        path.display()
                    ),
                }
            }
        })
    }

    /// Opens `bytes` as the document `name` (its extension picks the format).
    /// On a durable server the document is stored before it is served.
    pub fn open(&self, name: &str, bytes: Vec<u8>) -> Result<Format, ServerError> {
        if !valid_name(name) {
            return Err(ServerError::InvalidName(name.to_string()));
        }
        let format =
            Format::of(name).ok_or_else(|| ServerError::UnknownFormat(name.to_string()))?;
        if self.rooms.lock().unwrap().contains_key(name) {
            return Err(ServerError::Exists(name.to_string()));
        }
        let source = match &self.storage {
            Some(storage) => Source::NewStored {
                bytes,
                dir: storage.dir.join(name),
                options: storage.options,
            },
            None => Source::File(bytes),
        };
        self.start(name, format, source)?;
        Ok(format)
    }

    fn start(&self, name: &str, format: Format, source: Source) -> Result<(), ServerError> {
        let commands = room::spawn(name, format, source).map_err(|message| ServerError::Open {
            name: name.to_string(),
            message,
        })?;
        let mut rooms = self.rooms.lock().unwrap();
        if rooms.contains_key(name) {
            return Err(ServerError::Exists(name.to_string()));
        }
        rooms.insert(name.to_string(), RoomHandle { format, commands });
        Ok(())
    }

    pub fn documents(&self) -> Vec<(String, Format)> {
        self.rooms
            .lock()
            .unwrap()
            .iter()
            .map(|(name, room)| (name.clone(), room.format))
            .collect()
    }

    fn commands(&self, name: &str) -> Result<mpsc::Sender<Command>, ServerError> {
        self.rooms
            .lock()
            .unwrap()
            .get(name)
            .map(|room| room.commands.clone())
            .ok_or_else(|| ServerError::NotFound(name.to_string()))
    }

    async fn ask<T>(
        &self,
        name: &str,
        command: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, ServerError> {
        let (reply, answer) = oneshot::channel();
        self.commands(name)?
            .send(command(reply))
            .map_err(|_| ServerError::Stopped(name.to_string()))?;
        answer
            .await
            .map_err(|_| ServerError::Stopped(name.to_string()))
    }

    /// The document as a file, with every committed operation applied.
    pub async fn export(&self, name: &str) -> Result<Vec<u8>, ServerError> {
        self.ask(name, |reply| Command::Export { reply })
            .await?
            .map_err(|message| ServerError::Export {
                name: name.to_string(),
                message,
            })
    }

    /// The document's log position.
    pub async fn seq(&self, name: &str) -> Result<u64, ServerError> {
        self.ask(name, |reply| Command::Seq { reply }).await
    }

    /// Operations the model committed but jade could not realise (bugs).
    pub async fn faults(&self, name: &str) -> Result<Vec<String>, ServerError> {
        self.ask(name, |reply| Command::Faults { reply }).await
    }

    /// `GET /documents`, `GET|PUT /documents/{name}`, and the editing socket
    /// at `GET /documents/{name}/ws`; all behind the tokens, if any, and
    /// each document behind the documents its token opens.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/documents", get(list))
            .route("/documents/{name}", get(download).put(upload))
            .route("/documents/{name}/ws", get(socket))
            .layer(middleware::from_fn_with_state(self.clone(), authenticate))
            .with_state(self.clone())
    }

    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        axum::serve(listener, self.router()).await
    }
}

/// The documents the token may open.
async fn list(
    State(server): State<Server>,
    grant: Option<Extension<Grant>>,
) -> Json<Vec<serde_json::Value>> {
    let grant = grant.map(|Extension(grant)| grant);
    Json(
        server
            .documents()
            .into_iter()
            .filter(|(name, _)| access(grant.as_ref(), name).is_some())
            .map(|(name, format)| serde_json::json!({ "name": name, "format": format }))
            .collect(),
    )
}

fn error_response(error: ServerError) -> Response {
    let status = match error {
        ServerError::NotFound(_) => StatusCode::NOT_FOUND,
        ServerError::Exists(_) => StatusCode::CONFLICT,
        ServerError::UnknownFormat(_) | ServerError::InvalidName(_) | ServerError::Open { .. } => {
            StatusCode::BAD_REQUEST
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string()).into_response()
}

async fn download(
    Path(name): Path<String>,
    State(server): State<Server>,
    grant: Option<Extension<Grant>>,
) -> Response {
    let grant = grant.map(|Extension(grant)| grant);
    if access(grant.as_ref(), &name).is_none() {
        refused(&server, grant.as_ref(), &name, StatusCode::FORBIDDEN);
        return forbidden(&name);
    }
    match server.export(&name).await {
        Ok(bytes) => {
            server.audit().record(
                "downloaded",
                serde_json::json!({ "document": name, "token": token_name(grant.as_ref()) }),
            );
            ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
        }
        Err(error) => error_response(error),
    }
}

async fn upload(
    Path(name): Path<String>,
    State(server): State<Server>,
    grant: Option<Extension<Grant>>,
    body: Bytes,
) -> Response {
    let grant = grant.map(|Extension(grant)| grant);
    match access(grant.as_ref(), &name) {
        Some(Role::Editor) => {}
        Some(Role::Viewer) => {
            refused(&server, grant.as_ref(), &name, StatusCode::FORBIDDEN);
            return (StatusCode::FORBIDDEN, "viewers cannot upload documents").into_response();
        }
        None => {
            refused(&server, grant.as_ref(), &name, StatusCode::FORBIDDEN);
            return forbidden(&name);
        }
    }
    let audit = server.audit().clone();
    let entry = serde_json::json!({ "document": name, "token": token_name(grant.as_ref()) });
    let opened = tokio::task::spawn_blocking(move || server.open(&name, body.to_vec())).await;
    if matches!(opened, Ok(Ok(_))) {
        audit.record("uploaded", entry);
    }
    match opened {
        Ok(Ok(format)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "format": format })),
        )
            .into_response(),
        Ok(Err(error)) => error_response(error),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn socket(
    Path(name): Path<String>,
    State(server): State<Server>,
    grant: Option<Extension<Grant>>,
    presented: Option<Extension<Presented>>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let grant = grant.map(|Extension(grant)| grant);
    // Access is decided before anything else is looked at.
    let Some(role) = access(grant.as_ref(), &name) else {
        refused(&server, grant.as_ref(), &name, StatusCode::FORBIDDEN);
        return forbidden(&name);
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let tokens = match (presented, server.watch()) {
        (Some(Extension(Presented(token))), Some(tokens)) => Some(Watched {
            token,
            document: name.clone(),
            tokens,
        }),
        _ => None,
    };
    let settings = Settings {
        document: name.clone(),
        presence: server.presence,
        outgoing: server.outgoing.unwrap_or(DEFAULT_OUTGOING_LIMIT),
        audit: server.audit().clone(),
    };
    match server.commands(&name) {
        Ok(commands) => upgrade.on_upgrade(move |socket| {
            connection(
                socket,
                commands,
                Access {
                    grant,
                    role,
                    tokens,
                },
                settings,
            )
        }),
        Err(error) => error_response(error),
    }
}

/// What a socket may do, on a server with tokens kept up to date with them.
struct Access {
    /// Who the socket belongs to, on a server with tokens.
    grant: Option<Grant>,
    /// What it may do with its document.
    role: Role,
    tokens: Option<Watched>,
}

/// The token a socket presented, and the server's tokens as they change.
struct Watched {
    token: String,
    document: String,
    tokens: watch::Receiver<Arc<Tokens>>,
}

impl Access {
    /// Waits until the server's tokens change; never, without tokens.
    async fn changed(&mut self) {
        if let Some(watched) = &mut self.tokens {
            if watched.tokens.changed().await.is_ok() {
                return;
            }
        }
        // No tokens, or the server is gone: nothing will change.
        std::future::pending().await
    }

    /// Takes the tokens now in force, or says why the socket must close.
    fn refresh(&mut self) -> Result<(), String> {
        let Some(watched) = &mut self.tokens else {
            return Ok(());
        };
        let tokens = watched.tokens.borrow_and_update().clone();
        let (grant, role) = recheck(&tokens, &watched.token, &watched.document)?;
        self.grant = Some(grant);
        self.role = role;
        Ok(())
    }
}

/// What a connection is held to, and where it is recorded.
struct Settings {
    document: String,
    presence: PresenceLimits,
    /// Messages that may wait for the connection.
    outgoing: usize,
    audit: Audit,
}

/// Why the server ends a connection.
enum Ending {
    /// The token was withdrawn or lost access to the document.
    Revoked(String),
    /// More waited for the connection than its queue holds.
    Overloaded,
    /// The document's thread is gone.
    Stopped,
}

/// The token's name, for the audit log; never the token.
fn token_name(grant: Option<&Grant>) -> serde_json::Value {
    grant.map_or(serde_json::Value::Null, |grant| grant.name.clone().into())
}

/// Records that `grant` was refused `document` with `status`.
fn refused(server: &Server, grant: Option<&Grant>, document: &str, status: StatusCode) {
    server.audit().record(
        "refused",
        serde_json::json!({
            "document": document,
            "token": token_name(grant),
            "status": status.as_u16(),
        }),
    );
}

/// One socket, doing what `access` allows and sharing presence within
/// the limits of `settings`.
async fn connection(
    socket: WebSocket,
    commands: mpsc::Sender<Command>,
    mut access: Access,
    settings: Settings,
) {
    let Settings {
        document,
        presence: limits,
        outgoing: limit,
        audit,
    } = settings;
    let (mut sink, mut stream) = socket.split();
    let (outbox, mut outgoing, overloaded) = outbox(limit);
    // The last word, if the server closes the socket: said, then closed.
    let (closer, close) = oneshot::channel::<(ServerMessage, u16, &'static str)>();
    let mut writer = tokio::spawn(async move {
        tokio::pin!(close);
        loop {
            tokio::select! {
                biased;
                last = &mut close => {
                    if let Ok((last, code, reason)) = last {
                        let _ = sink.send(Message::Text(last.to_json().into())).await;
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code,
                                reason: reason.into(),
                            })))
                            .await;
                    }
                    break;
                }
                message = outgoing.recv() => {
                    let Some(message) = message else { break };
                    if sink.send(Message::Text(message.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let say = |message: ServerMessage| {
        let _ = outbox.send(message.to_json());
    };
    audit.record(
        "connected",
        serde_json::json!({
            "document": document,
            "token": token_name(access.grant.as_ref()),
            "role": access.role,
        }),
    );
    let mut replica = None;
    let opened = std::time::Instant::now();
    let mut presence_rate = Bucket::new(limits);
    // The tokens may have changed since the request was let in.
    let mut ending = access.refresh().err().map(Ending::Revoked);
    while ending.is_none() {
        let message = tokio::select! {
            // A change of tokens comes before anything sent after it.
            biased;
            () = access.changed() => {
                ending = access.refresh().err().map(Ending::Revoked);
                continue;
            }
            () = overloaded.wait() => {
                ending = Some(Ending::Overloaded);
                continue;
            }
            message = stream.next() => message,
        };
        let Some(Ok(message)) = message else { break };
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };
        let message = match serde_json::from_str::<ClientMessage>(&text) {
            Ok(message) => message,
            Err(error) => {
                say(ServerMessage::Error {
                    message: format!("not a client message: {error}"),
                });
                continue;
            }
        };
        match message {
            ClientMessage::Join | ClientMessage::Resume { .. } if replica.is_some() => {
                say(ServerMessage::Error {
                    message: "this connection already has a replica".into(),
                })
            }
            ClientMessage::Join => {
                let (reply, answer) = oneshot::channel();
                let sent = commands.send(Command::Join {
                    outbox: outbox.clone(),
                    reply,
                });
                match (sent, answer.await) {
                    (Ok(()), Ok(id)) => {
                        replica = Some(id);
                        audit.record(
                            "joined",
                            serde_json::json!({
                                "document": document,
                                "token": token_name(access.grant.as_ref()),
                                "replica": id.0,
                            }),
                        );
                    }
                    _ => ending = Some(Ending::Stopped),
                }
            }
            ClientMessage::Resume { replica: id, seq } => {
                let (reply, answer) = oneshot::channel();
                if commands
                    .send(Command::Resume {
                        replica: id,
                        seq,
                        outbox: outbox.clone(),
                        reply,
                    })
                    .is_err()
                {
                    ending = Some(Ending::Stopped);
                    continue;
                }
                match answer.await {
                    Ok(Ok(())) => {
                        replica = Some(id);
                        audit.record(
                            "resumed",
                            serde_json::json!({
                                "document": document,
                                "token": token_name(access.grant.as_ref()),
                                "replica": id.0,
                                "seq": seq,
                            }),
                        );
                    }
                    Ok(Err(
                        error @ (SyncError::Compacted { .. } | SyncError::UnknownReplica(_)),
                    )) => say(ServerMessage::Rejoin {
                        message: error.to_string(),
                    }),
                    Ok(Err(error)) => say(ServerMessage::Error {
                        message: error.to_string(),
                    }),
                    Err(_) => ending = Some(Ending::Stopped),
                }
            }
            ClientMessage::Presence { state } => match replica {
                Some(replica) => {
                    // Over a limit, it is dropped here, before the
                    // document's thread sees it; only the sender hears of
                    // it, and operations are not touched.
                    let admitted = limits.check_size(&state).and_then(|()| {
                        if presence_rate.allow(opened.elapsed()) {
                            Ok(())
                        } else {
                            Err(limits.rate_message())
                        }
                    });
                    if let Err(reason) = admitted {
                        say(ServerMessage::PresenceRejected { reason });
                        continue;
                    }
                    let command = Command::Presence {
                        replica,
                        name: access.grant.as_ref().map(|grant| grant.name.clone()),
                        state,
                        outbox: outbox.clone(),
                    };
                    if commands.send(command).is_err() {
                        ending = Some(Ending::Stopped);
                    }
                }
                None => say(ServerMessage::Error {
                    message: "join or resume before sending presence".into(),
                }),
            },
            ClientMessage::Submit(submission) => match replica {
                // Answered here: a viewer's operation never reaches the
                // document, and its replica has nothing of its own in the log.
                Some(_) if access.role != Role::Editor => say(ServerMessage::Rejected(Rejected {
                    client_seq: submission.client_seq,
                    reason: Rejection::new("this token is read-only (viewer)"),
                })),
                Some(replica) => {
                    let command = Command::Submit {
                        replica,
                        author: access.grant.as_ref().map(|grant| grant.name.clone()),
                        submission,
                    };
                    if commands.send(command).is_err() {
                        ending = Some(Ending::Stopped);
                    }
                }
                None => say(ServerMessage::Error {
                    message: "join or resume before submitting".into(),
                }),
            },
        }
    }
    if let Some(replica) = replica {
        let _ = commands.send(Command::Leave {
            replica,
            outbox: outbox.clone(),
        });
    }
    let reason = match &ending {
        None => "client",
        Some(Ending::Revoked(_)) => "revoked",
        Some(Ending::Overloaded) => "overloaded",
        Some(Ending::Stopped) => "stopped",
    };
    audit.record(
        "closed",
        serde_json::json!({
            "document": document,
            "token": token_name(access.grant.as_ref()),
            "replica": replica.map(|replica| replica.0),
            "reason": reason,
        }),
    );
    let last = match ending {
        Some(Ending::Revoked(message)) => Some((
            ServerMessage::Revoked {
                message: format!("access revoked: {message}"),
            },
            close_code::POLICY,
            "access revoked",
        )),
        Some(Ending::Overloaded) => Some((
            ServerMessage::Overloaded {
                message: format!(
                    "more than {limit} messages waited for this connection; resume to catch up"
                ),
            },
            close_code::AGAIN,
            "reading too slowly",
        )),
        None | Some(Ending::Stopped) => None,
    };
    if let Some(last) = last {
        let _ = closer.send(last);
        let _ = tokio::time::timeout(LAST_WORD_GRACE, &mut writer).await;
    }
    // The socket is gone; nothing more can be written to it.
    writer.abort();
}

/// How long a closing connection gets to take its last word before the
/// socket is dropped anyway.
const LAST_WORD_GRACE: Duration = Duration::from_secs(10);
