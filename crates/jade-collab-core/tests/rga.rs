use std::collections::BTreeSet;

use jade_collab_core::id::{Clock, Id, ReplicaId};
use jade_collab_core::rga::{spec, Rga, RgaError};
use proptest::prelude::*;

fn id(lamport: u64, replica: u64) -> Id {
    Id::new(lamport, ReplicaId(replica))
}

fn text(rga: &Rga<char>) -> String {
    rga.visible().map(|(_, c)| *c).collect()
}

#[test]
fn typing_in_order_reads_back() {
    let mut rga = Rga::new();
    rga.insert(id(1, 1), None, 'a').unwrap();
    rga.insert(id(2, 1), Some(id(1, 1)), 'b').unwrap();
    rga.insert(id(3, 1), Some(id(2, 1)), 'c').unwrap();
    assert_eq!(text(&rga), "abc");
    assert_eq!(rga.visible_len(), 3);
}

#[test]
fn concurrent_siblings_order_by_descending_id_whatever_the_arrival() {
    let mut a = Rga::new();
    a.insert(id(1, 1), None, 'x').unwrap();
    a.insert(id(1, 2), None, 'y').unwrap();
    let mut b = Rga::new();
    b.insert(id(1, 2), None, 'y').unwrap();
    b.insert(id(1, 1), None, 'x').unwrap();
    assert_eq!(text(&a), "yx");
    assert_eq!(text(&a), text(&b));
}

#[test]
fn an_insert_skips_newer_siblings_and_their_subtrees() {
    // "ab" typed by replica 1, then replica 2 typed "XY" after 'a' and
    // replica 3 concurrently typed "Z" after 'a' with an older lamport.
    let mut rga = Rga::new();
    rga.insert(id(1, 1), None, 'a').unwrap();
    rga.insert(id(2, 1), Some(id(1, 1)), 'b').unwrap();
    rga.insert(id(5, 2), Some(id(1, 1)), 'X').unwrap();
    rga.insert(id(6, 2), Some(id(5, 2)), 'Y').unwrap();
    rga.insert(id(3, 3), Some(id(1, 1)), 'Z').unwrap();
    assert_eq!(text(&rga), "aXYZb");
}

#[test]
fn deletion_leaves_a_tombstone_that_still_anchors_inserts() {
    let mut rga = Rga::new();
    rga.insert(id(1, 1), None, 'a').unwrap();
    rga.insert(id(2, 1), Some(id(1, 1)), 'b').unwrap();
    assert_eq!(rga.delete(id(1, 1)), Ok(true));
    assert_eq!(
        rga.delete(id(1, 1)),
        Ok(false),
        "deleting twice is idempotent"
    );
    rga.insert(id(3, 2), Some(id(1, 1)), 'c').unwrap();
    assert_eq!(text(&rga), "cb");
    assert!(rga.contains(id(1, 1)));
    assert!(!rga.is_visible(id(1, 1)));
}

#[test]
fn positions_are_reported_in_visible_coordinates() {
    let mut rga = Rga::new();
    rga.insert(id(1, 1), None, 'a').unwrap();
    rga.insert(id(2, 1), Some(id(1, 1)), 'b').unwrap();
    rga.insert(id(3, 1), Some(id(2, 1)), 'c').unwrap();
    rga.delete(id(2, 1)).unwrap();
    assert_eq!(rga.visible_index(id(3, 1)), Some(1));
    assert_eq!(rga.visible_index(id(2, 1)), None);
    assert_eq!(rga.visible_id_at(1), Some(id(3, 1)));
    assert_eq!(rga.visible_id_at(2), None);
    // The visible atom before a tombstone is the anchor a caret there uses.
    assert_eq!(rga.visible_before(id(2, 1)), Some(id(1, 1)));
}

#[test]
fn rejects_malformed_operations() {
    let mut rga = Rga::new();
    rga.insert(id(2, 1), None, 'a').unwrap();
    assert_eq!(
        rga.insert(id(2, 1), None, 'b'),
        Err(RgaError::DuplicateId(id(2, 1)))
    );
    assert_eq!(
        rga.insert(id(3, 1), Some(id(9, 9)), 'b'),
        Err(RgaError::MissingParent(id(9, 9)))
    );
    assert_eq!(
        rga.insert(id(2, 5), Some(id(2, 1)), 'b'),
        Err(RgaError::NotNewerThanParent {
            id: id(2, 5),
            parent: id(2, 1)
        })
    );
    assert_eq!(rga.delete(id(7, 7)), Err(RgaError::MissingTarget(id(7, 7))));
    assert_eq!(
        text(&rga),
        "a",
        "failed operations leave the sequence untouched"
    );
}

