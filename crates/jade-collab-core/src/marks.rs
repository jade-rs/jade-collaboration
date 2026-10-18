//! Marks: formatting as a CRDT over an RGA (Peritext; Litt, Lim, Kleppmann
//! and van Hardenberg, 2022).
//!
//! A [`Mark`] gives attribute `key` a value between two [`Boundary`]s — a
//! side of an atom of the story's [`Rga`], or an end of the story. Atoms are
//! never removed from an RGA (a deletion leaves a tombstone), so a mark's
//! anchors never disappear, and inserts never reorder the atoms that are
//! there. An atom is *covered* when it lies strictly between the two
//! boundaries in the story's order, tombstones included; for each key it
//! shows the value of the covering mark with the greatest [`Id`]
//! (last-writer-wins). A value of `None` covers the atom but says "back to
//! the base formatting" — what a clear writes.
//!
//! The state is just the *set* of marks: [`MarkSet::insert`] is idempotent
//! set insertion, and [`MarkSet::effective`] is a pure function of that set
//! and the RGA's order, which is itself a function of the set of inserts. So
//! replicas that received the same operations in any order agree
//! (Lean: `JadeCollab/Marks.lean`, `format_converges`). [`spec`] states the
//! definition literally, mirroring the Lean model; a property test checks the
//! sweep against it.
//!
//! How a mark *expands* is decided by its anchors, which [`anchors`] picks
//! from the attribute's [`Expand`] rule when the mark is made: it starts
//! right before the first atom (typing in front of it is not covered) and
//! ends right before the atom that followed the last one, or at the story's
//! end ([`Expand::End`]: bold, italic — typing at its end is covered), or
//! right after the last atom ([`Expand::None`]: a link).

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::id::Id;
use crate::rga::Rga;

/// Where a mark starts or ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "side", content = "atom", rename_all = "snake_case")]
pub enum Boundary {
    /// Before every atom of the story.
    Start,
    /// Right before this atom.
    Before(Id),
    /// Right after this atom.
    After(Id),
    /// After every atom of the story.
    End,
}

impl Boundary {
    /// The atom this boundary is anchored to, if any.
    pub fn atom(self) -> Option<Id> {
        match self {
            Boundary::Before(atom) | Boundary::After(atom) => Some(atom),
            Boundary::Start | Boundary::End => None,
        }
    }
}

/// Whether typing at a mark's end is covered by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expand {
    /// Neither edge grows (a link).
    None,
    /// Text typed right after the last atom is covered (bold, italic).
    End,
}

/// One mark operation: `key` gets `value` (`None`: the base formatting)
/// from `start` to `end`. `id` orders concurrent marks of one key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark<K, V> {
    pub id: Id,
    pub key: K,
    pub start: Boundary,
    pub end: Boundary,
    pub value: Option<V>,
}

impl<K, V> Mark<K, V> {
    pub fn new(id: Id, key: K, start: Boundary, end: Boundary, value: Option<V>) -> Self {
        Self {
            id,
            key,
            start,
            end,
            value,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkError {
    /// A mark with this id and key, but other content, is already here.
    Conflict(Id),
    /// The mark names an atom the story does not have.
    MissingAnchor(Id),
    /// The mark ends where it starts or before: it could never cover
    /// anything that is there.
    Empty,
}

impl fmt::Display for MarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarkError::Conflict(id) => write!(f, "mark {id} already exists with other content"),
            MarkError::MissingAnchor(id) => write!(f, "mark anchor {id} does not exist"),
            MarkError::Empty => write!(f, "the mark ends before it starts"),
        }
    }
}

impl std::error::Error for MarkError {}

/// The anchors of a mark over the atoms `first ..= last` of `rga` (in story
/// order, tombstones included) under `expand`; `None` if either atom is
/// missing or `last` comes before `first`. Lean: `markOver`.
pub fn anchors<T: Clone>(
    rga: &Rga<T>,
    first: Id,
    last: Id,
    expand: Expand,
) -> Option<(Boundary, Boundary)> {
    let (from, to) = (rga.position(first)?, rga.position(last)?);
    if from > to {
        return None;
    }
    let end = match expand {
        Expand::None => Boundary::After(last),
        Expand::End => rga
            .nodes_in(to + 1..(to + 2).min(rga.len()))
            .next()
            .map_or(Boundary::End, |next| Boundary::Before(next.id)),
    };
    Some((Boundary::Before(first), end))
}

