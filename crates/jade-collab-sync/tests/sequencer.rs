mod common;

use common::{origin, ToyDoc, ToyOp};
use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::protocol::{Downstream, Submission};
use jade_collab_sync::{Sequencer, SubmitOutcome};

fn insert(lamport: u64, replica: ReplicaId, parent: Option<Id>, ch: char) -> ToyOp {
    ToyOp::Insert {
        id: Id::new(lamport, replica),
        parent,
        ch,
    }
}

#[test]
fn joining_hands_out_fresh_replicas_and_the_current_snapshot() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join();
    let b = server.join();
    assert_ne!(a.replica, b.replica);
    assert_ne!(a.replica, ReplicaId::ORIGIN);
    assert_eq!(a.seq, 0);
    assert_eq!(a.snapshot, ToyDoc::sample());
}

#[test]
fn commits_in_arrival_order_and_numbers_the_log() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let outcome = server.submit(
        a,
        Submission {
            client_seq: 1,
            base_seq: 0,
            op: insert(5, a, Some(origin(2)), 'c'),
        },
    );
    let SubmitOutcome::Committed(committed) = outcome else {
        panic!("expected a commit, got {outcome:?}");
    };
    assert_eq!(committed.seq, 1);
    assert_eq!(committed.origin, a);
    assert_eq!(committed.client_seq, 1);
    assert_eq!(server.seq(), 1);
    assert_eq!(server.document().string(), "abc");
}

#[test]
fn rejects_a_sequenced_op_that_conflicts_with_an_earlier_commit() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let b = server.join().replica;
    let whole = Merge {
        top: origin(10),
        left: origin(20),
        bottom: origin(11),
        right: origin(21),
    };
    let row = Merge {
        top: origin(10),
        left: origin(20),
        bottom: origin(10),
        right: origin(21),
    };
    assert!(matches!(
        server.submit(
            a,
            Submission {
                client_seq: 1,
                base_seq: 0,
                op: ToyOp::Grid(GridOp::Merge(whole)),
            }
        ),
        SubmitOutcome::Committed(_)
    ));
    let outcome = server.submit(
        b,
        Submission {
            client_seq: 1,
            base_seq: 0,
            op: ToyOp::Grid(GridOp::Merge(row)),
        },
    );
    let SubmitOutcome::Rejected(rejected) = outcome else {
        panic!("expected a rejection, got {outcome:?}");
    };
    assert_eq!(rejected.client_seq, 1);
    assert!(!rejected.reason.message().is_empty());
    assert_eq!(server.seq(), 1, "a rejection is not logged");
    assert_eq!(server.document().grid.merges(), &[whole]);
}

#[test]
fn drops_resubmitted_operations() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let submission = Submission {
        client_seq: 1,
        base_seq: 0,
        op: insert(5, a, None, 'x'),
    };
    assert!(matches!(
        server.submit(a, submission.clone()),
        SubmitOutcome::Committed(_)
    ));
    assert_eq!(server.submit(a, submission), SubmitOutcome::Duplicate);
    assert_eq!(server.document().string(), "xab");
    assert_eq!(server.processed_client_seq(a), 1);
}

#[test]
fn rejects_submissions_from_strangers() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let outcome = server.submit(
        ReplicaId(99),
        Submission {
            client_seq: 1,
            base_seq: 0,
            op: insert(5, ReplicaId(99), None, 'x'),
        },
    );
    assert!(matches!(outcome, SubmitOutcome::Rejected(_)));
    assert_eq!(server.seq(), 0);
}

#[test]
fn resuming_returns_what_was_missed() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let b = server.join().replica;
    for (seq, ch) in [(1, 'x'), (2, 'y')] {
        server.submit(
            b,
            Submission {
                client_seq: seq,
                base_seq: 0,
                op: insert(10 + seq, b, None, ch),
            },
        );
    }
    let resume = server.resume(a, 1).unwrap();
    assert_eq!(resume.processed_client_seq, 0);
    assert_eq!(resume.missed.len(), 1);
    assert_eq!(resume.missed[0].seq, 2);
    assert!(server.resume(ReplicaId(42), 0).is_err());
    assert!(
        server.resume(a, 3).is_err(),
        "cannot resume from the future"
    );
}

#[test]
fn every_outcome_has_a_downstream_message() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let outcome = server.submit(
        a,
        Submission {
            client_seq: 1,
            base_seq: 0,
            op: insert(5, a, None, 'x'),
        },
    );
    let message = outcome.downstream().expect("commits are broadcast");
    let json = serde_json::to_string(&message).unwrap();
    let back: Downstream<ToyOp> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, message);
}

/// A materializer that refuses every merge — standing in for a real document
/// that cannot realise something the model would accept.
struct NoMerges;

impl jade_collab_sync::Materializer<ToyDoc> for NoMerges {
    fn admit(&self, _: &ToyDoc, op: &ToyOp) -> Result<(), jade_collab_sync::Rejection> {
        match op {
            ToyOp::Grid(GridOp::Merge(_)) => {
                Err(jade_collab_sync::Rejection::new("no merges here"))
            }
            _ => Ok(()),
        }
    }

    fn materialize(
        &mut self,
        _: &ToyDoc,
        _: (),
        _: &jade_collab_sync::ApplyContext,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn the_materializer_can_refuse_sequenced_ops_before_they_apply() {
    let mut server = Sequencer::with_materializer(ToyDoc::sample(), NoMerges);
    let a = server.join().replica;
    let outcome = server.submit(
        a,
        Submission {
            client_seq: 1,
            base_seq: 0,
            op: ToyOp::Grid(GridOp::Merge(Merge {
                top: origin(10),
                left: origin(20),
                bottom: origin(11),
                right: origin(20),
            })),
        },
    );
    let SubmitOutcome::Rejected(rejected) = outcome else {
        panic!("expected a rejection, got {outcome:?}");
    };
    assert_eq!(rejected.reason.message(), "no merges here");
    assert!(server.document().grid.merges().is_empty());
    assert_eq!(server.seq(), 0);
}
