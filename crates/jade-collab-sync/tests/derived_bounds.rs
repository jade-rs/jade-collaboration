use jade_collab_core::id::ReplicaId;
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::{
    ApplyContext, Document, Lane, Materializer, Rejection, Sequencer, SubmitOutcome,
};

#[derive(Clone, Debug, Default, PartialEq)]
struct Model(Vec<u64>);
#[derive(Clone, Debug, PartialEq)]
enum Op {
    Add {
        value: u64,
        children: u8,
        invalid: bool,
    },
    Child(u64),
    Invalid,
}
impl Document for Model {
    type Op = Op;
    type Effect = ();
    fn lane(_: &Op) -> Lane {
        Lane::Sequenced
    }
    fn max_lamport(&self) -> u64 {
        0
    }
    fn apply(&mut self, op: &Op, _: &ApplyContext) -> Result<(), Rejection> {
        match op {
            Op::Add { value, .. } | Op::Child(value) => self.0.push(*value),
            Op::Invalid => return Err(Rejection::new("invalid child")),
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
struct File {
    model: Model,
    calls: usize,
    fail_child: bool,
    fail_fork: bool,
}
impl Materializer<Model> for File {
    fn may_derive(&self, _: &Model, cause: &Op) -> bool {
        matches!(cause, Op::Add { children, .. } if *children > 0)
    }
    fn fork(&self, _: &Model) -> Result<Self, String> {
        if self.fail_fork {
            Err("injected fork failure".into())
        } else {
            Ok(self.clone())
        }
    }

    fn derived(&mut self, _: &Model, cause: &Op) -> Result<Vec<Op>, Rejection> {
        Ok(match cause {
            Op::Add {
                value,
                children,
                invalid,
            } => (1..=u64::from(*children))
                .map(|i| {
                    if *invalid && i == u64::from(*children) {
                        Op::Invalid
                    } else {
                        Op::Child(value + i)
                    }
                })
                .collect(),
            _ => vec![],
        })
    }
    fn materialize(&mut self, model: &Model, _: (), _: &ApplyContext) -> Result<(), String> {
        self.calls += 1;
        self.model = model.clone();
        if self.fail_child && self.calls == 2 {
            return Err("injected child write failure".into());
        }
        Ok(())
    }
}
fn server(seq: u64, file: File) -> Sequencer<Model, File> {
    let mut initial = Sequencer::with_materializer(Model::default(), file.clone());
    initial.join();
    let mut checkpoint = initial.checkpoint();
    checkpoint.seq = seq;
    Sequencer::restore(checkpoint, file)
}
fn add(
    server: &mut Sequencer<Model, File>,
    ticket: u64,
    children: u8,
    invalid: bool,
) -> SubmitOutcome<Op> {
    server.submit(
        ReplicaId(1),
        Submission {
            client_seq: ticket,
            base_seq: server.seq(),
            op: Op::Add {
                value: 10,
                children,
                invalid,
            },
        },
    )
}

#[test]
fn insufficient_log_space_refuses_the_whole_cause_and_derived_suffix() {
    let mut server = server(u64::MAX - 1, File::default());
    let before = server.checkpoint();
    let file = server.materializer().clone();
    assert!(matches!(
        add(&mut server, 1, 1, false),
        SubmitOutcome::Rejected(_)
    ));
    assert_eq!(server.document(), &before.document);
    assert_eq!(server.materializer(), &file);
    assert_eq!(server.seq(), before.seq);
    assert_eq!(server.processed_client_seq(ReplicaId(1)), 1);
    assert!(server.take_derived().is_empty());
    // The remaining position is still usable by an operation without children.
    assert!(matches!(
        add(&mut server, 2, 0, false),
        SubmitOutcome::Committed(_)
    ));
    assert_eq!(server.seq(), u64::MAX);
}

#[test]
fn exact_capacity_commits_every_child_through_the_last_position() {
    let mut server = server(u64::MAX - 3, File::default());
    assert!(matches!(
        add(&mut server, 1, 2, false),
        SubmitOutcome::Committed(_)
    ));
    assert_eq!(server.seq(), u64::MAX);
    assert_eq!(server.document().0, vec![10, 11, 12]);
    assert_eq!(server.materializer().model, *server.document());
    assert_eq!(
        server
            .take_derived()
            .iter()
            .map(|c| c.seq)
            .collect::<Vec<_>>(),
        vec![u64::MAX - 1, u64::MAX]
    );
}

#[test]
fn invalid_or_unmaterializable_children_leave_the_whole_file_and_model_unchanged() {
    for invalid in [false, true] {
        let mut server = server(
            0,
            File {
                fail_child: !invalid,
                ..File::default()
            },
        );
        let before = server.checkpoint();
        let file = server.materializer().clone();
        assert!(matches!(
            add(&mut server, 1, 1, invalid),
            SubmitOutcome::Rejected(_)
        ));
        assert_eq!(server.document(), &before.document);
        assert_eq!(server.materializer(), &file);
        assert_eq!(server.seq(), 0);
        assert!(
            server.faults().is_empty(),
            "an unpublished failed plan did not damage the live file"
        );
        assert!(server.take_derived().is_empty());
    }
}

#[test]
fn a_failed_materializer_fork_refuses_before_touching_the_live_file() {
    let mut server = server(
        0,
        File {
            fail_fork: true,
            ..File::default()
        },
    );
    let file = server.materializer().clone();
    assert!(matches!(
        add(&mut server, 1, 1, false),
        SubmitOutcome::Rejected(_)
    ));
    assert_eq!(server.materializer(), &file);
    assert!(server.document().0.is_empty());
    assert!(
        matches!(add(&mut server, 2, 0, false), SubmitOutcome::Committed(_)),
        "non-deriving edits never call fork"
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(128))]
    #[test]
    fn staged_causes_match_atomic_batches_through_bounds_refusals_retries_and_recovery(
        remaining in 0u8..30,
        history in proptest::collection::vec((0u8..6, 0u8..5, proptest::bool::ANY), 1..120),
    ) {
        use proptest::prelude::*;
        let mut server = server(u64::MAX - u64::from(remaining), File::default());
        let initial = server.checkpoint();
        let mut journal = Vec::new();
        let mut expected = Vec::new();
        let mut seq = u128::from(initial.seq);
        let mut ticket = 0u64;
        for (kind, children, invalid) in history {
            match kind {
                0..=2 => {
                    ticket += 1;
                    let before_file = server.materializer().clone();
                    let outcome = add(&mut server, ticket, children, invalid);
                    let accepted = (!invalid || children == 0) && seq + 1 + u128::from(children) <= u128::from(u64::MAX);
                    if accepted {
                        prop_assert!(matches!(outcome, SubmitOutcome::Committed(_)));
                        expected.extend((0..=u64::from(children)).map(|i| 10 + i));
                        seq += 1 + u128::from(children);
                    } else {
                        prop_assert!(matches!(outcome, SubmitOutcome::Rejected(_)));
                        prop_assert_eq!(server.materializer(), &before_file);
                    }
                    journal.extend(outcome.record(ReplicaId(1)));
                    let derived = server.take_derived();
                    prop_assert_eq!(derived.len(), if accepted { usize::from(children) } else { 0 });
                    journal.extend(derived.into_iter().map(jade_collab_sync::Record::Committed));
                }
                3 if ticket > 0 => {
                    let before = server.checkpoint();
                    prop_assert_eq!(add(&mut server, ticket, children, invalid), SubmitOutcome::Duplicate);
                    prop_assert_eq!(server.checkpoint(), before);
                }
                4 => {
                    let mut restored = Sequencer::restore(initial.clone(), File::default());
                    for record in &journal { restored.replay(record.clone()).unwrap(); }
                    prop_assert_eq!(restored.checkpoint(), server.checkpoint());
                    prop_assert_eq!(restored.materializer().model.0.as_slice(), expected.as_slice());
                    server = restored;
                }
                _ => {
                    server.compact_keeping(usize::from(children));
                    let checkpoint = server.checkpoint();
                    let file = server.materializer().clone();
                    server = Sequencer::restore_with_tail(checkpoint.clone(), server.log().to_vec(), file).unwrap();
                    prop_assert_eq!(server.checkpoint(), checkpoint);
                }
            }
            prop_assert_eq!(u128::from(server.seq()), seq);
            prop_assert_eq!(server.processed_client_seq(ReplicaId(1)), ticket);
            prop_assert_eq!(server.document().0.as_slice(), expected.as_slice());
            prop_assert_eq!(server.materializer().model.0.as_slice(), expected.as_slice());
            prop_assert!(server.faults().is_empty());
        }
    }
}
