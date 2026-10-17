//! Replicated Growable Array: the sequence CRDT behind every story's text.
//!
//! Each atom records the atom it was inserted after (its *parent*). The
//! document order is a depth-first walk of that tree with siblings in
//! descending [`Id`] order; deletion only marks a tombstone. [`spec`] states
//! exactly that and nothing else — it is the Rust mirror of
//! `lean/JadeCollab/Rga.lean` — and [`Rga`] is the incremental implementation
//! a property test checks against it.
//!
//! [`Rga`] keeps the walk materialized as a flat array and integrates an insert
//! with the classic skip rule: start right after the parent and pass every
//! atom with a greater id. That rule equals the tree walk only if every atom
//! has a greater Lamport time than its parent, which [`Rga::insert`] enforces
//! and [`crate::id::Clock`] guarantees for honest replicas.

use crate::id::Id;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node<T> {
    pub id: Id,
    pub parent: Option<Id>,
    pub value: T,
    pub deleted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RgaError {
    DuplicateId(Id),
    MissingParent(Id),
    MissingTarget(Id),
    /// The RGA skip rule needs every atom to be newer than its parent.
    NotNewerThanParent {
        id: Id,
        parent: Id,
    },
}

impl fmt::Display for RgaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RgaError::DuplicateId(id) => write!(f, "atom {id} already exists"),
            RgaError::MissingParent(id) => write!(f, "parent atom {id} does not exist"),
            RgaError::MissingTarget(id) => write!(f, "atom {id} does not exist"),
            RgaError::NotNewerThanParent { id, parent } => {
                write!(f, "atom {id} is not newer than its parent {parent}")
            }
        }
    }
}

impl std::error::Error for RgaError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rga<T> {
    nodes: Vec<Node<T>>,
}

impl<T> Default for Rga<T> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
}

impl<T> Rga<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every atom in document order, tombstones included.
    pub fn nodes(&self) -> &[Node<T>] {
        &self.nodes
    }

    pub fn ids(&self) -> Vec<Id> {
        self.nodes.iter().map(|node| node.id).collect()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Index of `id` in [`Rga::nodes`].
    pub fn position(&self, id: Id) -> Option<usize> {
        self.nodes.iter().position(|node| node.id == id)
    }

    pub fn get(&self, id: Id) -> Option<&Node<T>> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn contains(&self, id: Id) -> bool {
        self.position(id).is_some()
    }

    pub fn is_visible(&self, id: Id) -> bool {
        self.get(id).is_some_and(|node| !node.deleted)
    }

    pub fn max_lamport(&self) -> u64 {
        self.nodes
            .iter()
            .map(|node| node.id.lamport)
            .max()
            .unwrap_or(0)
    }

    pub fn visible(&self) -> impl Iterator<Item = (Id, &T)> {
        self.nodes
            .iter()
            .filter(|node| !node.deleted)
            .map(|node| (node.id, &node.value))
    }

    pub fn visible_len(&self) -> usize {
        self.nodes.iter().filter(|node| !node.deleted).count()
    }

    pub fn visible_id_at(&self, index: usize) -> Option<Id> {
        self.visible().nth(index).map(|(id, _)| id)
    }

    /// The number of visible atoms before `id`, if `id` is visible.
    pub fn visible_index(&self, id: Id) -> Option<usize> {
        let mut index = 0;
        for node in &self.nodes {
            if node.id == id {
                return (!node.deleted).then_some(index);
            }
            if !node.deleted {
                index += 1;
            }
        }
        None
    }

    /// The last visible atom strictly before `id` in document order.
    pub fn visible_before(&self, id: Id) -> Option<Id> {
        let position = self.position(id)?;
        self.nodes[..position]
            .iter()
            .rev()
            .find(|node| !node.deleted)
            .map(|node| node.id)
    }

    /// Where an atom with `id` inserted after `parent` lands, without inserting it.
    fn integration_point(&self, id: Id, parent: Option<Id>) -> Result<usize, RgaError> {
        if self.contains(id) {
            return Err(RgaError::DuplicateId(id));
        }
        let start = match parent {
            None => 0,
            Some(parent) => {
                let position = self
                    .position(parent)
                    .ok_or(RgaError::MissingParent(parent))?;
                if id.lamport <= parent.lamport {
                    return Err(RgaError::NotNewerThanParent { id, parent });
                }
                position + 1
            }
        };
        let skipped = self.nodes[start..]
            .iter()
            .take_while(|node| node.id > id)
            .count();
        Ok(start + skipped)
    }

    /// Inserts `value` after `parent` (`None`: at the start) and returns its
    /// index in [`Rga::nodes`]. A failed insert changes nothing.
    pub fn insert(&mut self, id: Id, parent: Option<Id>, value: T) -> Result<usize, RgaError> {
        let at = self.integration_point(id, parent)?;
        self.nodes.insert(
            at,
            Node {
                id,
                parent,
                value,
                deleted: false,
            },
        );
        Ok(at)
    }

    /// Inserts a chain — `values[0]` after `parent`, each next value after the
    /// previous one — with ids `first`, `first.offset(1)`, …. This is what
    /// typing a run produces; the run lands contiguously. Returns the index of
    /// the first atom. A failed insert changes nothing.
    pub fn insert_run(
        &mut self,
        first: Id,
        parent: Option<Id>,
        values: impl IntoIterator<Item = T>,
    ) -> Result<usize, RgaError> {
        let values: Vec<T> = values.into_iter().collect();
        if values.is_empty() {
            return self
                .position_after(parent)
                .ok_or_else(|| RgaError::MissingParent(parent.expect("start always exists")));
        }
        let at = self.integration_point(first, parent)?;
        let count = values.len() as u64;
        if let Some(existing) = self.nodes.iter().find(|node| {
            node.id.replica == first.replica && node.id > first && node.id < first.offset(count)
        }) {
            return Err(RgaError::DuplicateId(existing.id));
        }
        let mut previous = parent;
        let run: Vec<Node<T>> = values
            .into_iter()
            .enumerate()
            .map(|(k, value)| {
                let id = first.offset(k as u64);
                let node = Node {
                    id,
                    parent: previous,
                    value,
                    deleted: false,
                };
                previous = Some(id);
                node
            })
            .collect();
        self.nodes.splice(at..at, run);
        Ok(at)
    }

    fn position_after(&self, parent: Option<Id>) -> Option<usize> {
        match parent {
            None => Some(0),
            Some(parent) => self.position(parent).map(|p| p + 1),
        }
    }

    /// Marks `id` deleted; returns whether it was visible. Deleting a
    /// tombstone again is a no-op.
    pub fn delete(&mut self, id: Id) -> Result<bool, RgaError> {
        let node = self
            .nodes
            .iter_mut()
            .find(|node| node.id == id)
            .ok_or(RgaError::MissingTarget(id))?;
        let was_visible = !node.deleted;
        node.deleted = true;
        Ok(was_visible)
    }
}

