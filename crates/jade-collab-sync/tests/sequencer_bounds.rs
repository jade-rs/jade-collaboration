use std::collections::BTreeMap;

use jade_collab_core::id::ReplicaId;
use jade_collab_sync::protocol::{Committed, Submission};
use jade_collab_sync::{Checkpoint, Document, Lane, NoMaterializer, Record, Rejection, Sequencer};

#[derive(Clone, Debug, PartialEq)]
struct Counter(u64);
impl Document for Counter {
    type Op = ();
    type Effect = ();
    fn lane(_: &()) -> Lane {
        Lane::Convergent
    }
    fn apply(&mut self, _: &(), _: &jade_collab_sync::ApplyContext) -> Result<(), Rejection> {
        self.0 += 1;
        Ok(())
    }
    fn max_lamport(&self) -> u64 {
        0
    }
}

fn checkpoint(seq: u64) -> Checkpoint<Counter> {
    Checkpoint {
        seq,
        processed: BTreeMap::from([(ReplicaId(1), 4)]),
        owners: BTreeMap::new(),
        next_replica: 2,
        faults: vec![],
        document: Counter(0),
    }
}
fn committed(seq: u64, ticket: u64) -> Committed<()> {
    Committed {
        seq,
        origin: ReplicaId(1),
        client_seq: ticket,
        base_seq: seq.saturating_sub(1),
        op: (),
    }
}

#[test]
fn restoring_empty_or_retained_tails_at_the_last_log_position_does_not_overflow() {
    for tail in [
        vec![],
        vec![committed(u64::MAX, 4)],
        vec![committed(u64::MAX - 1, 3), committed(u64::MAX, 4)],
    ] {
        let server =
            Sequencer::restore_with_tail(checkpoint(u64::MAX), tail.clone(), NoMaterializer)
                .unwrap();
        assert_eq!(server.seq(), u64::MAX);
        assert_eq!(server.log(), tail);
        assert_eq!(
            server.resume(ReplicaId(1), u64::MAX).unwrap().missed.len(),
            0
        );
    }
}

#[test]
fn replay_after_the_last_log_position_is_an_error_without_effects() {
    let mut server = Sequencer::restore(checkpoint(u64::MAX), NoMaterializer);
    let before = server.checkpoint();
    assert!(server.replay(Record::Committed(committed(0, 5))).is_err());
    assert_eq!(server.checkpoint(), before);
}

#[test]
fn the_full_log_refuses_new_user_and_origin_edits_but_records_the_user_verdict() {
    let mut server = Sequencer::restore(checkpoint(u64::MAX), NoMaterializer);
    let before = server.checkpoint();
    assert!(server.commit_origin(()).is_err());
    assert_eq!(server.checkpoint(), before);
    let outcome = server.submit(
        ReplicaId(1),
        Submission {
            client_seq: 5,
            base_seq: u64::MAX,
            op: (),
        },
    );
    assert!(matches!(
        outcome,
        jade_collab_sync::SubmitOutcome::Rejected(_)
    ));
    assert_eq!(server.document(), &before.document);
    assert_eq!(server.seq(), u64::MAX);
    assert_eq!(server.processed_client_seq(ReplicaId(1)), 5);
    assert!(server.log().is_empty());
    let record = outcome.record(ReplicaId(1)).unwrap();
    let mut restored = Sequencer::restore(before, NoMaterializer);
    restored.replay(record).unwrap();
    assert_eq!(restored.checkpoint(), server.checkpoint());
    assert_eq!(
        restored.submit(
            ReplicaId(1),
            Submission {
                client_seq: 5,
                base_seq: u64::MAX,
                op: ()
            }
        ),
        jade_collab_sync::SubmitOutcome::Duplicate
    );
}

#[test]
fn journal_replay_cannot_rewind_or_repeat_a_processed_ticket() {
    for ticket in [0, 1, 4] {
        for record in [
            Record::Rejected {
                replica: ReplicaId(1),
                client_seq: ticket,
            },
            Record::Committed(committed(1, ticket)),
        ] {
            let mut server = Sequencer::restore(checkpoint(0), NoMaterializer);
            let before = server.checkpoint();
            assert!(
                server.replay(record).is_err(),
                "ticket {ticket} already answered"
            );
            assert_eq!(server.checkpoint(), before);
        }
    }
    let mut server = Sequencer::restore(checkpoint(0), NoMaterializer);
    server
        .replay(Record::Rejected {
            replica: ReplicaId(1),
            client_seq: 5,
        })
        .unwrap();
    server.replay(Record::Committed(committed(1, 7))).unwrap(); // issued tickets may have gaps
    assert_eq!(server.processed_client_seq(ReplicaId(1)), 7);
}

#[test]
fn restored_metadata_cannot_reissue_ids_or_bind_unknown_owners() {
    for kind in 0..5 {
        let mut checkpoint = checkpoint(0);
        match kind {
            0 => checkpoint.next_replica = 0,
            1 => checkpoint.next_replica = 1,
            2 => {
                checkpoint.processed.insert(ReplicaId::ORIGIN, 0);
            }
            3 => {
                checkpoint.owners.insert(ReplicaId(9), "unknown".into());
            }
            _ => {
                checkpoint.processed.insert(ReplicaId(u64::MAX), 0);
                checkpoint.next_replica = u64::MAX;
            }
        }
        assert!(
            Sequencer::restore_with_tail(checkpoint, vec![], NoMaterializer).is_err(),
            "invalid metadata kind {kind}"
        );
    }
}

