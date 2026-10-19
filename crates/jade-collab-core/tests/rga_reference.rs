//! The indexed [`Rga`] against a naive reference: the original flat-array
//! implementation with linear scans, kept here verbatim in behaviour. Every
//! public method — results, errors and positions — must agree on arbitrary
//! command sequences, including malformed ones, at every chunk capacity.

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::rga::{Node, Rga, RgaError};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};

fn id(lamport: u64, replica: u64) -> Id {
    Id::new(lamport, ReplicaId(replica))
}

/// The reference: one `Vec` in document order, every query a linear scan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Naive<T> {
    nodes: Vec<Node<T>>,
}

impl<T> Naive<T> {
    fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    fn ids(&self) -> Vec<Id> {
        self.nodes.iter().map(|n| n.id).collect()
    }

    fn position(&self, id: Id) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    fn get(&self, id: Id) -> Option<&Node<T>> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn contains(&self, id: Id) -> bool {
        self.position(id).is_some()
    }

    fn is_visible(&self, id: Id) -> bool {
        self.get(id).is_some_and(|n| !n.deleted)
    }

    fn max_lamport(&self) -> u64 {
        self.nodes.iter().map(|n| n.id.lamport).max().unwrap_or(0)
    }

    fn visible(&self) -> Vec<(Id, &T)> {
        self.nodes
            .iter()
            .filter(|n| !n.deleted)
            .map(|n| (n.id, &n.value))
            .collect()
    }

    fn visible_len(&self) -> usize {
        self.nodes.iter().filter(|n| !n.deleted).count()
    }

    fn visible_id_at(&self, index: usize) -> Option<Id> {
        self.visible().get(index).map(|(id, _)| *id)
    }

    fn visible_index(&self, id: Id) -> Option<usize> {
        let mut index = 0;
        for n in &self.nodes {
            if n.id == id {
                return (!n.deleted).then_some(index);
            }
            if !n.deleted {
                index += 1;
            }
        }
        None
    }

