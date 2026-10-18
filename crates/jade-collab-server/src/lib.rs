//! A WebSocket server that hosts jade documents for collaborative editing,
//! and the matching client.
//!
//! Each document lives on its own thread (jade documents are not `Send`)
//! with a [`jade_collab_sync::Sequencer`] and the jade document it
//! materializes into. Connections talk JSON ([`wire`]) to that thread
//! through channels. HTTP routes list documents, download the current file
//! and upload new ones. With a storage directory every document is durable:
//! a checkpoint of the model and the file, and a journal after it. With
//! [`Tokens`] every route and socket needs a bearer token. Replicas share
//! ephemeral [`Presence`] (cursors, selections) through the same socket.

mod auth;
mod client;
mod room;
mod server;
mod storage;
pub mod wire;

pub use auth::{Grant, Role, Tokens, TokensError};
pub use client::{Client, ClientError, Presence};
pub use server::{Server, ServerError};
pub use storage::StorageOptions;
pub use wire::Format;
