//! The mark CRDT: formatting anchored to the sides of RGA atoms, resolved
//! per attribute by the newest covering mark. Lean: `JadeCollab/Marks.lean`.

use std::collections::{BTreeMap, BTreeSet};

use jade_collab_core::id::{Clock, Id, ReplicaId};
use jade_collab_core::marks::{anchors, spec, Boundary, Expand, Mark, MarkError, MarkSet};
use jade_collab_core::rga::{spec as rga_spec, Rga};
use proptest::prelude::*;

fn id(lamport: u64, replica: u64) -> Id {
    Id::new(lamport, ReplicaId(replica))
}

/// "abcde" typed by replica 1: ids 1@r1 … 5@r1.
fn abcde() -> Rga<char> {
    let mut rga = Rga::new();
    rga.insert_run(id(1, 1), None, "abcde".chars()).unwrap();
    rga
}

/// Each atom's (id, value) for `key`, in story order.
fn shown<V: Clone + PartialEq + std::fmt::Debug>(
    rga: &Rga<char>,
    marks: &MarkSet<u8, V>,
    key: u8,
) -> Vec<(char, Option<Option<V>>)> {
    let table = per_atom(rga, marks);
    rga.iter()
        .filter(|n| !n.deleted)
        .map(|n| {
            (
                n.value,
                table.get(&(n.id, key)).map(|(_, value)| value.clone()),
            )
        })
        .collect()
}

/// The winning (mark id, value) of every covered (atom, key), from
/// [`MarkSet::effective`].
fn per_atom<T, V: Clone>(rga: &Rga<T>, marks: &MarkSet<u8, V>) -> BTreeMap<(Id, u8), (Id, Option<V>)>
where
    T: Clone,
{
    let order = rga.ids();
    let mut out = BTreeMap::new();
    for span in marks.effective(rga) {
        for index in span.range.clone() {
            let previous = out.insert((order[index], span.key), (span.id, span.value.clone()));
            assert!(previous.is_none(), "spans of one key overlap at {index}");
        }
    }
    out
}

fn mark(lamport: u64, key: u8, start: Boundary, end: Boundary, value: Option<u8>) -> Mark<u8, u8> {
    Mark::new(id(lamport, 2), key, start, end, value)
}

#[test]
fn bold_expands_at_its_end_and_a_link_does_not() {
    let mut rga = abcde();
    let (b, c) = (id(2, 1), id(3, 1));
    let mut marks = MarkSet::new();
    let (start, end) = anchors(&rga, b, c, Expand::End).unwrap();
    assert_eq!((start, end), (Boundary::Before(b), Boundary::Before(id(4, 1))));
    marks.insert(Mark::new(id(10, 2), 0u8, start, end, Some(1u8))).unwrap();
    let (start, end) = anchors(&rga, b, c, Expand::None).unwrap();
    assert_eq!((start, end), (Boundary::Before(b), Boundary::After(c)));
    marks.insert(Mark::new(id(11, 2), 1u8, start, end, Some(1u8))).unwrap();
    // "x" typed right after "c", "y" right before "b".
    rga.insert(id(20, 3), Some(c), 'x').unwrap();
    rga.insert(id(21, 3), Some(id(1, 1)), 'y').unwrap();
    let bold: String = shown(&rga, &marks, 0)
        .iter()
        .map(|(ch, v)| if v.is_some() { ch.to_ascii_uppercase() } else { *ch })
        .collect();
    let link: String = shown(&rga, &marks, 1)
        .iter()
        .map(|(ch, v)| if v.is_some() { ch.to_ascii_uppercase() } else { *ch })
        .collect();
    assert_eq!(bold, "ayBCXde");
    assert_eq!(link, "ayBCxde");
}

#[test]
fn a_mark_ending_at_the_story_end_expands_to_text_appended_later() {
    let mut rga = abcde();
    let (start, end) = anchors(&rga, id(4, 1), id(5, 1), Expand::End).unwrap();
    assert_eq!(end, Boundary::End);
    let mut marks = MarkSet::new();
    marks.insert(mark(10, 0, start, end, Some(1))).unwrap();
    rga.insert(id(20, 3), Some(id(5, 1)), 'f').unwrap();
    let covered: Vec<bool> = shown(&rga, &marks, 0).iter().map(|(_, v)| v.is_some()).collect();
    assert_eq!(covered, [false, false, false, true, true, true]);
}

