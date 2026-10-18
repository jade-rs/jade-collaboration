//! Replicated Growable Array: the sequence CRDT behind every story's text.
//!
//! Each atom records the atom it was inserted after (its *parent*). The
//! document order is a depth-first walk of that tree with siblings in
//! descending [`Id`] order; deletion only marks a tombstone. [`spec`] states
//! exactly that and nothing else — it is the Rust mirror of
//! `lean/JadeCollab/Rga.lean` — and [`Rga`] is the incremental implementation
//! a property test checks against it.
//!
//! [`Rga`] keeps the walk materialized in document order and integrates an
//! insert with the classic skip rule: start right after the parent and pass
//! every atom with a greater id. That rule equals the tree walk only if every
//! atom has a greater Lamport time than its parent, which [`Rga::insert`]
//! enforces and [`crate::id::Clock`] guarantees for honest replicas.
//!
//! # Layout
//!
//! The walk is cut into chunks of at most `CHUNK_CAPACITY` (128) atoms. A
//! hash map (foldhash, randomly seeded per process) sends each id to its
//! chunk, and two Fenwick trees over the chunks hold their atom and
//! visible-atom counts, so finding an id's position or visible index, finding
//! the atom at a visible index, inserting and deleting all cost
//! `O(log n + chunk)` instead of a scan of the whole story. Integration still
//! walks the atoms the skip rule passes, which is none for ordinary typing. A chunk that
//! outgrows its capacity is split in place; chunks are never merged, since
//! atoms are never removed. Serialization is the plain node list, in document
//! order, exactly as when the sequence was one array.

use crate::id::Id;
use foldhash::fast::RandomState;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::ops::{Bound, RangeBounds};
use std::sync::OnceLock;

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

/// The most atoms a chunk holds before it is split. Lookups scan one chunk;
/// a split rebuilds the per-chunk counts, `O(n / CHUNK_CAPACITY)`.
const CHUNK_CAPACITY: usize = 128;

/// A run of consecutive atoms in document order.
#[derive(Clone)]
struct Chunk<T> {
    /// Stable name of this chunk; [`Rga::slots`] maps it to its index.
    key: u32,
    nodes: Vec<Node<T>>,
    visible: usize,
}

/// Where an atom is, or where one goes: `nodes[offset]` of `chunks[chunk]`.
#[derive(Clone, Copy, Debug)]
struct Place {
    chunk: usize,
    offset: usize,
}

pub struct Rga<T> {
    /// The document order, cut into chunks. Never holds an empty chunk.
    chunks: Vec<Chunk<T>>,
    /// Chunk key → index in `chunks`.
    slots: Vec<u32>,
    /// Atom id → key of the chunk holding it.
    homes: HashMap<Id, u32, RandomState>,
    /// Atoms per chunk.
    lens: Fenwick,
    /// Visible atoms per chunk.
    visibles: Fenwick,
    len: usize,
    visible_len: usize,
    max_lamport: u64,
    capacity: usize,
    /// [`Rga::nodes`] of a multi-chunk sequence, built on demand and dropped
    /// by every change.
    flat: OnceLock<Vec<Node<T>>>,
}

impl<T> Default for Rga<T> {
    fn default() -> Self {
        Self::with_chunk_capacity(CHUNK_CAPACITY)
    }
}

