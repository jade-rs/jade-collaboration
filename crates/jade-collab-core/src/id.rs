//! Replica identifiers, operation identifiers and Lamport clocks.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A participant in one document's session. The server hands these out and
/// never reuses one for the same document. [`ReplicaId::ORIGIN`] owns the
/// content that was loaded from the file.
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ReplicaId(pub u64);

impl ReplicaId {
    pub const ORIGIN: ReplicaId = ReplicaId(0);
}

impl fmt::Display for ReplicaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// A globally unique, totally ordered identifier: Lamport time first, then the
/// replica that minted it. Field order matters — the derived `Ord` is the
/// lexicographic order the RGA relies on.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Id {
    pub lamport: u64,
    pub replica: ReplicaId,
}

impl Id {
    pub const fn new(lamport: u64, replica: ReplicaId) -> Self {
        Self { lamport, replica }
    }

    /// The identifier `n` ticks after this one on the same replica — the ids a
    /// [`Clock::reserve`] run hands out.
    pub const fn offset(self, n: u64) -> Self {
        Self::new(self.lamport + n, self.replica)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.lamport, self.replica)
    }
}

/// A Lamport clock owned by one replica.
///
/// Every id the clock hands out is greater (in Lamport time) than every id it
/// has [`observe`](Clock::observe)d, so anything a replica inserts after an
/// atom it can see gets a larger id than that atom.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    replica: ReplicaId,
    lamport: u64,
}

impl Clock {
    pub fn new(replica: ReplicaId) -> Self {
        Self {
            replica,
            lamport: 0,
        }
    }

    pub fn replica(&self) -> ReplicaId {
        self.replica
    }

    /// The Lamport time of the last id handed out or observed.
    pub fn now(&self) -> u64 {
        self.lamport
    }

    pub fn observe(&mut self, id: Id) {
        self.observe_lamport(id.lamport);
    }

    pub fn observe_lamport(&mut self, lamport: u64) {
        self.lamport = self.lamport.max(lamport);
    }

    pub fn tick(&mut self) -> Id {
        self.reserve(1)
    }

    /// Reserves `n >= 1` consecutive ids and returns the first; the rest are
    /// `first.offset(1)`, …, `first.offset(n - 1)`.
    pub fn reserve(&mut self, n: u64) -> Id {
        assert!(n >= 1, "reserve at least one id");
        let first = Id::new(self.lamport + 1, self.replica);
        self.lamport += n;
        first
    }
}