/// The specification: a document is a pure function of the set of inserts and
/// the set of deleted ids. Mirrors `lean/JadeCollab/Rga.lean`.
pub mod spec {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Insert<T> {
        pub id: Id,
        pub parent: Option<Id>,
        pub value: T,
    }

    impl<T> Insert<T> {
        pub fn new(id: Id, parent: Option<Id>, value: T) -> Self {
            Self { id, parent, value }
        }
    }

    /// Document order of every inserted id: depth-first from the start, with
    /// the children of each atom in descending id order. Duplicate inserts
    /// count once; atoms whose parent never arrived are not reachable.
    pub fn linearize<T>(inserts: &[Insert<T>]) -> Vec<Id> {
        let mut children: BTreeMap<Option<Id>, BTreeSet<Id>> = BTreeMap::new();
        for insert in inserts {
            children.entry(insert.parent).or_default().insert(insert.id);
        }
        let mut order = Vec::new();
        let mut visited = BTreeSet::new();
        // Children are pushed in ascending order so the greatest pops first.
        let mut stack: Vec<Id> = children
            .get(&None)
            .map(|c| c.iter().copied().collect())
            .unwrap_or_default();
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            order.push(id);
            if let Some(c) = children.get(&Some(id)) {
                stack.extend(c.iter().copied());
            }
        }
        order
    }

    pub fn render<T: Clone>(inserts: &[Insert<T>], deleted: &BTreeSet<Id>) -> Vec<(Id, T)> {
        let mut values: BTreeMap<Id, &T> = BTreeMap::new();
        for insert in inserts {
            values.entry(insert.id).or_insert(&insert.value);
        }
        linearize(inserts)
            .into_iter()
            .filter(|id| !deleted.contains(id))
            .map(|id| (id, values[&id].clone()))
            .collect()
    }
}
