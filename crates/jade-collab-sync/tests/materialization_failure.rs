//! Unexpected native failures can leave partial writes. They must not become
//! acknowledgements, durable rejections or replayable committed records.
use jade_collab_core::id::ReplicaId;
use jade_collab_sync::protocol::{Committed, Submission};
use jade_collab_sync::{
    ApplyContext, Document, Lane, Materializer, Record, Rejection, Sequencer, SubmitOutcome,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Clone, Debug, Default, PartialEq)]
struct Model(Vec<u64>);
#[derive(Clone, Debug, PartialEq)]
struct Add {
    value: u64,
    sequenced: bool,
    staged: bool,
}
impl Document for Model {
    type Op = Add;
    type Effect = ();
    fn lane(op: &Add) -> Lane {
        if op.sequenced {
            Lane::Sequenced
        } else {
            Lane::Convergent
        }
    }
    fn max_lamport(&self) -> u64 {
        0
    }
    fn apply(&mut self, op: &Add, _: &ApplyContext) -> Result<(), Rejection> {
        self.0.push(op.value);
        self.0.sort_unstable();
        Ok(())
    }
}
#[derive(Clone, Debug, Default)]
struct File {
    data: Vec<u64>,
    calls: usize,
    fail_at: Option<usize>,
    partial: bool,
    forks: Arc<AtomicUsize>,
}
impl Materializer<Model> for File {
    fn may_derive(&self, _: &Model, op: &Add) -> bool {
        op.staged
    }
    fn fork(&self, _: &Model) -> Result<Self, String> {
        self.forks.fetch_add(1, Ordering::Relaxed);
        Ok(self.clone())
    }
    fn materialize(&mut self, model: &Model, _: (), _: &ApplyContext) -> Result<(), String> {
        self.calls += 1;
        if self.fail_at == Some(self.calls) {
            if self.partial {
                self.data.push(999);
            }
            return Err("injected native failure".into());
        }
        self.data = model.0.clone();
        Ok(())
    }
}
fn op(value: u64, sequenced: bool) -> Add {
    Add {
        value,
        sequenced,
        staged: false,
    }
}
#[test]
fn an_unexpected_fast_path_failure_has_no_publishable_or_journalable_verdict() {
    for sequenced in [false, true] {
        for partial in [false, true] {
            let mut seq = Sequencer::with_materializer(
                Model::default(),
                File {
                    fail_at: Some(2),
                    partial,
                    ..Default::default()
                },
            );
            let id = seq.join().replica;
            let forks = seq.materializer().forks.clone();
            assert!(matches!(
                seq.submit(
                    id,
                    Submission {
                        client_seq: 1,
                        base_seq: 0,
                        op: op(1, sequenced)
                    }
                ),
                SubmitOutcome::Committed(_)
            ));
            let outcome = seq.submit(
                id,
                Submission {
                    client_seq: 2,
                    base_seq: 1,
                    op: op(2, sequenced),
                },
            );
            assert!(
                outcome.record(id).is_none(),
                "native failure must not manufacture a durable verdict: {outcome:?}"
            );
            assert!(
                outcome.downstream().is_none(),
                "native failure must leave the client outcome unknown"
            );
            assert_eq!(
                seq.seq(),
                1,
                "the last successful committed position remains usable for recovery"
            );
            assert_eq!(seq.log().len(), 1);
            assert_eq!(seq.processed_client_seq(id), 1);
            assert!(matches!(outcome, SubmitOutcome::Failed(_)));
            assert!(seq.try_document().is_err());
            assert!(seq.try_materializer().is_err());
            assert!(seq.try_materializer_mut().is_err());
            assert!(seq.try_checkpoint().is_err());
            assert!(seq.try_join().is_err());
            assert!(seq.resume(id, 1).is_err());
            let first_failure = seq.failure().cloned().unwrap();
            assert!(matches!(
                seq.submit(
                    id,
                    Submission {
                        client_seq: 3,
                        base_seq: 1,
                        op: op(3, sequenced)
                    }
                ),
                SubmitOutcome::Failed(_)
            ));
            assert!(matches!(
                seq.reject(id, 4, Rejection::new("cannot answer after failure")),
                SubmitOutcome::Failed(_)
            ));
            assert!(seq.commit_origin(op(3, false)).is_err());
            assert!(seq
                .replay(Record::Rejected {
                    replica: id,
                    client_seq: 5
                })
                .is_err());
            assert_eq!(seq.failure(), Some(&first_failure));
            assert_eq!(seq.faults().len(), 1);
            assert_eq!(seq.processed_client_seq(id), 1);
            assert_eq!(seq.seq(), 1);
            assert_eq!(
                forks.load(Ordering::Relaxed),
                0,
                "ordinary writes never clone native files"
            );
        }
    }
}
#[test]
fn replay_fails_instead_of_claiming_a_native_failure_was_restored() {
    let mut seq = Sequencer::with_materializer(
        Model::default(),
        File {
            fail_at: Some(1),
            partial: true,
            ..Default::default()
        },
    );
    let id = seq.join().replica;
    let result = seq.replay(Record::Committed(Committed {
        seq: 1,
        origin: id,
        client_seq: 1,
        base_seq: 0,
        op: op(1, false),
    }));
    assert!(
        result.is_err(),
        "startup must not serve a model ahead of its native file"
    );
    assert_eq!(seq.seq(), 0);
    assert_eq!(seq.processed_client_seq(id), 0);
}
#[test]
fn origin_write_failure_cannot_create_a_commit_for_the_journal() {
    let mut seq = Sequencer::with_materializer(
        Model::default(),
        File {
            fail_at: Some(1),
            partial: true,
            ..Default::default()
        },
    );
    assert!(seq.commit_origin(op(1, false)).is_err());
    assert_eq!(seq.seq(), 0);
    assert!(seq.log().is_empty());
}