#[test]
fn an_overflowing_run_is_refused_without_changing_the_sequence() {
    let mut rga = Rga::new();
    rga.insert(id(1, 1), None, 'a').unwrap();
    let before = rga.clone();
    assert!(rga
        .insert_run(id(u64::MAX - 1, 2), None, ['x', 'y', 'z'])
        .is_err());
    assert_eq!(rga, before);
}

#[test]
fn a_run_ending_at_the_last_identifier_reports_collisions_without_overflowing() {
    let mut rga = Rga::new();
    let last = id(u64::MAX, 2);
    rga.insert(last, None, 'a').unwrap();
    let before = rga.clone();
    assert_eq!(
        rga.insert_run(id(u64::MAX - 1, 2), None, ['x', 'y']),
        Err(RgaError::DuplicateId(last))
    );
    assert_eq!(rga, before);
    let mut empty = Rga::new();
    empty
        .insert_run(id(u64::MAX - 1, 2), None, ['x', 'y'])
        .unwrap();
    assert_eq!(text(&empty), "xy");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// A set-of-inserts reference, using u128 arithmetic for proposed runs,
    /// remains equal after malformed runs, deletions, retries and restoration.
    #[test]
    fn boundary_runs_remain_atomic_and_match_the_reference(
        steps in prop::collection::vec((
            0u8..4,
            prop_oneof![0..100u64, (0..8u64).prop_map(|n| u64::MAX - n)],
            1..4u64, 0..6usize, any::<usize>(),
        ), 1..90),
    ) {
        let mut rga = Rga::new();
        let mut inserts: Vec<spec::Insert<char>> = Vec::new();
        let mut deleted = BTreeSet::new();
        for (kind, first, replica, count, choice) in steps {
            let ids = spec::linearize(&inserts);
            match kind {
                1 if !ids.is_empty() => {
                    let target = ids[choice % ids.len()];
                    rga.delete(target).unwrap();
                    deleted.insert(target);
                }
                2 => { rga = serde_json::from_str(&serde_json::to_string(&rga).unwrap()).unwrap(); }
                _ => {
                    let parent = if choice % (ids.len() + 1) == ids.len() { None }
                        else { Some(ids[choice % (ids.len() + 1)]) };
                    let first = id(first, replica);
                    let before = rga.clone();
                    let value = (b'a' + (choice % 26) as u8) as char;
                    let fits = count == 0 || u128::from(first.lamport) + count as u128 - 1 <= u128::from(u64::MAX);
                    let fresh = count == 0 || (0..count).all(|k| {
                        !inserts.iter().any(|n| n.id.replica == first.replica && u128::from(n.id.lamport) == u128::from(first.lamport) + k as u128)
                    });
                    let newer = count == 0 || parent.is_none_or(|p| first.lamport > p.lamport);
                    let result = rga.insert_run(first, parent, vec![value; count]);
                    prop_assert_eq!(result.is_ok(), fits && fresh && newer);
                    if result.is_ok() {
                        let mut previous = parent;
                        for k in 0..count {
                            let next = id((u128::from(first.lamport) + k as u128) as u64, replica);
                            inserts.push(spec::Insert::new(next, previous, value));
                            previous = Some(next);
                        }
                    } else { prop_assert_eq!(&rga, &before); }
                }
            }
            prop_assert_eq!(rga.ids(), spec::linearize(&inserts));
            prop_assert_eq!(rga.visible().map(|(id, value)| (id, *value)).collect::<Vec<_>>(), spec::render(&inserts, &deleted));
        }
    }
}

#[test]
fn spec_renders_a_dfs_with_descending_siblings() {
    let inserts = vec![
        spec::Insert::new(id(1, 1), None, 'a'),
        spec::Insert::new(id(2, 1), Some(id(1, 1)), 'b'),
        spec::Insert::new(id(5, 2), Some(id(1, 1)), 'X'),
        spec::Insert::new(id(6, 2), Some(id(5, 2)), 'Y'),
        spec::Insert::new(id(3, 3), Some(id(1, 1)), 'Z'),
    ];
    let deleted = BTreeSet::from([id(6, 2)]);
    let rendered: String = spec::render(&inserts, &deleted)
        .into_iter()
        .map(|(_, c)| c)
        .collect();
    assert_eq!(rendered, "aXZb");
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
    /// A typed run: `len` atoms chained after `parent`, ids `first`, ….
    InsertRun {
        first: Id,
        parent: Option<Id>,
        values: Vec<char>,
    },
    Delete {
        id: Id,
    },
}

#[derive(Clone, Debug)]
enum Action {
    Insert { peer: usize, at: usize, value: char },
    InsertRun { peer: usize, at: usize, len: usize },
    Delete { peer: usize, at: usize },
    Sync { from: usize, to: usize },
}