#[test]
fn the_newest_mark_wins_per_key_and_keys_are_independent() {
    let rga = abcde();
    let mut marks = MarkSet::new();
    let all = (Boundary::Before(id(1, 1)), Boundary::End);
    marks.insert(mark(10, 0, all.0, all.1, Some(1))).unwrap();
    // An older mark arriving later does not win.
    marks
        .insert(mark(9, 0, Boundary::Before(id(2, 1)), Boundary::After(id(2, 1)), Some(7)))
        .unwrap();
    marks
        .insert(mark(11, 0, Boundary::Before(id(3, 1)), Boundary::After(id(3, 1)), Some(2)))
        .unwrap();
    marks.insert(mark(5, 1, all.0, all.1, Some(3))).unwrap();
    let bold: Vec<Option<u8>> = shown(&rga, &marks, 0).iter().map(|(_, v)| v.flatten()).collect();
    assert_eq!(bold, [Some(1), Some(1), Some(2), Some(1), Some(1)]);
    let other: Vec<Option<u8>> = shown(&rga, &marks, 1).iter().map(|(_, v)| v.flatten()).collect();
    assert_eq!(other, [Some(3); 5]);
}

#[test]
fn an_unset_mark_returns_its_atoms_to_the_base_formatting() {
    let rga = abcde();
    let mut marks = MarkSet::new();
    marks
        .insert(mark(10, 0, Boundary::Before(id(1, 1)), Boundary::End, Some(1)))
        .unwrap();
    marks
        .insert(mark(11, 0, Boundary::Before(id(2, 1)), Boundary::Before(id(4, 1)), None))
        .unwrap();
    let bold: Vec<Option<Option<u8>>> = shown(&rga, &marks, 0).into_iter().map(|(_, v)| v).collect();
    assert_eq!(
        bold,
        [Some(Some(1)), Some(None), Some(None), Some(Some(1)), Some(Some(1))]
    );
    assert_eq!(marks.at(&rga, id(2, 1), &0), Some((id(11, 2), None)));
    assert_eq!(marks.at(&rga, id(1, 1), &0), Some((id(10, 2), Some(&1))));
    assert_eq!(marks.at(&rga, id(1, 1), &1), None);
}

#[test]
fn deleted_text_keeps_anchoring_its_marks() {
    let mut rga = abcde();
    let mut marks = MarkSet::new();
    let (start, end) = anchors(&rga, id(2, 1), id(4, 1), Expand::End).unwrap();
    marks.insert(mark(10, 0, start, end, Some(1))).unwrap();
    // The whole marked range and the atom after it go.
    for n in 2..=5 {
        rga.delete(id(n, 1)).unwrap();
    }
    // Typing after the visible "a" lands before the tombstones: not covered.
    rga.insert(id(20, 3), Some(id(1, 1)), 'x').unwrap();
    // Typing after the tombstone of "d" (a replica that had not seen the
    // deletion) is still inside the mark.
    rga.insert(id(21, 4), Some(id(4, 1)), 'y').unwrap();
    assert_eq!(
        shown(&rga, &marks, 0),
        [('a', None), ('x', None), ('y', Some(Some(1)))]
    );
}

#[test]
fn marks_whose_anchors_are_missing_cover_nothing_and_are_refused_by_validation() {
    let rga = abcde();
    let mut marks = MarkSet::new();
    let ghost = mark(10, 0, Boundary::Before(id(99, 9)), Boundary::End, Some(1));
    assert_eq!(
        MarkSet::validate(&rga, &ghost),
        Err(MarkError::MissingAnchor(id(99, 9)))
    );
    marks.insert(ghost).unwrap();
    assert!(marks.effective(&rga).is_empty());
    let inverted = mark(11, 0, Boundary::After(id(3, 1)), Boundary::Before(id(2, 1)), Some(1));
    assert_eq!(MarkSet::validate(&rga, &inverted), Err(MarkError::Empty));
    let empty = mark(12, 0, Boundary::Before(id(3, 1)), Boundary::Before(id(3, 1)), Some(1));
    assert_eq!(MarkSet::validate(&rga, &empty), Err(MarkError::Empty));
    let ok = mark(13, 0, Boundary::Start, Boundary::After(id(1, 1)), Some(1));
    assert_eq!(MarkSet::validate(&rga, &ok), Ok(()));
}

