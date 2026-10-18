//! Restoring a sequencer from a checkpoint and the journal of what happened
//! after it: a server that restarts must be indistinguishable from one that
//! did not.

mod common;

use common::{generate, origin, Mirror, ToyDoc, ToyOp};
use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{Checkpoint, NoMaterializer, Record, Replica, Sequencer, SyncError};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

fn insert(lamport: u64, replica: ReplicaId, parent: Option<Id>, ch: char) -> ToyOp {
    ToyOp::Insert {
        id: Id::new(lamport, replica),
        parent,
        ch,
    }
}

fn whole_table() -> ToyOp {
    ToyOp::Grid(GridOp::Merge(Merge {
        top: origin(10),
        left: origin(20),
        bottom: origin(11),
        right: origin(21),
    }))
}

/// Submits and returns what must be journaled.
fn submit(
    server: &mut Sequencer<ToyDoc>,
    from: ReplicaId,
    client_seq: u64,
    op: ToyOp,
) -> Option<Record<ToyOp>> {
    server
        .submit(
            from,
            Submission {
                client_seq,
                base_seq: server.seq(),
                op,
            },
        )
        .record(from)
}

/// Through JSON, as on disk.
fn through_json<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

#[test]
fn a_checkpoint_restores_the_document_and_the_bookkeeping() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let b = server.join().replica;
    submit(&mut server, a, 1, insert(5, a, Some(origin(2)), 'c'));
    submit(&mut server, b, 1, whole_table());
    submit(&mut server, a, 2, whole_table()); // rejected: overlaps

    let checkpoint = through_json(&server.checkpoint());
    assert_eq!(checkpoint.seq, 2);
    assert_eq!(&checkpoint.document, server.document());
    let mut restored = Sequencer::restore(checkpoint, NoMaterializer);
    assert_eq!(restored.seq(), 2);
    assert_eq!(restored.document(), server.document());
    assert_eq!(restored.processed_client_seq(a), 2);
    assert_eq!(restored.processed_client_seq(b), 1);
    assert_eq!(restored.checkpoint(), server.checkpoint());
    // New replicas keep getting fresh ids.
    assert_eq!(restored.join().replica, server.join().replica);
}

#[test]
fn replaying_the_journal_after_a_checkpoint_catches_up() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    submit(&mut server, a, 1, insert(5, a, None, 'x'));
    let checkpoint = server.checkpoint();

    let mut journal = Vec::new();
    let b = server.join().replica;
    journal.push(Record::Joined { replica: b });
    journal.extend(submit(&mut server, b, 1, whole_table()));
    journal.extend(submit(&mut server, a, 2, whole_table())); // rejected
    journal.extend(submit(&mut server, a, 3, insert(9, a, None, 'y')));
    // A duplicate changes nothing and leaves nothing to journal.
    assert_eq!(submit(&mut server, a, 3, insert(9, a, None, 'y')), None);
    assert_eq!(journal.len(), 4);
    assert!(matches!(
        journal[2],
        Record::Rejected {
            replica,
            client_seq: 2
        } if replica == a
    ));

    let mut restored = Sequencer::restore(checkpoint, NoMaterializer);
    for record in through_json(&journal) {
        restored.replay(record).unwrap();
    }
    assert_eq!(restored.checkpoint(), server.checkpoint());
    assert_eq!(restored.log(), &server.log()[1..]);
    for replica in [a, b] {
        assert_eq!(restored.resume(replica, 1), server.resume(replica, 1));
    }
}

#[test]
fn replay_refuses_a_journal_that_does_not_fit() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let commit = submit(&mut server, a, 1, insert(5, a, None, 'x')).unwrap();

    let mut restored = Sequencer::restore(
        Sequencer::new(ToyDoc::sample()).checkpoint(),
        NoMaterializer,
    );
    // `a` has not joined yet in the restored sequencer.
    assert_eq!(
        restored.replay(commit.clone()),
        Err(SyncError::UnknownReplica(a))
    );
    assert!(matches!(
        restored.replay(Record::Joined {
            replica: ReplicaId(7)
        }),
        Err(SyncError::Journal { .. })
    ));
    restored.replay(Record::Joined { replica: a }).unwrap();
    restored.replay(commit.clone()).unwrap();
    assert!(matches!(
        restored.replay(commit),
        Err(SyncError::OutOfOrder {
            expected: 2,
            received: 1
        })
    ));
    assert_eq!(restored.document(), server.document());
}

#[test]
fn resuming_from_before_a_compacted_log_asks_the_replica_to_join_again() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let b = server.join().replica;
    submit(&mut server, b, 1, insert(5, b, None, 'x'));
    submit(&mut server, b, 2, insert(6, b, None, 'y'));
    server.compact();
    assert_eq!(server.log_start(), 2);
    assert!(server.log().is_empty());
    assert_eq!(server.seq(), 2);

    let error = server.resume(a, 1).unwrap_err();
    assert_eq!(
        error,
        SyncError::Compacted {
            requested: 1,
            oldest: 2
        }
    );
    assert!(error.to_string().contains("join again"), "{error}");
    // From the checkpoint on, resuming still works.
    assert_eq!(server.resume(a, 2).unwrap().missed, vec![]);
    submit(&mut server, b, 3, insert(7, b, None, 'z'));
    assert_eq!(server.resume(a, 2).unwrap().missed.len(), 1);
}

/// A server that restarts from its checkpoint and journal, compared with one
/// that did not restart but whose clients reconnected at the same moment.
#[derive(Clone, Debug)]
enum Step {
    Local { client: usize, choice: u64 },
    Up { client: usize },
    Down { client: usize },
    Checkpoint,
    Restart,
}