/// Which atoms (as indices into the story order) a boundary pair covers:
/// those strictly between them. `None` when an anchor is missing.
fn covered<T: Clone>(rga: &Rga<T>, start: Boundary, end: Boundary) -> Option<Range<usize>> {
    let lo = match start {
        Boundary::Start => 0,
        Boundary::Before(atom) => rga.position(atom)?,
        Boundary::After(atom) => rga.position(atom)? + 1,
        Boundary::End => rga.len(),
    };
    let hi = match end {
        Boundary::Start => 0,
        Boundary::Before(atom) => rga.position(atom)?,
        Boundary::After(atom) => rga.position(atom)? + 1,
        Boundary::End => rga.len(),
    };
    Some(lo..hi.max(lo))
}

/// The atoms of one key a single mark wins: `range` indexes the story order
/// ([`Rga::nodes`], tombstones included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span<K, V> {
    pub key: K,
    pub range: Range<usize>,
    /// The winning mark.
    pub id: Id,
    pub value: Option<V>,
}

/// Where a mark's covered range opens (`true`) or closes.
type Event<'a, K, V> = (usize, bool, &'a Mark<K, V>);

/// A story's marks: a set, keyed by (id, key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkSet<K, V> {
    marks: BTreeMap<(Id, K), Mark<K, V>>,
}

impl<K, V> Default for MarkSet<K, V> {
    fn default() -> Self {
        Self {
            marks: BTreeMap::new(),
        }
    }
}

impl<K: Ord + Clone, V: Clone + PartialEq> MarkSet<K, V> {
    /// Adds `mark`; `Ok(false)` when this very mark is already here.
    pub fn insert(&mut self, mark: Mark<K, V>) -> Result<bool, MarkError> {
        match self.marks.get(&(mark.id, mark.key.clone())) {
            Some(existing) if *existing == mark => Ok(false),
            Some(_) => Err(MarkError::Conflict(mark.id)),
            None => {
                self.marks.insert((mark.id, mark.key.clone()), mark);
                Ok(true)
            }
        }
    }
}

