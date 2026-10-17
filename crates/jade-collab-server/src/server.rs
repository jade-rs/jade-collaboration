use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Mutex};

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc::unbounded_channel, oneshot};

use crate::room::{self, Command};
use crate::wire::{ClientMessage, Format, ServerMessage};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("no document named {0}")]
    NotFound(String),
    #[error("a document named {0} is already open")]
    Exists(String),
    #[error("{0} is neither a .docx nor a .xlsx name")]
    UnknownFormat(String),
    #[error("cannot open {name}: {message}")]
    Open { name: String, message: String },
    #[error("document {0} stopped")]
    Stopped(String),
    #[error("cannot export {name}: {message}")]
    Export { name: String, message: String },
}

struct RoomHandle {
    format: Format,
    commands: mpsc::Sender<Command>,
}

/// Hosts named documents. Cheap to clone; clones share the documents.
#[derive(Clone, Default)]
pub struct Server {
    rooms: Arc<Mutex<BTreeMap<String, RoomHandle>>>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens `bytes` as the document `name` (its extension picks the format).
    pub fn open(&self, name: &str, bytes: Vec<u8>) -> Result<Format, ServerError> {
        let format =
            Format::of(name).ok_or_else(|| ServerError::UnknownFormat(name.to_string()))?;
        if self.rooms.lock().unwrap().contains_key(name) {
            return Err(ServerError::Exists(name.to_string()));
        }
        let commands = room::spawn(name, format, bytes).map_err(|message| ServerError::Open {
            name: name.to_string(),
            message,
        })?;
        let mut rooms = self.rooms.lock().unwrap();
        if rooms.contains_key(name) {
            return Err(ServerError::Exists(name.to_string()));
        }
        rooms.insert(name.to_string(), RoomHandle { format, commands });
        Ok(format)
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
    /// at `GET /documents/{name}/ws`.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/documents", get(list))
            .route("/documents/{name}", get(download).put(upload))
            .route("/documents/{name}/ws", get(socket))
            .with_state(self.clone())
    }

    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        axum::serve(listener, self.router()).await
    }
}

async fn list(State(server): State<Server>) -> Json<Vec<serde_json::Value>> {
    Json(
        server
            .documents()
            .into_iter()
            .map(|(name, format)| serde_json::json!({ "name": name, "format": format }))
            .collect(),
    )
}

fn error_response(error: ServerError) -> Response {
    let status = match error {
        ServerError::NotFound(_) => StatusCode::NOT_FOUND,
        ServerError::Exists(_) => StatusCode::CONFLICT,
        ServerError::UnknownFormat(_) | ServerError::Open { .. } => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string()).into_response()
}

async fn download(Path(name): Path<String>, State(server): State<Server>) -> Response {
    match server.export(&name).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        Err(error) => error_response(error),
    }
}

async fn upload(Path(name): Path<String>, State(server): State<Server>, body: Bytes) -> Response {
    let opened = tokio::task::spawn_blocking(move || server.open(&name, body.to_vec())).await;
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
    upgrade: WebSocketUpgrade,
) -> Response {
    match server.commands(&name) {
        Ok(commands) => upgrade.on_upgrade(move |socket| connection(socket, commands)),
        Err(error) => error_response(error),
    }
}

async fn connection(socket: WebSocket, commands: mpsc::Sender<Command>) {
    let (mut sink, mut stream) = socket.split();
    let (outbox, mut outgoing) = unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if sink.send(Message::Text(message.into())).await.is_err() {
                break;
            }
        }
    });
    let say = |message: ServerMessage| {
        let _ = outbox.send(message.to_json());
    };
    let mut replica = None;
    while let Some(Ok(message)) = stream.next().await {
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
                if commands
                    .send(Command::Join {
                        outbox: outbox.clone(),
                        reply,
                    })
                    .is_err()
                {
                    break;
                }
                replica = answer.await.ok();
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
                    break;
                }
                match answer.await {
                    Ok(Ok(())) => replica = Some(id),
                    Ok(Err(message)) => say(ServerMessage::Error { message }),
                    Err(_) => break,
                }
            }
            ClientMessage::Submit(submission) => match replica {
                Some(replica) => {
                    if commands
                        .send(Command::Submit {
                            replica,
                            submission,
                        })
                        .is_err()
                    {
                        break;
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
    // The socket is gone; nothing more can be written to it.
    writer.abort();
}