impl<T> Rga<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// A sequence whose chunks hold at most `capacity` atoms. Only the
    /// layout changes, never the behaviour; tests use tiny chunks to make
    /// every insert cross chunk boundaries.
    #[doc(hidden)]
    pub fn with_chunk_capacity(capacity: usize) -> Self {
        Self {
            chunks: Vec::new(),
            slots: Vec::new(),
            homes: HashMap::default(),
            lens: Fenwick::default(),
            visibles: Fenwick::default(),
            len: 0,
            visible_len: 0,
            max_lamport: 0,
            capacity: capacity.max(1),
            flat: OnceLock::new(),
        }
    }

    /// Every atom in document order, tombstones included.
    ///
    /// A sequence longer than one chunk materializes this slice on the first
    /// call after a change (`O(n)`); prefer [`Rga::iter`] or
    /// [`Rga::nodes_in`] on hot paths.
    pub fn nodes(&self) -> &[Node<T>]
    where
        T: Clone,
    {
        match self.chunks.as_slice() {
            [] => &[],
            [only] => &only.nodes,
            _ => self.flat.get_or_init(|| self.iter().cloned().collect()),
        }
    }

    /// Every atom in document order, tombstones included.
    pub fn iter(&self) -> impl Iterator<Item = &Node<T>> + '_ {
        self.chunks.iter().flat_map(|chunk| chunk.nodes.iter())
    }

    /// The atoms at document positions `range` — what `&nodes()[range]`
    /// holds, without materializing the rest. Panics like slice indexing
    /// when the range is out of bounds.
    pub fn nodes_in(&self, range: impl RangeBounds<usize>) -> impl Iterator<Item = &Node<T>> + '_ {
        let start = match range.start_bound() {
            Bound::Included(&start) => start,
            Bound::Excluded(&start) => start.checked_add(1).expect("range start overflows"),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&end) => end.checked_add(1).expect("range end overflows"),
            Bound::Excluded(&end) => end,
            Bound::Unbounded => self.len,
        };
        assert!(start <= end, "range starts at {start} but ends at {end}");
        assert!(
            end <= self.len,
            "range end {end} out of range for a sequence of length {}",
            self.len
        );
        let (chunk, skip) = if start < self.len {
            let chunk = self.lens.find(start);
            (chunk, start - self.lens.prefix(chunk))
        } else {
            (self.chunks.len(), 0)
        };
        self.chunks[chunk..]
            .iter()
            .flat_map(|chunk| chunk.nodes.iter())
            .skip(skip)
            .take(end - start)
    }

    pub fn ids(&self) -> Vec<Id> {
        self.iter().map(|node| node.id).collect()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn locate(&self, id: Id) -> Option<Place> {
        let chunk = self.slots[*self.homes.get(&id)? as usize] as usize;
        let offset = self.chunks[chunk]
            .nodes
            .iter()
            .position(|node| node.id == id)
            .expect("an indexed atom is in its chunk");
        Some(Place { chunk, offset })
    }

    fn index_of(&self, place: Place) -> usize {
        self.lens.prefix(place.chunk) + place.offset
    }

    /// Visible atoms strictly before `place`.
    fn visible_before_place(&self, place: Place) -> usize {
        self.visibles.prefix(place.chunk)
            + self.chunks[place.chunk].nodes[..place.offset]
                .iter()
                .filter(|node| !node.deleted)
                .count()
    }

    /// Index of `id` in [`Rga::nodes`].
    pub fn position(&self, id: Id) -> Option<usize> {
        self.locate(id).map(|place| self.index_of(place))
    }

    pub fn get(&self, id: Id) -> Option<&Node<T>> {
        let place = self.locate(id)?;
        Some(&self.chunks[place.chunk].nodes[place.offset])
    }

    pub fn contains(&self, id: Id) -> bool {
        self.homes.contains_key(&id)
    }

    pub fn is_visible(&self, id: Id) -> bool {
        self.get(id).is_some_and(|node| !node.deleted)
    }

    pub fn max_lamport(&self) -> u64 {
        self.max_lamport
    }

    pub fn visible(&self) -> impl Iterator<Item = (Id, &T)> {
        self.chunks
            .iter()
            .filter(|chunk| chunk.visible > 0)
            .flat_map(|chunk| chunk.nodes.iter())
            .filter(|node| !node.deleted)
            .map(|node| (node.id, &node.value))
    }

    pub fn visible_len(&self) -> usize {
        self.visible_len
    }

    pub fn visible_id_at(&self, index: usize) -> Option<Id> {
        if index >= self.visible_len {
            return None;
        }
        let chunk = self.visibles.find(index);
        let local = index - self.visibles.prefix(chunk);
        self.chunks[chunk]
            .nodes
            .iter()
            .filter(|node| !node.deleted)
            .nth(local)
            .map(|node| node.id)
    }

    /// The number of visible atoms before `id`, if `id` is visible.
    pub fn visible_index(&self, id: Id) -> Option<usize> {
        let place = self.locate(id)?;
        if self.chunks[place.chunk].nodes[place.offset].deleted {
            return None;
        }
        Some(self.visible_before_place(place))
    }

    /// The last visible atom strictly before `id` in document order.
    pub fn visible_before(&self, id: Id) -> Option<Id> {
        let place = self.locate(id)?;
        let before = self.visible_before_place(place);
        before
            .checked_sub(1)
            .and_then(|index| self.visible_id_at(index))
    }

    /// Where an atom with `id` inserted after `parent` lands, without inserting it.
    fn integration_point(&self, id: Id, parent: Option<Id>) -> Result<Place, RgaError> {
        if self.contains(id) {
            return Err(RgaError::DuplicateId(id));
        }
        let Place {
            mut chunk,
            mut offset,
        } = match parent {
            None => Place {
                chunk: 0,
                offset: 0,
            },
            Some(parent) => {
                let place = self.locate(parent).ok_or(RgaError::MissingParent(parent))?;
                if id.lamport <= parent.lamport {
                    return Err(RgaError::NotNewerThanParent { id, parent });
                }
                Place {
                    chunk: place.chunk,
                    offset: place.offset + 1,
                }
            }
        };
        // The skip rule: pass every atom with a greater id. Stay at the end of
        // a chunk rather than the start of the next, so typing keeps
        // appending to the same chunk.
        while let Some(nodes) = self.chunks.get(chunk).map(|chunk| &chunk.nodes) {
            offset += nodes[offset..]
                .iter()
                .take_while(|node| node.id > id)
                .count();
            let next_is_skipped = self
                .chunks
                .get(chunk + 1)
                .is_some_and(|next| next.nodes[0].id > id);
            if offset < nodes.len() || !next_is_skipped {
                break;
            }
            chunk += 1;
            offset = 0;
        }
        Ok(Place { chunk, offset })
    }

    /// Inserts `value` after `parent` (`None`: at the start) and returns its
    /// index in [`Rga::nodes`]. A failed insert changes nothing.
    pub fn insert(&mut self, id: Id, parent: Option<Id>, value: T) -> Result<usize, RgaError> {
        let place = self.integration_point(id, parent)?;
        let at = self.index_of(place);
        self.splice(
            place,
            vec![Node {
                id,
                parent,
                value,
                deleted: false,
            }],
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
        let place = self.integration_point(first, parent)?;
        let count = values.len() as u64;
        // Of the run's later ids already present, report the first in
        // document order — by lookup, or by a scan when that is shorter.
        let taken =
            |id: &Id| id.replica == first.replica && *id > first && *id < first.offset(count);
        let existing = if count as usize > self.len {
            self.iter().map(|node| node.id).find(taken)
        } else {
            (1..count)
                .map(|k| first.offset(k))
                .filter_map(|id| self.position(id).map(|position| (position, id)))
                .min()
                .map(|(_, id)| id)
        };
        if let Some(existing) = existing {
            return Err(RgaError::DuplicateId(existing));
        }
        let at = self.index_of(place);
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
        self.splice(place, run);
        Ok(at)
    }

    fn position_after(&self, parent: Option<Id>) -> Option<usize> {
        match parent {
            None => Some(0),
            Some(parent) => self.position(parent).map(|p| p + 1),
        }
    }

    /// Puts fresh, visible, not yet indexed atoms at `place`.
    fn splice(&mut self, place: Place, run: Vec<Node<T>>) {
        self.flat.take();
        let count = run.len();
        if self.chunks.is_empty() {
            self.slots.push(0);
            self.chunks.push(Chunk {
                key: 0,
                nodes: Vec::new(),
                visible: 0,
            });
            self.lens = Fenwick::from_counts([0]);
            self.visibles = Fenwick::from_counts([0]);
        }
        let chunk = &mut self.chunks[place.chunk];
        let overflows = chunk.nodes.len() + count > self.capacity;
        self.homes.reserve(count);
        for node in &run {
            // An over-full chunk is split next, and that indexes every atom.
            if !overflows {
                self.homes.insert(node.id, chunk.key);
            }
            self.max_lamport = self.max_lamport.max(node.id.lamport);
        }
        chunk.nodes.splice(place.offset..place.offset, run);
        chunk.visible += count;
        self.len += count;
        self.visible_len += count;
        if overflows {
            self.split(place.chunk);
        } else {
            self.lens.add(place.chunk, count);
            self.visibles.add(place.chunk, count);
        }
    }

    /// Cuts an over-full chunk into chunks at most half full, indexes every
    /// atom in them, then rebuilds the per-chunk counts.
    fn split(&mut self, index: usize) {
        let half = self.capacity.div_ceil(2);
        let chunk = &mut self.chunks[index];
        let pieces = chunk.nodes.len().div_ceil(half);
        let mut rest = std::mem::take(&mut chunk.nodes);
        // Even piece sizes, the first `extra` pieces one atom longer.
        let (size, extra) = (rest.len() / pieces, rest.len() % pieces);
        let mut tails = Vec::with_capacity(pieces - 1);
        for piece in (1..pieces).rev() {
            let at = piece * size + piece.min(extra);
            tails.push(rest.split_off(at));
        }
        tails.reverse();
        // Do not keep the whole original allocation for the first piece.
        rest.shrink_to(self.capacity + 1);
        for node in &rest {
            self.homes.insert(node.id, chunk.key);
        }
        chunk.visible = rest.iter().filter(|node| !node.deleted).count();
        chunk.nodes = rest;
        let new_chunks: Vec<Chunk<T>> = tails
            .into_iter()
            .map(|nodes| {
                let key = self.slots.len() as u32;
                self.slots.push(0);
                for node in &nodes {
                    self.homes.insert(node.id, key);
                }
                Chunk {
                    key,
                    visible: nodes.iter().filter(|node| !node.deleted).count(),
                    nodes,
                }
            })
            .collect();
        self.chunks.splice(index + 1..index + 1, new_chunks);
        for (at, chunk) in self.chunks.iter().enumerate().skip(index) {
            self.slots[chunk.key as usize] = at as u32;
        }
        self.recount();
    }

    fn recount(&mut self) {
        self.lens = Fenwick::from_counts(self.chunks.iter().map(|chunk| chunk.nodes.len()));
        self.visibles = Fenwick::from_counts(self.chunks.iter().map(|chunk| chunk.visible));
    }

    /// Marks `id` deleted; returns whether it was visible. Deleting a
    /// tombstone again is a no-op.
    pub fn delete(&mut self, id: Id) -> Result<bool, RgaError> {
        let place = self.locate(id).ok_or(RgaError::MissingTarget(id))?;
        let chunk = &mut self.chunks[place.chunk];
        let node = &mut chunk.nodes[place.offset];
        let was_visible = !node.deleted;
        if was_visible {
            self.flat.take();
            node.deleted = true;
            chunk.visible -= 1;
            self.visible_len -= 1;
            self.visibles.sub(place.chunk, 1);
        }
        Ok(was_visible)
    }

    /// A sequence holding `nodes` in this order, or the first repeated id.
    fn from_nodes(nodes: Vec<Node<T>>, capacity: usize) -> Result<Self, Id> {
        let mut rga = Self::with_chunk_capacity(capacity);
        let half = rga.capacity.div_ceil(2);
        let mut nodes = nodes.into_iter().peekable();
        while nodes.peek().is_some() {
            let key = rga.chunks.len() as u32;
            let piece: Vec<Node<T>> = nodes.by_ref().take(half).collect();
            for node in &piece {
                if rga.homes.insert(node.id, key).is_some() {
                    return Err(node.id);
                }
                rga.max_lamport = rga.max_lamport.max(node.id.lamport);
            }
            let visible = piece.iter().filter(|node| !node.deleted).count();
            rga.len += piece.len();
            rga.visible_len += visible;
            rga.slots.push(key);
            rga.chunks.push(Chunk {
                key,
                nodes: piece,
                visible,
            });
        }
        rga.recount();
        Ok(rga)
    }
}

impl<T: Clone> Clone for Rga<T> {
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.clone(),
            slots: self.slots.clone(),
            homes: self.homes.clone(),
            lens: self.lens.clone(),
            visibles: self.visibles.clone(),
            len: self.len,
            visible_len: self.visible_len,
            max_lamport: self.max_lamport,
            capacity: self.capacity,
            flat: OnceLock::new(),
        }
    }
}

