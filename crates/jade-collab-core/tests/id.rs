use jade_collab_core::id::{Clock, Id, ReplicaId};
use proptest::prelude::*;

#[test]
fn ids_order_by_lamport_then_replica() {
    let a = Id::new(1, ReplicaId(9));
    let b = Id::new(2, ReplicaId(1));
    let c = Id::new(2, ReplicaId(3));
    assert!(a < b);
    assert!(b < c);
}

#[test]
fn clock_ticks_strictly_increase() {
    let mut clock = Clock::new(ReplicaId(4));
    let first = clock.tick();
    let second = clock.tick();
    assert_eq!(first, Id::new(1, ReplicaId(4)));
    assert_eq!(second, Id::new(2, ReplicaId(4)));
}

#[test]
fn clock_jumps_past_observed_ids() {
    let mut clock = Clock::new(ReplicaId(1));
    clock.observe(Id::new(41, ReplicaId(7)));
    assert_eq!(clock.tick(), Id::new(42, ReplicaId(1)));
    // Observing something older never moves the clock back.
    clock.observe(Id::new(3, ReplicaId(7)));
    assert_eq!(clock.tick(), Id::new(43, ReplicaId(1)));
}

#[test]
fn reserving_a_run_hands_out_consecutive_ids() {
    let mut clock = Clock::new(ReplicaId(2));
    let first = clock.reserve(3);
    assert_eq!(first, Id::new(1, ReplicaId(2)));
    assert_eq!(clock.tick(), Id::new(4, ReplicaId(2)));
}

#[test]
fn ids_round_trip_through_json() {
    let id = Id::new(5, ReplicaId(6));
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<Id>(&json).unwrap(), id);
}

proptest! {
    /// Whatever a replica observes, the next id it hands out is greater than
    /// every id it has seen — the property the RGA integration rule needs.
    #[test]
    fn tick_exceeds_everything_observed(
        observed in prop::collection::vec((0u64..1_000, 0u64..8), 0..50),
        replica in 0u64..8,
    ) {
        let mut clock = Clock::new(ReplicaId(replica));
        let mut max = None;
        for (lamport, from) in observed {
            let id = Id::new(lamport, ReplicaId(from));
            clock.observe(id);
            max = max.max(Some(id));
            let next = clock.tick();
            prop_assert!(Some(next) > max);
            prop_assert!(next.lamport > max.unwrap().lamport);
            max = Some(next);
        }
    }
}
