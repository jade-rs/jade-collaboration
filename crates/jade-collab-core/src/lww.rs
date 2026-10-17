//! Last-writer-wins registers.

use crate::id::Id;
use serde::{Deserialize, Serialize};

/// A register whose value is the one written with the greatest stamp. Writes
/// commute, and re-applying a write is a no-op, so any delivery order of the
/// same writes gives the same register (Lean: `Lww.converges`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LwwRegister<T> {
    entry: Option<(Id, T)>,
}

impl<T> Default for LwwRegister<T> {
    fn default() -> Self {
        Self { entry: None }
    }
}

impl<T> LwwRegister<T> {
    pub fn new(stamp: Id, value: T) -> Self {
        Self {
            entry: Some((stamp, value)),
        }
    }

    /// Applies a write; returns whether it became the current value.
    pub fn apply(&mut self, stamp: Id, value: T) -> bool {
        if self
            .entry
            .as_ref()
            .is_some_and(|(current, _)| *current >= stamp)
        {
            return false;
        }
        self.entry = Some((stamp, value));
        true
    }

    /// Whether a write with `stamp` would win.
    pub fn would_accept(&self, stamp: Id) -> bool {
        self.entry
            .as_ref()
            .is_none_or(|(current, _)| *current < stamp)
    }

    pub fn get(&self) -> Option<&T> {
        self.entry.as_ref().map(|(_, value)| value)
    }

    /// The current value, for a rewrite every replica performs identically
    /// at the same point of the log (e.g. moving a formula's references when
    /// rows are inserted). The stamp does not change.
    pub fn value_mut(&mut self) -> Option<&mut T> {
        self.entry.as_mut().map(|(_, value)| value)
    }

    pub fn stamp(&self) -> Option<Id> {
        self.entry.as_ref().map(|(stamp, _)| *stamp)
    }
}

impl<T: Clone> LwwRegister<T> {
    pub fn merge(&mut self, other: &Self) {
        if let Some((stamp, value)) = &other.entry {
            self.apply(*stamp, value.clone());
        }
    }
}