#[test]
fn native_failure_on_an_independent_stage_remains_an_atomic_refusal_and_live_edits_continue() {
    let mut seq = Sequencer::with_materializer(
        Model::default(),
        File {
            fail_at: Some(1),
            partial: true,
            ..Default::default()
        },
    );
    let id = seq.join().replica;
    let before = seq.checkpoint();
    let outcome = seq.submit(
        id,
        Submission {
            client_seq: 1,
            base_seq: 0,
            op: Add {
                value: 1,
                sequenced: true,
                staged: true,
            },
        },
    );
    assert!(matches!(outcome, SubmitOutcome::Rejected(_)));
    assert!(outcome.record(id).is_some());
    assert!(seq.failure().is_none());
    assert!(seq.faults().is_empty());
    assert_eq!(seq.document(), &before.document);
    assert!(seq.materializer().data.is_empty());
    assert_eq!(seq.materializer().calls, 0);
    assert_eq!(seq.processed_client_seq(id), 1);
    seq.materializer_mut().fail_at = None;
    assert!(matches!(
        seq.submit(
            id,
            Submission {
                client_seq: 2,
                base_seq: 0,
                op: op(2, false)
            }
        ),
        SubmitOutcome::Committed(_)
    ));
    assert_eq!(seq.materializer().data, vec![2]);
    assert_eq!(seq.document().0, vec![2]);
}
#[test]
fn checkpoints_with_old_native_faults_are_refused_by_checked_restore() {
    let mut seq = Sequencer::with_materializer(Model::default(), File::default());
    seq.join();
    let mut checkpoint = seq.checkpoint();
    checkpoint.faults.push("an old writer failed".into());
    assert!(Sequencer::restore_with_tail(checkpoint.clone(), vec![], File::default()).is_err());
    let restored = Sequencer::restore(checkpoint, File::default());
    assert!(restored.try_checkpoint().is_err());
    assert!(restored.try_document().is_err());
}
#[test]
fn a_failed_simulation_returns_an_error_and_keeps_its_clients_unanswered_submission() {
    let mut sim = jade_collab_sync::sim::Simulation::with_materializer(
        Model::default(),
        File {
            fail_at: Some(1),
            partial: true,
            ..Default::default()
        },
        1,
    );
    sim.local(0, op(1, false)).unwrap();
    assert!(sim.quiesce().is_err());
    assert_eq!(sim.client(0).pending_len(), 1);
    assert_eq!(sim.server().seq(), 0);
    assert!(sim.server().log().is_empty());
    assert_eq!(sim.server().processed_client_seq(ReplicaId(1)), 0);
}