fn action(peers: usize) -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => (0..peers, any::<usize>(), prop::char::range('a', 'z'))
            .prop_map(|(peer, at, value)| Action::Insert { peer, at, value }),
        2 => (0..peers, any::<usize>()).prop_map(|(peer, at)| Action::Delete { peer, at }),
        2 => (0..peers, 0..peers).prop_map(|(from, to)| Action::Sync { from, to }),
    ]
}

/// Like [`action`], plus typed runs, so histories grow to thousands of atoms.
fn large_action(peers: usize) -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => (0..peers, any::<usize>(), prop::char::range('a', 'z'))
            .prop_map(|(peer, at, value)| Action::Insert { peer, at, value }),
        2 => (0..peers, any::<usize>(), 1usize..60)
            .prop_map(|(peer, at, len)| Action::InsertRun { peer, at, len }),
        3 => (0..peers, any::<usize>()).prop_map(|(peer, at)| Action::Delete { peer, at }),
        2 => (0..peers, 0..peers).prop_map(|(from, to)| Action::Sync { from, to }),
    ]
}

struct Peer {
    rga: Rga<char>,
    clock: Clock,
    seen: BTreeSet<usize>,
}

fn deliver(peer: &mut Peer, index: usize, op: &Op) {
    if !peer.seen.insert(index) {
        return;
    }
    match op {
        Op::Insert { id, parent, value } => {
            peer.clock.observe(*id);
            peer.rga.insert(*id, *parent, *value).unwrap();
        }
        Op::InsertRun {
            first,
            parent,
            values,
        } => {
            peer.clock.observe(first.offset(values.len() as u64 - 1));
            peer.rga
                .insert_run(*first, *parent, values.iter().copied())
                .unwrap();
        }
        Op::Delete { id } => {
            peer.rga.delete(*id).unwrap();
        }
    }
}

/// Anchors on any atom (tombstones included) or the start.
fn anchor(rga: &Rga<char>, at: usize) -> Option<Id> {
    let len = rga.len();
    if len == 0 || at % (len + 1) == len {
        None
    } else {
        Some(rga.nodes()[at % (len + 1)].id)
    }
}

/// Runs `actions` over `peers` replicas and returns the global op log and the
/// replicas after a final full sync.
fn run(peers: usize, actions: &[Action]) -> (Vec<Op>, Vec<Peer>) {
    let mut log: Vec<Op> = Vec::new();
    let mut replicas: Vec<Peer> = (0..peers)
        .map(|p| Peer {
            rga: Rga::new(),
            clock: Clock::new(ReplicaId(p as u64 + 1)),
            seen: BTreeSet::new(),
        })
        .collect();
    for action in actions {
        match *action {
            Action::Insert { peer, at, value } => {
                let replica = &mut replicas[peer];
                let parent = anchor(&replica.rga, at);
                let id = replica.clock.tick();
                let op = Op::Insert { id, parent, value };
                log.push(op.clone());
                deliver(replica, log.len() - 1, &op);
            }
            Action::InsertRun { peer, at, len } => {
                let replica = &mut replicas[peer];
                let parent = anchor(&replica.rga, at);
                let first = replica.clock.reserve(len as u64);
                let values = (0..len).map(|k| (b'A' + (k % 26) as u8) as char).collect();
                let op = Op::InsertRun {
                    first,
                    parent,
                    values,
                };
                log.push(op.clone());
                deliver(replica, log.len() - 1, &op);
            }
            Action::Delete { peer, at } => {
                let replica = &mut replicas[peer];
                let len = replica.rga.visible_len();
                if len == 0 {
                    continue;
                }
                let id = replica.rga.visible_id_at(at % len).unwrap();
                let op = Op::Delete { id };
                log.push(op.clone());
                deliver(replica, log.len() - 1, &op);
            }
            Action::Sync { from, to } => {
                if from == to {
                    continue;
                }
                let indices: Vec<usize> = replicas[from].seen.iter().copied().collect();
                for index in indices {
                    deliver(&mut replicas[to], index, &log[index]);
                }
            }
        }
    }
    for replica in &mut replicas {
        for (index, op) in log.iter().enumerate() {
            deliver(replica, index, op);
        }
    }
    (log, replicas)
}

fn spec_of(log: &[Op]) -> (Vec<spec::Insert<char>>, BTreeSet<Id>) {
    let mut inserts = Vec::new();
    let mut deleted = BTreeSet::new();
    for op in log {
        match op {
            Op::Insert { id, parent, value } => {
                inserts.push(spec::Insert::new(*id, *parent, *value))
            }
            Op::InsertRun {
                first,
                parent,
                values,
            } => {
                let mut previous = *parent;
                for (k, value) in values.iter().enumerate() {
                    let id = first.offset(k as u64);
                    inserts.push(spec::Insert::new(id, previous, *value));
                    previous = Some(id);
                }
            }
            Op::Delete { id } => {
                deleted.insert(*id);
            }
        }
    }
    (inserts, deleted)
}