fn steps(clients: usize) -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            6 => (0..clients, any::<u64>()).prop_map(|(client, choice)| Step::Local { client, choice }),
            4 => (0..clients).prop_map(|client| Step::Up { client }),
            4 => (0..clients).prop_map(|client| Step::Down { client }),
            1 => Just(Step::Checkpoint),
            1 => Just(Step::Restart),
        ],
        0..150,
    )
}

/// What is on disk: the checkpoint and the materializer's file at it, and
/// the journal since.
struct Disk {
    checkpoint: String,
    file: String,
}

fn restore(disk: &Disk, journal: &[Record<ToyOp>]) -> Sequencer<ToyDoc, Mirror> {
    let checkpoint: Checkpoint<ToyDoc> = serde_json::from_str(&disk.checkpoint).unwrap();
    let mut server = Sequencer::restore(checkpoint, Mirror::load(&disk.file));
    for record in journal {
        let line = serde_json::to_string(record).unwrap();
        server.replay(serde_json::from_str(&line).unwrap()).unwrap();
    }
    server
}

/// Equal state, and equal answers to every replica (and a stranger) resuming
/// from anywhere that matters.
fn same_server(
    a: &Sequencer<ToyDoc, Mirror>,
    b: &Sequencer<ToyDoc, Mirror>,
) -> Result<(), TestCaseError> {
    prop_assert_eq!(a.checkpoint(), b.checkpoint());
    prop_assert_eq!(a.faults(), b.faults());
    prop_assert_eq!(a.materializer(), b.materializer());
    prop_assert_eq!(a.log_start(), b.log_start());
    prop_assert_eq!(a.log(), b.log());
    let edges = [
        0,
        a.log_start().saturating_sub(1),
        a.log_start(),
        a.seq(),
        a.seq() + 1,
    ];
    for replica in (0..a.checkpoint().next_replica + 1).map(ReplicaId) {
        for seq in edges {
            prop_assert_eq!(a.resume(replica, seq), b.resume(replica, seq));
        }
    }
    Ok(())
}

fn same_client(a: &Replica<ToyDoc>, b: &Replica<ToyDoc>) -> Result<(), TestCaseError> {
    prop_assert_eq!(a.id(), b.id());
    prop_assert_eq!(a.seq(), b.seq());
    prop_assert_eq!(a.view(), b.view());
    prop_assert_eq!(a.confirmed(), b.confirmed());
    prop_assert_eq!(a.pending_submissions(), b.pending_submissions());
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn a_restarted_server_is_indistinguishable_from_one_that_kept_running(steps in steps(3)) {
        let sample = ToyDoc::sample();
        let new_sim = || Simulation::with_materializer(sample.clone(), Mirror::load(&sample.string()), 3);
        let mut restarted = new_sim();
        let mut running = new_sim();
        // Before the first checkpoint, the disk holds the document as opened.
        let mut disk = Disk {
            checkpoint: serde_json::to_string(&Sequencer::new(sample.clone()).checkpoint()).unwrap(),
            file: sample.string(),
        };
        for step in &steps {
            match *step {
                Step::Local { client, choice } => {
                    for sim in [&mut restarted, &mut running] {
                        let op = generate(sim.client_mut(client), choice);
                        let _ = sim.local(client, op);
                    }
                }
                Step::Up { client } => {
                    restarted.deliver_up(client);
                    running.deliver_up(client);
                }
                Step::Down { client } => {
                    restarted.deliver_down(client).unwrap();
                    running.deliver_down(client).unwrap();
                }
                Step::Checkpoint => {
                    let checkpoint = restarted.checkpoint();
                    disk = Disk {
                        checkpoint: serde_json::to_string(&checkpoint).unwrap(),
                        file: restarted.server().materializer().export(),
                    };
                    prop_assert!(restarted.journal().is_empty());
                    running.checkpoint();
                }
                Step::Restart => {
                    let server = restore(&disk, restarted.journal());
                    restarted.restart(server).unwrap();
                    running.reconnect().unwrap();
                }
            }
            same_server(restarted.server(), running.server())?;
            for client in 0..3 {
                same_client(restarted.client(client), running.client(client))?;
            }
            // The journal always restores the server as it is now.
            let replayed = restore(&disk, restarted.journal());
            same_server(&replayed, restarted.server())?;
        }
        restarted.quiesce().unwrap();
        running.quiesce().unwrap();
        same_server(restarted.server(), running.server())?;
        for client in 0..3 {
            let replica = restarted.client(client);
            same_client(replica, running.client(client))?;
            prop_assert_eq!(replica.pending_len(), 0);
            prop_assert_eq!(replica.view(), restarted.server().document());
        }
    }
}

#[test]
fn a_replica_behind_the_checkpoint_rejoins_on_reconnect() {
    let mut sim = Simulation::new(ToyDoc::sample(), 2);
    let behind = sim.client(0).id();
    let id = sim.client_mut(1).next_id();
    sim.local(
        1,
        ToyOp::Insert {
            id,
            parent: None,
            ch: 'x',
        },
    )
    .unwrap();
    sim.deliver_up(1);
    sim.deliver_down(1).unwrap();
    sim.checkpoint();
    sim.reconnect().unwrap();
    assert_ne!(sim.client(0).id(), behind, "joined again as a new replica");
    assert_eq!(sim.client(0).view(), sim.server().document());
    assert_eq!(
        sim.journal(),
        &[Record::Joined {
            replica: sim.client(0).id()
        }]
    );
}