#[test]
fn delivering_a_mark_twice_is_a_no_op_and_a_conflicting_copy_is_refused() {
    let mut marks: MarkSet<u8, u8> = MarkSet::new();
    let m = mark(10, 0, Boundary::Start, Boundary::End, Some(1));
    assert_eq!(marks.insert(m.clone()), Ok(true));
    assert_eq!(marks.insert(m.clone()), Ok(false));
    assert_eq!(marks.len(), 1);
    let mut other = m.clone();
    other.value = Some(2);
    assert_eq!(marks.insert(other), Err(MarkError::Conflict(id(10, 2))));
    // The same id may carry one mark per key (a clear unsets several).
    let mut sibling = m;
    sibling.key = 1;
    assert_eq!(marks.insert(sibling), Ok(true));
    assert_eq!(marks.len(), 2);
}

#[test]
fn a_mark_set_serializes_as_its_list_of_marks() {
    let mut marks: MarkSet<u8, String> = MarkSet::new();
    marks
        .insert(Mark::new(
            id(10, 2),
            3,
            Boundary::Before(id(1, 1)),
            Boundary::End,
            Some("x".to_string()),
        ))
        .unwrap();
    let json = serde_json::to_value(&marks).unwrap();
    assert!(json.is_array(), "{json}");
    let back: MarkSet<u8, String> = serde_json::from_value(json).unwrap();
    assert_eq!(back, marks);
}

// ---------------------------------------------------------------------------
// Concurrent histories.

#[derive(Clone, Debug)]
enum Op {
    Insert {
        id: Id,
        parent: Option<Id>,
        value: char,
    },
    Delete {
        id: Id,
    },
    Mark(Mark<u8, u8>),
}

#[derive(Clone, Debug)]
enum Action {
    Insert { peer: usize, at: usize, value: char },
    Delete { peer: usize, at: usize },
    Mark {
        peer: usize,
        from: usize,
        len: usize,
        key: u8,
        value: Option<u8>,
        expand: bool,
    },
    Sync { from: usize, to: usize },
}

fn action(peers: usize) -> impl Strategy<Value = Action> {
    prop_oneof![
        5 => (0..peers, any::<usize>(), prop::char::range('a', 'z'))
            .prop_map(|(peer, at, value)| Action::Insert { peer, at, value }),
        2 => (0..peers, any::<usize>()).prop_map(|(peer, at)| Action::Delete { peer, at }),
        3 => (0..peers, any::<usize>(), 0..6usize, 0..3u8, prop::option::of(0..3u8), any::<bool>())
            .prop_map(|(peer, from, len, key, value, expand)| Action::Mark { peer, from, len, key, value, expand }),
        3 => (0..peers, 0..peers).prop_map(|(from, to)| Action::Sync { from, to }),
    ]
}

struct Peer {
    rga: Rga<char>,
    marks: MarkSet<u8, u8>,
    clock: Clock,
    seen: BTreeSet<usize>,
}

impl Peer {
    fn new(replica: u64) -> Self {
        Self {
            rga: Rga::new(),
            marks: MarkSet::new(),
            clock: Clock::new(ReplicaId(replica)),
            seen: BTreeSet::new(),
        }
    }

    /// Whether every atom `op` names is here.
    fn ready(&self, op: &Op) -> bool {
        match op {
            Op::Insert { parent, .. } => parent.is_none_or(|p| self.rga.contains(p)),
            Op::Delete { id } => self.rga.contains(*id),
            Op::Mark(mark) => [mark.start, mark.end]
                .iter()
                .all(|b| b.atom().is_none_or(|a| self.rga.contains(a))),
        }
    }

    fn apply(&mut self, op: &Op) {
        match op {
            Op::Insert { id, parent, value } => {
                self.clock.observe(*id);
                self.rga.insert(*id, *parent, *value).unwrap();
            }
            Op::Delete { id } => {
                self.rga.delete(*id).unwrap();
            }
            Op::Mark(mark) => {
                self.clock.observe(mark.id);
                MarkSet::validate(&self.rga, mark).unwrap();
                self.marks.insert(mark.clone()).unwrap();
            }
        }
    }

    fn deliver(&mut self, index: usize, op: &Op) {
        if self.seen.insert(index) {
            assert!(self.ready(op), "causal delivery broke for {op:?}");
            self.apply(op);
        }
    }
}