/// Delivers `log` to a fresh sequence in the causal order `picks` chooses.
fn deliver_in_random_causal_order(log: &[Op], picks: &[usize]) -> Rga<char> {
    let mut rga: Rga<char> = Rga::new();
    let mut pending: Vec<&Op> = log.iter().collect();
    let mut pick = picks.iter().cycle();
    while !pending.is_empty() {
        let ready: Vec<usize> = pending
            .iter()
            .enumerate()
            .filter(|(_, op)| match op {
                Op::Insert { parent, .. } | Op::InsertRun { parent, .. } => {
                    parent.is_none_or(|p| rga.contains(p))
                }
                Op::Delete { id } => rga.contains(*id),
            })
            .map(|(i, _)| i)
            .collect();
        let chosen = ready[*pick.next().unwrap() % ready.len()];
        match pending.remove(chosen) {
            Op::Insert { id, parent, value } => {
                rga.insert(*id, *parent, *value).unwrap();
            }
            Op::InsertRun {
                first,
                parent,
                values,
            } => {
                rga.insert_run(*first, *parent, values.iter().copied())
                    .unwrap();
            }
            Op::Delete { id } => {
                rga.delete(*id).unwrap();
            }
        }
    }
    rga
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Replicas that exchange operations in arbitrary causal orders end up
    /// identical — tombstones and all — and equal to the specification.
    #[test]
    fn concurrent_replicas_converge_to_the_spec(
        actions in prop::collection::vec(action(3), 0..80),
    ) {
        let (log, replicas) = run(3, &actions);
        let (inserts, deleted) = spec_of(&log);
        let expected = spec::render(&inserts, &deleted);
        for replica in &replicas {
            prop_assert_eq!(replica.rga.ids(), replicas[0].rga.ids());
            let visible: Vec<(Id, char)> = replica.rga.visible().map(|(id, c)| (id, *c)).collect();
            prop_assert_eq!(&visible, &expected);
        }
        prop_assert_eq!(replicas[0].rga.ids(), spec::linearize(&inserts));
    }

    /// Feeding the same history to a fresh sequence in a random causal order
    /// gives the specification's document.
    #[test]
    fn any_causal_delivery_order_matches_the_spec(
        actions in prop::collection::vec(action(3), 0..60),
        picks in prop::collection::vec(any::<usize>(), 200),
    ) {
        let (log, _) = run(3, &actions);
        let rga = deliver_in_random_causal_order(&log, &picks);
        let (inserts, deleted) = spec_of(&log);
        let visible: Vec<(Id, char)> = rga.visible().map(|(id, c)| (id, *c)).collect();
        prop_assert_eq!(visible, spec::render(&inserts, &deleted));
    }

    /// The specification itself ignores delivery order and duplicates
    /// (the Lean theorem `render_congr`, checked on random data).
    #[test]
    fn spec_is_order_and_duplicate_insensitive(
        actions in prop::collection::vec(action(2), 0..40),
        seed in any::<u64>(),
    ) {
        let (log, _) = run(2, &actions);
        let (inserts, deleted) = spec_of(&log);
        let mut shuffled = inserts.clone();
        shuffled.extend(inserts.iter().take(2).cloned());
        let mut state = seed;
        for i in (1..shuffled.len()).rev() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            shuffled.swap(i, (state >> 33) as usize % (i + 1));
        }
        prop_assert_eq!(spec::render(&inserts, &deleted), spec::render(&shuffled, &deleted));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Histories of thousands of atoms — typed runs, concurrent single
    /// inserts and deletions across five replicas — still converge to the
    /// specification.
    #[test]
    fn large_concurrent_histories_converge_to_the_spec(
        actions in prop::collection::vec(large_action(5), 0..400),
        picks in prop::collection::vec(any::<usize>(), 64),
    ) {
        let (log, replicas) = run(5, &actions);
        let (inserts, deleted) = spec_of(&log);
        let expected = spec::render(&inserts, &deleted);
        let order = spec::linearize(&inserts);
        for replica in &replicas {
            prop_assert_eq!(replica.rga.ids(), order.clone());
            let visible: Vec<(Id, char)> = replica.rga.visible().map(|(id, c)| (id, *c)).collect();
            prop_assert_eq!(&visible, &expected);
            for (index, (id, _)) in expected.iter().enumerate() {
                prop_assert_eq!(replica.rga.visible_index(*id), Some(index));
                prop_assert_eq!(replica.rga.visible_id_at(index), Some(*id));
            }
            for (position, id) in order.iter().enumerate() {
                prop_assert_eq!(replica.rga.position(*id), Some(position));
            }
        }
        let rga = deliver_in_random_causal_order(&log, &picks);
        prop_assert_eq!(rga.ids(), order);
    }
}
