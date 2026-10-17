use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::lww::LwwRegister;
use proptest::prelude::*;

fn id(lamport: u64, replica: u64) -> Id {
    Id::new(lamport, ReplicaId(replica))
}

#[test]
fn later_stamp_wins_regardless_of_arrival() {
    let mut a = LwwRegister::default();
    assert!(a.apply(id(1, 1), "first"));
    assert!(a.apply(id(2, 1), "second"));
    assert_eq!(a.get(), Some(&"second"));

    let mut b = LwwRegister::default();
    assert!(b.apply(id(2, 1), "second"));
    assert!(!b.apply(id(1, 1), "first"), "an older write must not win");
    assert_eq!(b.get(), Some(&"second"));
}

#[test]
fn replica_breaks_lamport_ties() {
    let mut reg = LwwRegister::default();
    reg.apply(id(3, 2), "two");
    reg.apply(id(3, 1), "one");
    assert_eq!(reg.get(), Some(&"two"));
    assert_eq!(reg.stamp(), Some(id(3, 2)));
}

#[test]
fn reapplying_the_same_write_is_a_no_op() {
    let mut reg = LwwRegister::default();
    assert!(reg.apply(id(1, 1), 10));
    assert!(!reg.apply(id(1, 1), 10));
}

#[test]
fn merge_keeps_the_newest() {
    let mut a = LwwRegister::default();
    a.apply(id(5, 1), 'a');
    let mut b = LwwRegister::default();
    b.apply(id(6, 2), 'b');
    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);
    assert_eq!(ab, ba);
    assert_eq!(ab.get(), Some(&'b'));
}

proptest! {
    /// Strong eventual consistency for one register: any delivery order of
    /// the same writes, with duplicates, gives the same register.
    #[test]
    fn any_delivery_order_converges(
        writes in prop::collection::btree_map((0u64..20, 0u64..4), any::<i32>(), 0..20),
        seed in any::<u64>(),
    ) {
        let writes: Vec<_> = writes.into_iter().map(|((l, r), v)| (id(l, r), v)).collect();
        let mut forward = LwwRegister::default();
        for (stamp, value) in &writes {
            forward.apply(*stamp, *value);
        }
        // A deterministic shuffle plus duplicates.
        let mut shuffled = writes.clone();
        shuffled.extend(writes.iter().take(3).cloned());
        let mut state = seed;
        for i in (1..shuffled.len()).rev() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            shuffled.swap(i, (state >> 33) as usize % (i + 1));
        }
        let mut other = LwwRegister::default();
        for (stamp, value) in &shuffled {
            other.apply(*stamp, *value);
        }
        prop_assert_eq!(&forward, &other);
        let newest = writes.iter().max_by_key(|(stamp, _)| *stamp);
        prop_assert_eq!(forward.get(), newest.map(|(_, v)| v));
    }

    #[test]
    fn merge_is_a_semilattice_join(
        a in prop::option::of(((0u64..10, 0u64..3), any::<u8>())),
        b in prop::option::of(((0u64..10, 0u64..3), any::<u8>())),
        c in prop::option::of(((0u64..10, 0u64..3), any::<u8>())),
    ) {
        let make = |w: Option<((u64, u64), u8)>| {
            let mut reg = LwwRegister::default();
            if let Some(((l, r), v)) = w {
                reg.apply(id(l, r), v);
            }
            reg
        };
        let (a, b, c) = (make(a), make(b), make(c));
        let join = |x: &LwwRegister<u8>, y: &LwwRegister<u8>| {
            let mut out = x.clone();
            out.merge(y);
            out
        };
        // Commutative only up to equal stamps carrying equal values; stamps are
        // unique per write in the system, so compare stamps.
        prop_assert_eq!(join(&a, &b).stamp(), join(&b, &a).stamp());
        prop_assert_eq!(join(&join(&a, &b), &c).stamp(), join(&a, &join(&b, &c)).stamp());
        prop_assert_eq!(join(&a, &a), a.clone());
    }
}