/// Runs `actions` over `peers` replicas; returns the log and the replicas
/// after a final full sync.
fn run(peers: usize, actions: &[Action]) -> (Vec<Op>, Vec<Peer>) {
    let mut log: Vec<Op> = Vec::new();
    let mut replicas: Vec<Peer> = (0..peers).map(|p| Peer::new(p as u64 + 1)).collect();
    for action in actions {
        let op = match *action {
            Action::Insert { peer, at, value } => {
                let replica = &mut replicas[peer];
                let len = replica.rga.len();
                let parent = (at % (len + 1) != len).then(|| replica.rga.nodes()[at % (len + 1)].id);
                let id = replica.clock.tick();
                (peer, Op::Insert { id, parent, value })
            }
            Action::Delete { peer, at } => {
                let replica = &replicas[peer];
                let len = replica.rga.visible_len();
                if len == 0 {
                    continue;
                }
                let id = replica.rga.visible_id_at(at % len).unwrap();
                (peer, Op::Delete { id })
            }
            Action::Mark { peer, from, len, key, value, expand } => {
                let replica = &mut replicas[peer];
                let visible = replica.rga.visible_len();
                if visible == 0 {
                    continue;
                }
                let from = from % visible;
                let to = (from + len).min(visible - 1);
                let first = replica.rga.visible_id_at(from).unwrap();
                let last = replica.rga.visible_id_at(to).unwrap();
                let expand = if expand { Expand::End } else { Expand::None };
                let (start, end) = anchors(&replica.rga, first, last, expand).unwrap();
                let id = replica.clock.tick();
                (peer, Op::Mark(Mark::new(id, key, start, end, value)))
            }
            Action::Sync { from, to } => {
                if from != to {
                    let indices: Vec<usize> = replicas[from].seen.iter().copied().collect();
                    for index in indices {
                        replicas[to].deliver(index, &log[index]);
                    }
                }
                continue;
            }
        };
        let (peer, op) = op;
        log.push(op.clone());
        replicas[peer].deliver(log.len() - 1, &op);
    }
    for replica in &mut replicas {
        for (index, op) in log.iter().enumerate() {
            replica.deliver(index, op);
        }
    }
    (log, replicas)
}

/// The specification's formatting of every (atom, key) the log ever made.
fn spec_of(log: &[Op]) -> BTreeMap<(Id, u8), (Id, Option<u8>)> {
    let inserts: Vec<rga_spec::Insert<char>> = log
        .iter()
        .filter_map(|op| match op {
            Op::Insert { id, parent, value } => Some(rga_spec::Insert::new(*id, *parent, *value)),
            _ => None,
        })
        .collect();
    let marks: Vec<Mark<u8, u8>> = log
        .iter()
        .filter_map(|op| match op {
            Op::Mark(mark) => Some(mark.clone()),
            _ => None,
        })
        .collect();
    let order = rga_spec::linearize(&inserts);
    let mut out = BTreeMap::new();
    for atom in &order {
        for key in 0..3u8 {
            if let Some(found) = spec::effective(&order, &marks, *atom, &key) {
                out.insert((*atom, key), found);
            }
        }
    }
    out
}

