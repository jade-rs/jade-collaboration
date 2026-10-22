//! A WebSocket server that hosts jade documents for collaborative editing,
//! and the matching client.
//!
//! Each document lives on its own thread (jade documents are not `Send`)
//! with a [`jade_collab_sync::Sequencer`] and the jade document it
//! materializes into. Connections talk JSON ([`wire`]) to that thread
//! through channels. HTTP routes list documents, download the current file
//! and upload new ones. With a storage directory every document is durable:
//! a checkpoint of the model and the file, and a journal after it. With
//! [`Tokens`] every route and socket needs a bearer token, which may be
//! limited to some documents; the tokens can be replaced while the server
//! runs ([`Server::set_tokens`], [`Server::watch_tokens`]). Replicas share
//! ephemeral [`Presence`] (cursors, selections) through the same socket.
//! A connection that stops reading is dropped once its bounded outgoing
//! queue is full ([`Server::with_outgoing_limit`]), and every access can be
//! written to an audit log ([`Server::with_audit_log`]).

mod address;
mod audit;
mod auth;
mod client;
mod limits;
mod mailbox;
mod outbox;
mod registry;
mod room;
mod server;
mod storage;
pub mod transport;
pub mod wire;

pub use address::{document_name_from_url, document_path, document_socket_path};
pub use auth::{DocumentRole, DocumentRule, Grant, Role, Tokens, TokensError};
pub use client::{
    Client, ClientEdit, ClientError, ClientSnapshot, EditError, Inspection, Presence,
    StaleSnapshot, DEFAULT_CONNECT_TIMEOUT,
};
pub use limits::PresenceLimits;
pub use mailbox::{DEFAULT_INCOMING_BYTES, DEFAULT_INCOMING_LIMIT};
pub use outbox::DEFAULT_OUTGOING_LIMIT;
pub use server::{Server, ServerError, DEFAULT_UPLOAD_LIMIT};
pub use storage::{Fsync, StorageOptions};
pub use wire::Format;
