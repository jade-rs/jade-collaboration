use jade_collab_core::id::{Clock, Id, IdError, ReplicaId};
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

fn boundary() -> impl Strategy<Value = u64> {
    prop_oneof![
        0..32u64,
        any::<u64>(),
        (0..32u64).prop_map(|n| u64::MAX - n)
    ]
}

proptest! {
    /// A u128 reference clock covers arbitrary observations, reservations,
    /// exhaustion and later retries, including the exact last representable id.
    #[test]
    fn checked_clock_and_ranges_match_unbounded_arithmetic(
        steps in prop::collection::vec((any::<bool>(), boundary(), boundary()), 1..100),
    ) {
        let mut clock = Clock::new(ReplicaId(7));
        let mut reference = 0u128;
        for (observe, value, count) in steps {
            if observe {
                clock.observe_lamport(value);
                reference = reference.max(u128::from(value));
            } else {
                let before = clock.clone();
                let result = clock.try_reserve(count);
                if count > 0 && reference + u128::from(count) <= u128::from(u64::MAX) {
                    prop_assert_eq!(result, Ok(Id::new((reference + 1) as u64, ReplicaId(7))));
                    reference += u128::from(count);
                } else {
                    prop_assert_eq!(result, Err(if count == 0 { IdError::EmptyReservation } else { IdError::Exhausted }));
                    prop_assert_eq!(&clock, &before, "failed reservations do not advance time");
                }
            }
            prop_assert_eq!(u128::from(clock.now()), reference);
            let start = Id::new(value, ReplicaId(3));
            let last = u128::from(value) + u128::from(count.saturating_sub(1));
            let actual = start.last_in_run(count);
            if count > 0 && last <= u128::from(u64::MAX) {
                prop_assert_eq!(actual, Ok(Id::new(last as u64, ReplicaId(3))));
            } else {
                prop_assert!(actual.is_err());
            }
        }
    }
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