/// Two sequences are equal when they hold the same atoms in the same order,
/// however they are chunked.
impl<T: PartialEq> PartialEq for Rga<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for Rga<T> {}

/// Formats an iterator of atoms as a list.
struct NodeList<'a, T>(&'a Rga<T>);

impl<T: fmt::Debug> fmt::Debug for NodeList<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.iter()).finish()
    }
}

impl<T: Serialize> Serialize for NodeList<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter())
    }
}

impl<T: fmt::Debug> fmt::Debug for Rga<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rga")
            .field("nodes", &NodeList(self))
            .finish()
    }
}

/// The serialized form: the node list in document order, as `{"nodes": […]}`.
#[derive(Deserialize)]
#[serde(rename = "Rga")]
struct Repr<T> {
    nodes: Vec<Node<T>>,
}

impl<T: Serialize> Serialize for Rga<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Rga", 1)?;
        state.serialize_field("nodes", &NodeList(self))?;
        state.end()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Rga<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let Repr { nodes } = Repr::deserialize(deserializer)?;
        Rga::from_nodes(nodes, CHUNK_CAPACITY)
            .map_err(|id| D::Error::custom(format!("atom {id} appears twice")))
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

/// Per-chunk counts with `O(log n)` prefix sums and prefix search.
#[derive(Clone, Debug, Default)]
struct Fenwick {
    /// `tree[k - 1]` is the sum of the counts at 1-based positions
    /// `k - lowbit(k) + 1 ..= k`.
    tree: Vec<usize>,
}

