//! The format-neutral sync engine.
//!
//! A document type implements [`Document`]: it applies identifier-addressed
//! operations atomically and says which [`Lane`] each one travels. The
//! [`Sequencer`] is the server core — it orders every submission into one log,
//! validates it against the authoritative state and hands committed effects to
//! a [`Materializer`] (the jade document on the server). A [`Replica`] is the
//! client core — it applies local operations optimistically, applies remote
//! convergent operations straight onto its view when its pending queue is also
//! convergent, and otherwise replays that queue. See `docs/design.md`.

pub mod protocol;
mod replica;
mod sequencer;
pub mod sim;

use jade_collab_core::id::{Id, ReplicaId};
use serde::{Deserialize, Serialize};
use std::fmt;

pub use replica::{Replica, ReplicaEvent};
pub use sequencer::{Checkpoint, Record, Sequencer, SubmitOutcome};

/// How an operation is reconciled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Invariant-confluent: commutes with other convergent operations, applied by
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

/// A newly minted identifier or write stamp must belong to its submitting
/// replica. Existing targets/anchors are shared references and are not checked.
pub fn check_id_owner(id: Id, origin: ReplicaId) -> Result<(), Rejection> {
    if id.replica == origin {
        Ok(())
    } else {
        Err(Rejection::new(format!(
            "new identifier {id} does not belong to submitting replica {origin}"
        )))
    }
}

/// A collaboratively edited document model.
///
/// Clones must be independent logical states: applying an operation to one
/// clone must not change another. Immutable sharing or copy-on-write storage
/// is fine. Replicas use clones for confirmed/optimistic views and edit plans.
pub trait Document: Clone {
    type Op: Clone + fmt::Debug + PartialEq;
    /// What the server needs to realise a committed operation in the real
    /// file (e.g. the jade commands it maps to). `()` for pure models.
    type Effect;

    fn lane(op: &Self::Op) -> Lane;

    /// Pure admission for new user submissions, before model/file work. Formats
    /// check minted IDs and stamps here; references to existing content may be
    /// from any replica. Committed history and origin-derived facts bypass this
    /// policy, retaining their original meaning across software upgrades.
    fn validate_submission(&self, _op: &Self::Op, _origin: ReplicaId) -> Result<(), Rejection> {
        Ok(())
    }

    /// Applies `op`, or changes nothing and says why not. Must be
    /// deterministic: every replica applying the same operation to the same
    /// state gets the same state. A convergent operation must also commute
    /// with other convergent operations and must not depend on `ctx.seq` (it is
    /// applied tentatively and not reapplied on acknowledgement); a sequenced
    /// one may. Sequenced guards can depend on convergent changes, so a mixed
    /// optimistic queue must be replayed when confirmed state advances.
    fn apply(&mut self, op: &Self::Op, ctx: &ApplyContext) -> Result<Self::Effect, Rejection>;

    /// Applies a trusted committed fact, including one admitted under older
    /// resource policy. Logical validity must still be checked. Transports
    /// never use this for a new client submission; recovery and downstream
    /// replication may use it after validating the log/ticket metadata.
    /// It must agree with `apply` for currently admitted operations: only
    /// admission policy may be relaxed, not the operation's meaning.
    fn apply_recorded(
        &mut self,
        op: &Self::Op,
        ctx: &ApplyContext,
    ) -> Result<Self::Effect, Rejection> {
        self.apply(op, ctx)
    }

    /// The greatest Lamport time of any identifier in the document, so a
    /// replica's clock can stay ahead of everything it has seen.
    fn max_lamport(&self) -> u64;
}

/// Realises committed operations outside the model — on the server, in a jade
/// document. Effects run in log order with the model right after the operation.
/// Causes that may derive run on independent staged copies: callbacks can also
/// run on a discarded copy when the whole submission is refused.
pub trait Materializer<D: Document> {
    /// Whether this cause can produce derived operations or a derivation error.
    /// False promises an empty, infallible derivation; the sequencer skips it.
    /// This pure classification lets ordinary typing avoid file copies.
    fn may_derive(&self, document: &D, cause: &D::Op) -> bool;

    /// An independent logical copy of the real file and its replication
    /// bindings. Writes/derivation on the copy must not affect this instance.
    /// Only called for causes classified by `may_derive`; failure refuses the
    /// submission before touching live state. Reopening an exported package is
    /// permitted when a native editor cannot clone its internal state.
    fn fork(&self, document: &D) -> Result<Self, String>
    where
        Self: Sized;

    /// Called before a *sequenced* operation is applied, with the model as it
    /// is before it: the real document may refuse what the model would accept
    /// (the rejection goes back to the origin like any other). Never called
    /// for convergent operations, which must commit wherever they land.
    fn admit(&self, _document: &D, _op: &D::Op) -> Result<(), Rejection> {
        Ok(())
    }

    /// What the real document did on its own after `cause` was committed
    /// that the model cannot predict (a spreadsheet naming a new table
    /// column, say), as operations the server commits right after `cause` on
    /// behalf of [`ReplicaId::ORIGIN`]. Never called when a log is replayed:
    /// derived operations are in the log.
    /// An error aborts the entire staged cause, including file writes. In
    /// particular, allocating origin identifiers must refuse exhaustion.
    fn derived(&mut self, _document: &D, _cause: &D::Op) -> Result<Vec<D::Op>, Rejection> {
        Ok(Vec::new())
    }

    /// An unexpected error may leave partial native writes. It aborts a staged
    /// copy; on the live fast path it fences the sequencer until recovery.
    /// Reject expected input/native constraints in pure validation/admit first.
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
    fn may_derive(&self, _: &D, _: &D::Op) -> bool {
        false
    }
    fn fork(&self, _: &D) -> Result<Self, String> {
        Ok(*self)
    }
    fn materialize(&mut self, _: &D, _: D::Effect, _: &ApplyContext) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("replica {replica} belongs to a different principal")]
    ReplicaOwnerMismatch { replica: ReplicaId },
    #[error("replica {0} never joined this document")]
    UnknownReplica(ReplicaId),
    #[error("cannot resume from log position {requested}; the log is at {current}")]
    FutureSeq { requested: u64, current: u64 },
    #[error(
        "cannot resume from log position {requested}; the log before {oldest} was \
         compacted into a checkpoint, so join again as a new replica"
    )]
    Compacted { requested: u64, oldest: u64 },
    #[error("the journal does not fit this sequencer: {reason}")]
    Journal { reason: String },
    #[error("expected log position {expected}, received {received}")]
    OutOfOrder { expected: u64, received: u64 },
    #[error("the server answered submission {client_seq}, which is not the oldest pending one")]
    UnexpectedReply { client_seq: u64 },
    #[error("invalid resume response: {reason}")]
    InvalidResume { reason: String },
    #[error("the document has reached the last log position")]
    LogExhausted,
    #[error("replica identifier space exhausted")]
    ReplicaIdExhausted,
    /// The working model/file may be partially changed. No verdict or
    /// checkpoint may be published; recover from the last healthy durable state.
    #[error("native materialization failed at log position {seq}: {reason}")]
    Materialization { seq: u64, reason: String },
    #[error("committed operation {seq} does not apply here: {reason}")]
    Diverged { seq: u64, reason: String },
}