impl<K: Ord + Clone, V: Clone> MarkSet<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.marks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// Every mark, by (id, key).
    pub fn iter(&self) -> impl Iterator<Item = &Mark<K, V>> + '_ {
        self.marks.values()
    }

    /// Whether `mark` is well formed on `rga`: its anchors exist and it
    /// starts before it ends. Both are stable — atoms are never removed and
    /// never reorder — so every replica that has the anchors decides alike.
    pub fn validate<T: Clone>(rga: &Rga<T>, mark: &Mark<K, V>) -> Result<(), MarkError> {
        for atom in [mark.start, mark.end]
            .into_iter()
            .filter_map(Boundary::atom)
        {
            if !rga.contains(atom) {
                return Err(MarkError::MissingAnchor(atom));
            }
        }
        // Boundaries in one order: Start, then (before, after) of each atom,
        // then End.
        let rank = |b: Boundary| match b {
            Boundary::Start => 0,
            Boundary::Before(atom) => 2 * rga.position(atom).expect("checked") + 1,
            Boundary::After(atom) => 2 * rga.position(atom).expect("checked") + 2,
            Boundary::End => 2 * rga.len() + 1,
        };
        if rank(mark.start) >= rank(mark.end) {
            return Err(MarkError::Empty);
        }
        Ok(())
    }

    /// What `atom` shows for `key`: the winning mark's id and value, or
    /// `None` when no mark of `key` covers it (or `atom` is not in `rga`).
    pub fn at<T: Clone>(&self, rga: &Rga<T>, atom: Id, key: &K) -> Option<(Id, Option<&V>)> {
        let index = rga.position(atom)?;
        self.marks
            .values()
            .filter(|m| m.key == *key)
            .filter(|m| covered(rga, m.start, m.end).is_some_and(|r| r.contains(&index)))
            .max_by_key(|m| m.id)
            .map(|m| (m.id, m.value.as_ref()))
    }

    /// The effective formatting of the whole story: for each key (in order),
    /// maximal runs of atoms won by one mark, left to right. Atoms no mark
    /// of a key covers are in no span of that key. `O((n + m) log m)` for
    /// `m` marks.
    pub fn effective<T: Clone>(&self, rga: &Rga<T>) -> Vec<Span<K, V>> {
        // Per key: (position, opens?, mark) events.
        let mut events: BTreeMap<&K, Vec<Event<'_, K, V>>> = BTreeMap::new();
        for mark in self.marks.values() {
            let Some(range) = covered(rga, mark.start, mark.end) else {
                continue;
            };
            if range.is_empty() {
                continue;
            }
            let list = events.entry(&mark.key).or_default();
            list.push((range.start, true, mark));
            list.push((range.end, false, mark));
        }
        let mut spans: Vec<Span<K, V>> = Vec::new();
        for (key, mut list) in events {
            list.sort_by_key(|(at, opens, mark)| (*at, *opens, mark.id));
            let mut active: BTreeMap<Id, &Mark<K, V>> = BTreeMap::new();
            let mut at = 0;
            let mut k = 0;
            while k < list.len() {
                let here = list[k].0;
                if here > at {
                    if let Some((&id, mark)) = active.last_key_value() {
                        match spans.last_mut() {
                            Some(last)
                                if last.key == *key && last.id == id && last.range.end == at =>
                            {
                                last.range.end = here;
                            }
                            _ => spans.push(Span {
                                key: key.clone(),
                                range: at..here,
                                id,
                                value: mark.value.clone(),
                            }),
                        }
                    }
                    at = here;
                }
                while k < list.len() && list[k].0 == here {
                    let (_, opens, mark) = list[k];
                    if opens {
                        active.insert(mark.id, mark);
                    } else {
                        active.remove(&mark.id);
                    }
                    k += 1;
                }
            }
        }
        spans
    }
}

impl<K: Serialize, V: Serialize> Serialize for MarkSet<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.marks.values())
    }
}

impl<'de, K, V> Deserialize<'de> for MarkSet<K, V>
where
    K: Deserialize<'de> + Ord + Clone,
    V: Deserialize<'de> + Clone + PartialEq,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut set = MarkSet::new();
        for mark in Vec::<Mark<K, V>>::deserialize(deserializer)? {
            set.insert(mark).map_err(D::Error::custom)?;
        }
        Ok(set)
    }
}

/// The definition, literally: an atom is covered when it lies strictly
/// between the boundaries in the story order; the newest covering mark of
/// the key wins. Mirrors `lean/JadeCollab/Marks.lean` (`covers`,
/// `effective`).
pub mod spec {
    use super::*;

    fn index(order: &[Id], atom: Id) -> Option<usize> {
        order.iter().position(|a| *a == atom)
    }

    /// Whether `mark` covers `atom` in `order`.
    pub fn covers<K, V>(order: &[Id], mark: &Mark<K, V>, atom: Id) -> bool {
        let Some(x) = index(order, atom) else {
            return false;
        };
        let starts_before = match mark.start {
            Boundary::Start => true,
            Boundary::Before(a) => index(order, a).is_some_and(|a| a <= x),
            Boundary::After(a) => index(order, a).is_some_and(|a| a < x),
            Boundary::End => false,
        };
        let ends_after = match mark.end {
            Boundary::Start => false,
            Boundary::Before(b) => index(order, b).is_some_and(|b| x < b),
            Boundary::After(b) => index(order, b).is_some_and(|b| x <= b),
            Boundary::End => true,
        };
        starts_before && ends_after
    }

    /// What `atom` shows for `key` given the marks received (in any order,
    /// with any duplication).
    pub fn effective<K: PartialEq, V: Clone>(
        order: &[Id],
        marks: &[Mark<K, V>],
        atom: Id,
        key: &K,
    ) -> Option<(Id, Option<V>)> {
        marks
            .iter()
            .filter(|m| m.key == *key && covers(order, m, atom))
            .max_by_key(|m| m.id)
            .map(|m| (m.id, m.value.clone()))
    }
}