#[test]
fn retained_tail_must_fit_the_checkpoints_replica_watermarks() {
    for entry in [
        committed(1, 0),
        committed(1, 5),
        Committed {
            origin: ReplicaId(9),
            ..committed(1, 1)
        },
        Committed {
            origin: ReplicaId::ORIGIN,
            ..committed(1, 1)
        },
    ] {
        assert!(Sequencer::restore_with_tail(checkpoint(1), vec![entry], NoMaterializer).is_err());
    }
    assert!(Sequencer::restore_with_tail(
        checkpoint(2),
        vec![committed(1, 4), committed(2, 3)],
        NoMaterializer
    )
    .is_err());
}

#[test]
fn a_refused_replay_leaves_later_live_tickets_duplicate() {
    let mut server = Sequencer::restore(checkpoint(0), NoMaterializer);
    assert!(server
        .replay(Record::Rejected {
            replica: ReplicaId(1),
            client_seq: 1
        })
        .is_err());
    assert_eq!(
        server.submit(
            ReplicaId(1),
            Submission {
                client_seq: 4,
                base_seq: 0,
                op: ()
            }
        ),
        jade_collab_sync::SubmitOutcome::Duplicate
    );
}

#[test]
fn exhausted_replica_issuance_preserves_existing_owners_and_can_still_resume() {
    let mut checkpoint = checkpoint(u64::MAX);
    checkpoint.next_replica = u64::MAX - 1;
    checkpoint.owners.insert(ReplicaId(1), "existing".into());
    let mut server = Sequencer::restore_with_tail(checkpoint, vec![], NoMaterializer).unwrap();
    let last = server.try_join_as(Some("last".into())).unwrap();
    assert_eq!(last.replica, ReplicaId(u64::MAX - 1));
    let before = server.checkpoint();
    for owner in [None, Some("another".into())] {
        assert_eq!(
            server.try_join_as(owner),
            Err(jade_collab_sync::SyncError::ReplicaIdExhausted)
        );
        assert_eq!(server.checkpoint(), before);
    }
    assert!(server
        .resume_as(last.replica, u64::MAX, Some("last"))
        .is_ok());
    assert!(server
        .resume_as(ReplicaId(1), u64::MAX, Some("existing"))
        .is_ok());
    assert_eq!(
        server.replay(Record::Joined {
            replica: ReplicaId(u64::MAX)
        }),
        Err(jade_collab_sync::SyncError::ReplicaIdExhausted)
    );
    assert_eq!(server.checkpoint(), before);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(128))]
    #[test]
    fn joins_replays_compaction_and_restarts_match_bounded_counters(
        start_log in proptest::num::u8::ANY,
        start_id in 0u8..12,
        steps in proptest::collection::vec((0u8..9, proptest::num::u8::ANY), 1..140),
    ) {
        use proptest::prelude::*;
        let checkpoint = Checkpoint { next_replica: u64::MAX - u64::from(start_id), ..checkpoint(u64::MAX - u64::from(start_log)) };
        let (mut seq, mut next, mut watermark, mut count) = (u128::from(checkpoint.seq), u128::from(checkpoint.next_replica), 4u64, 0u64);
        let limit = u128::from(u64::MAX);
        let mut server = Sequencer::restore_with_tail(checkpoint, vec![], NoMaterializer).unwrap();
        let mut owners = BTreeMap::new();
        for (kind, choice) in steps {
            match kind {
                0 | 1 => {
                    let owner = (kind == 1).then(|| format!("owner-{next}"));
                    let before = server.checkpoint();
                    let result = server.try_join_as(owner.clone());
                    if next == limit {
                        prop_assert_eq!(result, Err(jade_collab_sync::SyncError::ReplicaIdExhausted));
                        prop_assert_eq!(server.checkpoint(), before);
                    } else {
                        let joined = result.unwrap();
                        prop_assert_eq!(u128::from(joined.replica.0), next);
                        prop_assert_eq!(u128::from(joined.seq), seq);
                        owners.insert(joined.replica, owner); next += 1;
                    }
                }
                2 if seq < limit => {
                    watermark += u64::from(choice) + 1;
                    server.replay(Record::Committed(committed((seq + 1) as u64, watermark))).unwrap();
                    seq += 1; count += 1;
                }
                3 => {
                    watermark += u64::from(choice) + 1;
                    server.replay(Record::Rejected { replica: ReplicaId(1), client_seq: watermark }).unwrap();
                }
                4 | 5 => {
                    let before = server.checkpoint();
                    let ticket = u64::from(choice).min(watermark);
                    let record = if kind == 4 { Record::Rejected { replica: ReplicaId(1), client_seq: ticket } }
                        else { Record::Committed(committed((seq as u64).saturating_add(1), ticket)) };
                    prop_assert!(server.replay(record).is_err());
                    prop_assert_eq!(server.checkpoint(), before);
                }
                6 => { server.compact_keeping(usize::from(choice % 8)); }
                7 => {
                    let checkpoint = server.checkpoint();
                    server = Sequencer::restore_with_tail(checkpoint.clone(), server.log().to_vec(), NoMaterializer).unwrap();
                    prop_assert_eq!(server.checkpoint(), checkpoint);
                }
                _ => {
                    let response = server.resume(ReplicaId(1), seq as u64).unwrap();
                    prop_assert_eq!(response.processed_client_seq, watermark);
                    prop_assert!(response.missed.is_empty());
                    for (replica, owner) in &owners {
                        prop_assert!(server.resume_as(*replica, seq as u64, owner.as_deref()).is_ok());
                        prop_assert!(server.resume_as(*replica, seq as u64, Some("wrong")).is_err());
                    }
                }
            }
            prop_assert_eq!(u128::from(server.seq()), seq);
            prop_assert_eq!(u128::from(server.checkpoint().next_replica), next);
            prop_assert_eq!(server.processed_client_seq(ReplicaId(1)), watermark);
            prop_assert_eq!(server.document().0, count);
        }
    }
}
