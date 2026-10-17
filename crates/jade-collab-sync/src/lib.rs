//! The format-neutral sync engine.
//!
//! A document type implements [`Document`]: it applies identifier-addressed
//! operations atomically and says which [`Lane`] each one travels. The
//! [`Sequencer`] is the server core — it orders every submission into one log,
//! validates it against the authoritative state and hands committed effects to
//! a [`Materializer`] (the jade document on the server). A [`Replica`] is the
//! client core — it applies local operations optimistically, applies remote
//! convergent operations straight onto its view, and replays its pending queue
//! only when a remote sequenced operation arrives. See `docs/design.md`.

pub mod protocol;
mod replica;
mod sequencer;
pub mod sim;

use jade_collab_core::id::ReplicaId;
use serde::{Deserialize, Serialize};
use std::fmt;

pub use replica::{Replica, ReplicaEvent};
pub use sequencer::{Sequencer, SubmitOutcome};

/// How an operation is reconciled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Invariant-confluent: commutes with every other operation, applied by
    /// replicas as soon as it arrives and never rejected for concurrency.
    Convergent,
    /// Needs coordination: validated by the sequencer in log order; replicas
    /// treat it as tentative until it is committed.
    Sequenced,
}

/// What an operation is applied under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplyContext {
    pub origin: ReplicaId,
    /// The last log position the origin had seen when it made the operation.
    pub base_seq: u64,
    /// The log position the operation is committed at; `None` while it is
    /// only tentative on its origin.
    pub seq: Option<u64>,
}

/// Why an operation was not applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    message: String,
}

impl Rejection {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl<E: std::error::Error> From<E> for Rejection {
    fn from(error: E) -> Self {
        Self::new(error.to_string())
    }
}

/// A collaboratively edited document model.
pub trait Document: Clone {
    type Op: Clone + fmt::Debug + PartialEq;
    /// What the server needs to realise a committed operation in the real
    /// file (e.g. the jade commands it maps to). `()` for pure models.
    type Effect;

    fn lane(op: &Self::Op) -> Lane;

    /// Applies `op`, or changes nothing and says why not. Must be
    /// deterministic: every replica applying the same operation to the same
    /// state gets the same state.
    fn apply(&mut self, op: &Self::Op, ctx: &ApplyContext) -> Result<Self::Effect, Rejection>;

    /// The greatest Lamport time of any identifier in the document, so a
    /// replica's clock can stay ahead of everything it has seen.
    fn max_lamport(&self) -> u64;
}

/// Realises committed operations outside the model — on the server, in a jade
/// document. Called once per committed operation, in log order, with the
/// model as it is right after the operation.
pub trait Materializer<D: Document> {
    /// Called before a *sequenced* operation is applied, with the model as it
    /// is before it: the real document may refuse what the model would accept
    /// (the rejection goes back to the origin like any other). Never called
    /// for convergent operations, which must commit wherever they land.
    fn admit(&self, _document: &D, _op: &D::Op) -> Result<(), Rejection> {
        Ok(())
    }

    fn materialize(
        &mut self,
        document: &D,
        effect: D::Effect,
        ctx: &ApplyContext,
    ) -> Result<(), String>;
}

/// The materializer of a server that keeps only the model.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoMaterializer;

impl<D: Document> Materializer<D> for NoMaterializer {
    fn materialize(&mut self, _: &D, _: D::Effect, _: &ApplyContext) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("replica {0} never joined this document")]
    UnknownReplica(ReplicaId),
    #[error("cannot resume from log position {requested}; the log is at {current}")]
    FutureSeq { requested: u64, current: u64 },
    #[error("expected log position {expected}, received {received}")]
    OutOfOrder { expected: u64, received: u64 },
    #[error("the server answered submission {client_seq}, which is not the oldest pending one")]
    UnexpectedReply { client_seq: u64 },
    #[error("committed operation {seq} does not apply here: {reason}")]
    Diverged { seq: u64, reason: String },
}