/// Delivers `log` to a fresh replica in the causal order `picks` chooses.
fn deliver_in_random_causal_order(log: &[Op], picks: &[usize]) -> Peer {
    let mut peer = Peer::new(99);
    let mut pending: Vec<&Op> = log.iter().collect();
    let mut pick = picks.iter().cycle();
    while !pending.is_empty() {
        let ready: Vec<usize> = pending
            .iter()
            .enumerate()
            .filter(|(_, op)| peer.ready(op))
            .map(|(i, _)| i)
            .collect();
        let chosen = ready[*pick.next().unwrap() % ready.len()];
        let op = pending.remove(chosen);
        peer.apply(op);
    }
    peer
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Replicas that exchange inserts, deletions and marks in arbitrary
    /// causal orders end up with identical formatting on every atom, equal
    /// to the specification (Lean: `format_converges`).
    #[test]
    fn concurrent_replicas_converge_to_the_spec(
        actions in prop::collection::vec(action(3), 0..80),
    ) {
        let (log, replicas) = run(3, &actions);
        let expected = spec_of(&log);
        for replica in &replicas {
            prop_assert_eq!(replica.rga.ids(), replicas[0].rga.ids());
            prop_assert_eq!(replica.marks.effective(&replica.rga), replicas[0].marks.effective(&replicas[0].rga));
            prop_assert_eq!(&per_atom(&replica.rga, &replica.marks), &expected);
        }
    }

    /// The same history delivered to a fresh replica in any causal order
    /// gives the same formatting.
    #[test]
    fn any_causal_delivery_order_gives_the_same_formatting(
        actions in prop::collection::vec(action(3), 0..60),
        picks in prop::collection::vec(any::<usize>(), 200),
    ) {
        let (log, replicas) = run(3, &actions);
        let peer = deliver_in_random_causal_order(&log, &picks);
        prop_assert_eq!(peer.marks.effective(&peer.rga), replicas[0].marks.effective(&replicas[0].rga));
    }

    /// The sweep agrees with the definition atom by atom, on every replica,
    /// before the final sync as well.
    #[test]
    fn the_sweep_agrees_with_the_specification_mid_history(
        actions in prop::collection::vec(action(2), 0..60),
        cut in any::<usize>(),
    ) {
        let cut = cut % (actions.len() + 1);
        let (log, _) = run(2, &actions[..cut]);
        // One replica that saw a causal prefix of the log only.
        let mut peer = Peer::new(7);
        for op in log.iter().take(log.len() / 2 + 1) {
            if peer.ready(op) {
                peer.apply(op);
            }
        }
        let order = peer.rga.ids();
        let marks: Vec<Mark<u8, u8>> = peer.marks.iter().cloned().collect();
        let table = per_atom(&peer.rga, &peer.marks);
        for atom in &order {
            for key in 0..3u8 {
                prop_assert_eq!(table.get(&(*atom, key)).cloned(), spec::effective(&order, &marks, *atom, &key));
                prop_assert_eq!(
                    peer.marks.at(&peer.rga, *atom, &key).map(|(id, v)| (id, v.copied())),
                    spec::effective(&order, &marks, *atom, &key)
                );
            }
        }
    }

    /// Inserting atoms anywhere never changes what the old atoms show
    /// (Lean: `effective_preserved`).
    #[test]
    fn inserting_never_reformats_existing_atoms(
        actions in prop::collection::vec(action(2), 0..60),
        inserts in prop::collection::vec((any::<usize>(), prop::char::range('A', 'Z')), 1..10),
    ) {
        let (_, mut replicas) = run(2, &actions);
        let peer = &mut replicas[0];
        let before = per_atom(&peer.rga, &peer.marks);
        let old: BTreeSet<Id> = peer.rga.ids().into_iter().collect();
        for (at, value) in inserts {
            let len = peer.rga.len();
            let parent = (at % (len + 1) != len).then(|| peer.rga.nodes()[at % (len + 1)].id);
            let id = peer.clock.tick();
            peer.rga.insert(id, parent, value).unwrap();
        }
        let after: BTreeMap<(Id, u8), (Id, Option<u8>)> = per_atom(&peer.rga, &peer.marks)
            .into_iter()
            .filter(|((atom, _), _)| old.contains(atom))
            .collect();
        prop_assert_eq!(after, before);
    }

    /// The expand rules, for a mark made on one replica and a character
    /// typed concurrently on another, in either delivery order: inside the
    /// range it is covered, right after the last character it is covered
    /// exactly when the attribute expands, right before the first never
    /// (Lean: `typed_inside`, `typed_after_end`, `typed_before_start`).
    #[test]
    fn typing_at_a_marks_edges_follows_the_expand_rules(
        actions in prop::collection::vec(action(1), 1..40),
        from in any::<usize>(),
        len in 0..5usize,
        expand in any::<bool>(),
        place in 0..3u8,
        inside in any::<usize>(),
        mark_first in any::<bool>(),
    ) {
        let (_, replicas) = run(1, &actions);
        let base = &replicas[0].rga;
        let visible = base.visible_len();
        prop_assume!(visible > 0);
        let from = from % visible;
        let to = (from + len).min(visible - 1);
        let (first, last) = (base.visible_id_at(from).unwrap(), base.visible_id_at(to).unwrap());
        let mode = if expand { Expand::End } else { Expand::None };
        let (start, end) = anchors(base, first, last, mode).unwrap();
        let m = Mark::new(Id::new(base.max_lamport() + 1, ReplicaId(5)), 0u8, start, end, Some(1u8));
        let order = base.ids();
        let (fp, lp) = (base.position(first).unwrap(), base.position(last).unwrap());
        // The concurrent typist's parent and what the rules promise.
        let (parent, covered) = match place {
            0 => (Some(last), expand),
            1 => (fp.checked_sub(1).map(|p| order[p]), false),
            _ => {
                prop_assume!(lp > fp);
                (Some(order[fp + inside % (lp - fp)]), true)
            }
        };
        let typed = Id::new(base.max_lamport() + 1, ReplicaId(6));
        let mut rga = base.clone();
        let mut marks = MarkSet::new();
        if mark_first {
            marks.insert(m.clone()).unwrap();
            rga.insert(typed, parent, '*').unwrap();
        } else {
            rga.insert(typed, parent, '*').unwrap();
            marks.insert(m.clone()).unwrap();
        }
        prop_assert_eq!(marks.at(&rga, typed, &0).is_some(), covered);
    }
}