impl Fenwick {
    fn from_counts(counts: impl IntoIterator<Item = usize>) -> Self {
        let mut tree: Vec<usize> = counts.into_iter().collect();
        let n = tree.len();
        for k in 1..=n {
            let up = k + (k & k.wrapping_neg());
            if up <= n {
                tree[up - 1] += tree[k - 1];
            }
        }
        Self { tree }
    }

    fn add(&mut self, index: usize, delta: usize) {
        let mut k = index + 1;
        while k <= self.tree.len() {
            self.tree[k - 1] += delta;
            k += k & k.wrapping_neg();
        }
    }

    fn sub(&mut self, index: usize, delta: usize) {
        let mut k = index + 1;
        while k <= self.tree.len() {
            self.tree[k - 1] -= delta;
            k += k & k.wrapping_neg();
        }
    }

    /// The sum of the counts before `index`.
    fn prefix(&self, index: usize) -> usize {
        let (mut k, mut sum) = (index, 0);
        while k > 0 {
            sum += self.tree[k - 1];
            k &= k - 1;
        }
        sum
    }

    /// The index whose count covers `target`: `prefix(i) <= target <
    /// prefix(i + 1)`. Requires `target` below the total.
    fn find(&self, mut target: usize) -> usize {
        let n = self.tree.len();
        let mut position = 0;
        let mut step = if n == 0 { 0 } else { 1 << n.ilog2() };
        while step > 0 {
            let next = position + step;
            if next <= n && self.tree[next - 1] <= target {
                position = next;
                target -= self.tree[next - 1];
            }
            step >>= 1;
        }
        position
    }
}