    fn visible_before(&self, id: Id) -> Option<Id> {
        let position = self.position(id)?;
        self.nodes[..position]
            .iter()
            .rev()
            .find(|n| !n.deleted)
            .map(|n| n.id)
    }

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
        let skipped = self.nodes[start..].iter().take_while(|n| n.id > id).count();
        Ok(start + skipped)
    }

    fn insert(&mut self, id: Id, parent: Option<Id>, value: T) -> Result<usize, RgaError> {
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

    fn insert_run(
        &mut self,
        first: Id,
        parent: Option<Id>,
        values: Vec<T>,
    ) -> Result<usize, RgaError> {
        if values.is_empty() {
            return match parent {
                None => Ok(0),
                Some(p) => self
                    .position(p)
                    .map(|i| i + 1)
                    .ok_or(RgaError::MissingParent(p)),
            };
        }
        let at = self.integration_point(first, parent)?;
        let count = values.len() as u64;
        if let Some(existing) = self
            .nodes
            .iter()
            .find(|n| n.id.replica == first.replica && n.id > first && n.id < first.offset(count))
        {
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

    fn delete(&mut self, id: Id) -> Result<bool, RgaError> {
        let n = self
            .nodes
            .iter_mut()
            .find(|n| n.id == id)
            .ok_or(RgaError::MissingTarget(id))?;
        let was_visible = !n.deleted;
        n.deleted = true;
        Ok(was_visible)
    }
}

// ---------------------------------------------------------------------------
// Random commands, interpreted against the reference's current state so that
// most are well formed but every error path is exercised too.

#[derive(Clone, Debug)]
enum Cmd {
    Insert {
        parent: u16,
        lamport: u8,
        replica: u64,
        value: char,
    },
    Run {
        parent: u16,
        lamport: u8,
        replica: u64,
        len: usize,
    },
    Delete {
        target: u16,
    },
}

fn cmd(max_run: usize) -> impl Strategy<Value = Cmd> {
    prop_oneof![
        6 => (any::<u16>(), any::<u8>(), 1u64..4, prop::char::range('a', 'z'))
            .prop_map(|(parent, lamport, replica, value)| Cmd::Insert { parent, lamport, replica, value }),
        2 => (any::<u16>(), any::<u8>(), 1u64..4, 0..max_run)
            .prop_map(|(parent, lamport, replica, len)| Cmd::Run { parent, lamport, replica, len }),
        3 => any::<u16>().prop_map(|target| Cmd::Delete { target }),
    ]
}

/// An id that is never inserted.
fn absent(k: u16) -> Id {
    id(1_000_000 + k as u64, 99)
}

/// `None` (the start), an existing atom (tombstones included), or an atom that
/// does not exist.
fn pick_parent(naive: &Naive<char>, k: u16) -> Option<Id> {
    let ids = naive.ids();
    match k % 16 {
        0 => None,
        1 => Some(absent(k)),
        _ if ids.is_empty() => None,
        // Mostly near the end, where a run keeps its typing locality.
        2..=5 => Some(ids[ids.len() - 1 - (k as usize / 16) % ids.len().min(4)]),
        _ => Some(ids[k as usize % ids.len()]),
    }
}

/// Usually a fresh, newest id; sometimes a duplicate or an id that is not
/// newer than its parent.
fn pick_lamport(naive: &Naive<char>, k: u8) -> u64 {
    let max = naive.max_lamport();
    match k % 8 {
        0 => k as u64 % (max + 1),
        1 => max.saturating_sub(k as u64 % 4),
        _ => max + 1 + (k as u64 % 3),
    }
}

fn values(len: usize, seed: u8) -> Vec<char> {
    (0..len)
        .map(|k| (b'a' + ((k + seed as usize) % 26) as u8) as char)
        .collect()
}

/// Applies `cmd` to both and checks the results agree.
fn step(rga: &mut Rga<char>, naive: &mut Naive<char>, cmd: &Cmd) -> Result<(), TestCaseError> {
    match *cmd {
        Cmd::Insert {
            parent,
            lamport,
            replica,
            value,
        } => {
            let parent = pick_parent(naive, parent);
            let id = id(pick_lamport(naive, lamport), replica);
            prop_assert_eq!(
                rga.insert(id, parent, value),
                naive.insert(id, parent, value),
                "insert {} after {:?}",
                id,
                parent
            );
        }
        Cmd::Run {
            parent,
            lamport,
            replica,
            len,
        } => {
            let parent = pick_parent(naive, parent);
            let first = id(pick_lamport(naive, lamport), replica);
            prop_assert_eq!(
                rga.insert_run(first, parent, values(len, lamport)),
                naive.insert_run(first, parent, values(len, lamport)),
                "insert_run {} x{} after {:?}",
                first,
                len,
                parent
            );
        }
        Cmd::Delete { target } => {
            let ids = naive.ids();
            let target = if ids.is_empty() || target % 8 == 0 {
                absent(target)
            } else {
                ids[target as usize % ids.len()]
            };
            prop_assert_eq!(
                rga.delete(target),
                naive.delete(target),
                "delete {}",
                target
            );
        }
    }
    Ok(())
}

/// A handful of queries, cheap enough to run after every command.
fn spot_check(rga: &Rga<char>, naive: &Naive<char>, seed: usize) -> Result<(), TestCaseError> {
    prop_assert_eq!(rga.len(), naive.nodes.len());
    prop_assert_eq!(rga.is_empty(), naive.nodes.is_empty());
    prop_assert_eq!(rga.visible_len(), naive.visible_len());
    prop_assert_eq!(rga.max_lamport(), naive.max_lamport());
    // Interleaving `nodes()` with edits checks that every edit drops the
    // materialized slice.
    if seed.is_multiple_of(4) {
        prop_assert_eq!(rga.nodes(), naive.nodes.as_slice());
    }
    if let Some(node) = naive.nodes.get(seed % (naive.nodes.len() + 1)) {
        let id = node.id;
        prop_assert_eq!(rga.position(id), naive.position(id));
        prop_assert_eq!(rga.visible_index(id), naive.visible_index(id));
        prop_assert_eq!(rga.visible_before(id), naive.visible_before(id));
        prop_assert_eq!(rga.get(id), naive.get(id));
    }
    let index = seed % (naive.visible_len() + 2);
    prop_assert_eq!(rga.visible_id_at(index), naive.visible_id_at(index));
    Ok(())
}

/// Every query, for every atom and every index.
fn full_check(rga: &Rga<char>, naive: &Naive<char>) -> Result<(), TestCaseError> {
    prop_assert_eq!(rga.nodes(), naive.nodes.as_slice());
    prop_assert_eq!(rga.iter().cloned().collect::<Vec<_>>(), naive.nodes.clone());
    prop_assert_eq!(rga.ids(), naive.ids());
    prop_assert_eq!(rga.len(), naive.nodes.len());
    prop_assert_eq!(rga.is_empty(), naive.nodes.is_empty());
    prop_assert_eq!(rga.max_lamport(), naive.max_lamport());
    prop_assert_eq!(rga.visible().collect::<Vec<_>>(), naive.visible());
    prop_assert_eq!(rga.visible_len(), naive.visible_len());
    let probes = naive
        .ids()
        .into_iter()
        .chain([absent(0), absent(7), id(0, 0)]);
    for id in probes {
        prop_assert_eq!(rga.position(id), naive.position(id), "position {}", id);
        prop_assert_eq!(rga.get(id), naive.get(id), "get {}", id);
        prop_assert_eq!(rga.contains(id), naive.contains(id), "contains {}", id);
        prop_assert_eq!(
            rga.is_visible(id),
            naive.is_visible(id),
            "is_visible {}",
            id
        );
        prop_assert_eq!(
            rga.visible_index(id),
            naive.visible_index(id),
            "visible_index {}",
            id
        );
        prop_assert_eq!(
            rga.visible_before(id),
            naive.visible_before(id),
            "visible_before {}",
            id
        );
    }
    for index in 0..naive.visible_len() + 2 {
        prop_assert_eq!(
            rga.visible_id_at(index),
            naive.visible_id_at(index),
            "visible_id_at {}",
            index
        );
    }
    Ok(())
}

/// Every range of `nodes_in` matches slicing the reference.
fn check_ranges(rga: &Rga<char>, naive: &Naive<char>, seed: usize) -> Result<(), TestCaseError> {
    let n = naive.nodes.len();
    let a = seed % (n + 1);
    let b = (seed / 7) % (n + 1);
    let (start, end) = (a.min(b), a.max(b));
    let collect = |it: Box<dyn Iterator<Item = &Node<char>> + '_>| it.cloned().collect::<Vec<_>>();
    prop_assert_eq!(
        collect(Box::new(rga.nodes_in(start..end))),
        naive.nodes[start..end].to_vec()
    );
    prop_assert_eq!(
        collect(Box::new(rga.nodes_in(start..))),
        naive.nodes[start..].to_vec()
    );
    prop_assert_eq!(
        collect(Box::new(rga.nodes_in(..end))),
        naive.nodes[..end].to_vec()
    );
    prop_assert_eq!(collect(Box::new(rga.nodes_in(..))), naive.nodes.clone());
    // Backwards from a position: what slicing up to it holds, reversed.
    let mut reversed = naive.nodes[..end].to_vec();
    reversed.reverse();
    prop_assert_eq!(collect(Box::new(rga.nodes_before_rev(end))), reversed);
    if end < n {
        prop_assert_eq!(
            collect(Box::new(rga.nodes_in(start..=end))),
            naive.nodes[start..=end].to_vec()
        );
    }
    Ok(())
}

fn capacity() -> impl Strategy<Value = Option<usize>> {
    prop_oneof![
        Just(None),
        (1usize..9).prop_map(Some),
        Just(Some(16)),
        Just(Some(64)),
    ]
}

fn fresh(capacity: Option<usize>) -> Rga<char> {
    match capacity {
        None => Rga::new(),
        Some(capacity) => Rga::with_chunk_capacity(capacity),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Small chunks force splits on almost every insert; the default
    /// capacity covers the production layout.
    #[test]
    fn every_query_agrees_with_the_naive_reference(
        capacity in capacity(),
        cmds in prop::collection::vec((cmd(12), any::<usize>()), 0..120),
        seed in any::<usize>(),
    ) {
        let mut rga = fresh(capacity);
        let mut naive = Naive::new();
        for (cmd, probe) in &cmds {
            step(&mut rga, &mut naive, cmd)?;
            spot_check(&rga, &naive, *probe)?;
        }
        full_check(&rga, &naive)?;
        check_ranges(&rga, &naive, seed)?;
    }
}

proptest! {
    // Each case builds a document of thousands of atoms and checks every
    // query against a linear-scan reference, so fewer cases.
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Long runs spread over many default-sized chunks, then edits on top.
    #[test]
    fn large_documents_agree_with_the_naive_reference(
        capacity in prop_oneof![Just(None), Just(Some(32))],
        cmds in prop::collection::vec((cmd(400), any::<usize>()), 0..200),
        seed in any::<usize>(),
    ) {
        let mut rga = fresh(capacity);
        let mut naive = Naive::new();
        for (cmd, probe) in &cmds {
            step(&mut rga, &mut naive, cmd)?;
            spot_check(&rga, &naive, *probe)?;
        }
        full_check(&rga, &naive)?;
        check_ranges(&rga, &naive, seed)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The JSON shape is the reference's plain node list, both ways, and a
    /// deserialized sequence keeps working like the reference.
    #[test]
    fn serde_round_trips_through_the_plain_node_list(
        capacity in capacity(),
        cmds in prop::collection::vec(cmd(40), 0..80),
        more in prop::collection::vec(cmd(40), 0..40),
    ) {
        let mut rga = fresh(capacity);
        let mut naive = Naive::new();
        for cmd in &cmds {
            step(&mut rga, &mut naive, cmd)?;
        }
        let json = serde_json::to_value(&rga).unwrap();
        prop_assert_eq!(&json, &serde_json::to_value(&naive).unwrap());
        let mut back: Rga<char> = serde_json::from_value(json).unwrap();
        prop_assert_eq!(&back, &rga);
        for cmd in &more {
            step(&mut back, &mut naive, cmd)?;
        }
        full_check(&back, &naive)?;
    }

    /// Equality is about the sequence, not how it is chunked.
    #[test]
    fn equality_ignores_the_chunk_layout(
        a in capacity(),
        b in capacity(),
        cmds in prop::collection::vec(cmd(40), 0..80),
    ) {
        let (mut x, mut y, mut naive) = (fresh(a), fresh(b), Naive::new());
        for cmd in &cmds {
            let mut scratch = naive.clone();
            step(&mut x, &mut naive, cmd)?;
            step(&mut y, &mut scratch, cmd)?;
        }
        prop_assert_eq!(&x, &y);
        prop_assert_eq!(&x.clone(), &y);
        // …and different sequences are different.
        if let Some(last) = cmds.last() {
            let (mut z, mut shorter) = (fresh(b), Naive::new());
            for cmd in &cmds[..cmds.len() - 1] {
                step(&mut z, &mut shorter, cmd)?;
            }
            prop_assert_eq!(x == z, naive == shorter, "after dropping {:?}", last);
        }
    }
}

#[test]
fn deserializing_rejects_a_repeated_id() {
    let json = serde_json::json!({
        "nodes": [
            {"id": {"lamport": 1, "replica": 1}, "parent": null, "value": 'a', "deleted": false},
            {"id": {"lamport": 1, "replica": 1}, "parent": null, "value": 'b', "deleted": false},
        ]
    });
    let error = serde_json::from_value::<Rga<char>>(json).unwrap_err();
    assert!(error.to_string().contains("1@r1"), "{error}");
}

#[test]
fn nodes_and_iter_walk_a_multi_chunk_story_in_order() {
    let mut rga = Rga::with_chunk_capacity(4);
    rga.insert_run(id(1, 0), None, "hello world".chars())
        .unwrap();
    rga.insert(id(20, 2), Some(id(5, 0)), ',').unwrap();
    rga.delete(id(6, 0)).unwrap();
    let text: String = rga.iter().filter(|n| !n.deleted).map(|n| n.value).collect();
    assert_eq!(text, "hello,world");
    assert_eq!(rga.nodes().len(), 12);
    assert_eq!(rga.nodes()[5].id, id(20, 2));
    assert_eq!(
        rga.nodes_in(4..=6).map(|n| n.value).collect::<String>(),
        "o, "
    );
}

#[test]
#[should_panic]
fn nodes_in_panics_past_the_end_like_a_slice() {
    let mut rga = Rga::with_chunk_capacity(2);
    rga.insert_run(id(1, 0), None, "abc".chars()).unwrap();
    let _ = rga.nodes_in(2..5).count();
}
