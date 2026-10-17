//! Messages between a replica and the sequencer. Transport-agnostic; the
//! WebSocket server wraps them in its own envelope.

use crate::Rejection;
use jade_collab_core::id::ReplicaId;
use serde::{Deserialize, Serialize};

/// An operation a replica asks the server to commit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Submission<Op> {
    /// Strictly increasing per replica; `(replica, client_seq)` identifies the
    /// submission, so resending after a reconnect is harmless.
    pub client_seq: u64,
    /// The last log position the replica had seen.
    pub base_seq: u64,
    pub op: Op,
}

/// An operation at its place in the log. Broadcast to every replica; for the
/// origin it doubles as the acknowledgement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Committed<Op> {
    pub seq: u64,
    pub origin: ReplicaId,
    pub client_seq: u64,
    pub base_seq: u64,
    pub op: Op,
}

/// Sent to the origin only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rejected {
    pub client_seq: u64,
    pub reason: Rejection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Downstream<Op> {
    Committed(Committed<Op>),
    Rejected(Rejected),
}

/// What a new replica starts from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome<D> {
    pub replica: ReplicaId,
    pub seq: u64,
    pub snapshot: D,
}

/// What a returning replica missed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resume<Op> {
    /// The newest of the replica's submissions the server has handled; any
    /// still-pending submission up to it that is not in `missed` was rejected.
    pub processed_client_seq: u64,
    pub missed: Vec<Committed<Op>>,
}
