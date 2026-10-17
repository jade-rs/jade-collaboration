//! A WebSocket server that hosts jade documents for collaborative editing,
//! and the matching client.
//!
//! Each document lives on its own thread (jade documents are not `Send`)
//! with a [`jade_collab_sync::Sequencer`] and the jade document it
//! materializes into. Connections talk JSON ([`wire`]) to that thread
//! through channels. HTTP routes list documents, download the current file
//! and upload new ones.

mod client;
mod room;
mod server;
pub mod wire;

pub use client::{Client, ClientError};
pub use server::{Server, ServerError};
pub use wire::Format;