use proptest::prelude::*;
proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    #[test]
    fn stateful_failures_staging_checkpoints_replay_and_exact_ticket_retry_match_the_reference(
        steps in prop::collection::vec((0u8..6,any::<u16>(),any::<bool>()),1..100),
    ) {
        let mut seq=Sequencer::with_materializer(Model::default(),File::default());
        let id=seq.join().replica;
        let mut checkpoint=seq.checkpoint();let mut tail=vec![];let mut journal=vec![];
        let mut expected=Vec::new();let mut ticket=0;let mut commits=0;let mut last:Option<Submission<Add>>=None;
        for (kind,value,sequenced) in steps {
            if kind==4 {
                checkpoint=seq.checkpoint();seq.compact_keeping(2);tail=seq.log().to_vec();journal.clear();
            } else if kind==5 {
                if let Some(mut repeated)=last.clone() {
                    let before=seq.checkpoint();repeated.op.value=999_999;
                    prop_assert!(matches!(seq.submit(id,repeated),SubmitOutcome::Duplicate));
                    prop_assert_eq!(seq.checkpoint(),before);
                }
            } else {
                ticket+=1;
                let submission=Submission {client_seq:ticket,base_seq:seq.seq(),op:Add {value:u64::from(value),sequenced,staged:kind==3}};
                last=Some(submission.clone());
                let fail=matches!(kind,1..=3);
                let before=seq.checkpoint();
                if fail {
                    let file=seq.materializer_mut();file.fail_at=Some(file.calls+1);file.partial=kind!=1;
                }
                let outcome=seq.submit(id,submission.clone());
                if matches!(kind,1|2) {
                    prop_assert!(matches!(outcome,SubmitOutcome::Failed(_)));
                    prop_assert!(outcome.record(id).is_none());prop_assert!(outcome.downstream().is_none());
                    prop_assert_eq!(seq.seq(),before.seq);
                    prop_assert_eq!(seq.processed_client_seq(id),before.processed[&id]);
                    prop_assert!(seq.try_document().is_err());prop_assert!(seq.try_checkpoint().is_err());
                    let native=File {data:checkpoint.document.0.clone(),..Default::default()};
                    seq=Sequencer::restore_with_tail(checkpoint.clone(),tail.clone(),native).unwrap();
                    for record in journal.iter().cloned() {seq.replay(record).unwrap();}
                    prop_assert_eq!(seq.document(),&before.document,"failure was absent from durable history");
                    let retried=seq.submit(id,submission);
                    prop_assert!(matches!(retried,SubmitOutcome::Committed(_)));
                    journal.push(retried.record(id).unwrap());expected.push(u64::from(value));commits+=1;
                } else if kind==3 {
                    prop_assert!(matches!(outcome,SubmitOutcome::Rejected(_)));
                    prop_assert_eq!(seq.document(),&before.document);
                    prop_assert_eq!(seq.materializer().data.as_slice(),before.document.0.as_slice());
                    journal.push(outcome.record(id).unwrap());seq.materializer_mut().fail_at=None;
                } else {
                    prop_assert!(matches!(outcome,SubmitOutcome::Committed(_)));
                    journal.push(outcome.record(id).unwrap());expected.push(u64::from(value));commits+=1;
                }
            }
            expected.sort_unstable();
            prop_assert_eq!(seq.document().0.as_slice(),expected.as_slice());
            prop_assert_eq!(seq.materializer().data.as_slice(),expected.as_slice());
            prop_assert_eq!(seq.seq(),commits);prop_assert_eq!(seq.processed_client_seq(id),ticket);
            prop_assert!(seq.failure().is_none());prop_assert!(seq.faults().is_empty());
        }
        let mut recovered=Sequencer::restore_with_tail(checkpoint.clone(),tail,File {data:checkpoint.document.0.clone(),..Default::default()}).unwrap();
        for record in journal {recovered.replay(record).unwrap();}
        prop_assert_eq!(recovered.checkpoint(),seq.checkpoint());
        prop_assert_eq!(recovered.materializer().data.as_slice(),expected.as_slice());
    }
}
