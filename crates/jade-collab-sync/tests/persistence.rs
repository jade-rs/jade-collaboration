//! Restoring a sequencer from a checkpoint and the journal of what happened
//! after it: a server that restarts must be indistinguishable from one that
//! did not.

mod common;

use common::{generate, origin, Mirror, ToyDoc, ToyOp};
use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::protocol::{Committed, Submission};
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
fn legacy_unbound_replicas_cannot_be_claimed_by_an_authenticated_principal() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let replica = server.join().replica;
    let mut old = serde_json::to_value(server.checkpoint()).unwrap();
    old.as_object_mut().unwrap().remove("owners");
    let mut restored: Sequencer<ToyDoc> =
        Sequencer::restore(serde_json::from_value(old).unwrap(), NoMaterializer);
    assert!(restored.resume_as(replica, 0, None).is_ok());
    assert!(matches!(
        restored.resume_as(replica, 0, Some("Ada")),
        Err(SyncError::ReplicaOwnerMismatch { .. })
    ));
    let fresh = restored.join_as(Some("Ada".into())).replica;
    assert_ne!(fresh, replica);
    assert!(restored.resume_as(fresh, 0, Some("Ada")).is_ok());
}

proptest! {
    #[test]
    fn ownership_authorization_survives_join_refusal_replay_checkpoint_and_legacy_histories(
        steps in prop::collection::vec((0u8..6,0usize..8,0u8..4),1..100),
    ) {
        use std::collections::BTreeMap;
        let principal=|n:u8|match n {0=>None,1|3=>Some("Ada".to_string()),_=>Some("Bob".to_string())};
        let mut server=Sequencer::new(ToyDoc::sample());
        let mut base=server.checkpoint();
        let mut journal=Vec::new();
        let mut owners=BTreeMap::new();
        for (kind,slot,who) in steps {
            let caller=principal(who);
            match kind {
                0=>{
                    let joined=server.join_as(caller.clone());
                    let expected=ReplicaId(owners.len() as u64+1);
                    prop_assert_eq!(joined.replica,expected);
                    owners.insert(expected,caller.clone());
                    journal.push(Record::Joined {replica:expected});
                    if let Some(principal)=caller.clone() {journal.push(Record::Owned {replica:expected,principal});}
                }
                1=>{
                    let checkpoint=through_json(&server.checkpoint());
                    server=Sequencer::restore(checkpoint,NoMaterializer);
                }
                2=>{
                    let mut restored=Sequencer::restore(through_json(&base),NoMaterializer);
                    for record in &journal {restored.replay(through_json(record)).unwrap();}
                    prop_assert_eq!(restored.checkpoint(),server.checkpoint());
                    server=restored;
                }
                3=>{
                    let replica=ReplicaId(slot as u64+1);
                    let before=server.checkpoint();
                    if let Some(Some(_))=owners.get(&replica) {
                        prop_assert!(server.replay(Record::Owned {replica,principal:"Mallory".into()}).is_err(),"owners cannot be rebound");
                        prop_assert_eq!(server.checkpoint(),before);
                    }
                }
                4=>{
                    let replica=ReplicaId(slot as u64+1);
                    if owners.contains_key(&replica) {
                        let ticket=server.processed_client_seq(replica)+1;
                        if let Some(record)=server.reject(replica,ticket,jade_collab_sync::Rejection::new("read-only")).record(replica) {journal.push(record);}
                    }
                }
                _=>{
                    let mut old=serde_json::to_value(server.checkpoint()).unwrap();
                    old.as_object_mut().unwrap().remove("owners");
                    server=Sequencer::restore(serde_json::from_value(old).unwrap(),NoMaterializer);
                    for owner in owners.values_mut() {*owner=None;}
                    base=server.checkpoint();journal.clear();
                }
            }
            let before=server.checkpoint();
            for id in 1..=owners.len()+1 {
                for who in 0..4 {
                    let caller=principal(who);
                    let replica=ReplicaId(id as u64);
                    let expected=owners.get(&replica).is_some_and(|owner|*owner==caller);
                    prop_assert_eq!(server.resume_as(replica,0,caller.as_deref()).is_ok(),expected);
                }
            }
            prop_assert_eq!(server.checkpoint(),before,"authorization cannot change document, tickets, owner or sequence");
        }
    }
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

#[test]
fn a_kept_tail_lets_replicas_just_behind_a_checkpoint_resume() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let b = server.join().replica;
    for (client_seq, ch) in ['v', 'w', 'x', 'y'].into_iter().enumerate() {
        let client_seq = client_seq as u64 + 1;
        submit(
            &mut server,
            b,
            client_seq,
            insert(4 + client_seq, b, None, ch),
        );
    }
    let full = server.log().to_vec();
    server.compact_keeping(2);
    assert_eq!(server.log_start(), 2);
    assert_eq!(server.log(), &full[2..]);
    assert_eq!(server.seq(), 4);
    // Within the tail, a resume is what it was before the checkpoint.
    assert_eq!(server.resume(a, 2).unwrap().missed, full[2..].to_vec());
    assert_eq!(server.resume(a, 3).unwrap().missed, full[3..].to_vec());
    assert_eq!(
        server.resume(a, 1),
        Err(SyncError::Compacted {
            requested: 1,
            oldest: 2
        })
    );
    // Keeping more than there is keeps everything there is.
    server.compact_keeping(10);
    assert_eq!(server.log_start(), 2);
    server.compact_keeping(0);
    assert_eq!(server.log_start(), 4);
    assert!(server.log().is_empty());
}

#[test]
fn a_checkpoint_restores_with_its_tail() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    submit(&mut server, a, 1, insert(5, a, None, 'x'));
    submit(&mut server, a, 2, insert(6, a, None, 'y'));
    submit(&mut server, a, 3, insert(7, a, None, 'z'));
    let checkpoint = through_json(&server.checkpoint());
    let full = server.log().to_vec();
    let tail: Vec<Committed<ToyOp>> = through_json(&full[1..].to_vec());
    server.compact_keeping(2);

    let restored =
        Sequencer::restore_with_tail(checkpoint.clone(), tail.clone(), NoMaterializer).unwrap();
    assert_eq!(restored.log_start(), 1);
    assert_eq!(restored.log(), server.log());
    assert_eq!(restored.checkpoint(), server.checkpoint());
    for seq in 0..5 {
        assert_eq!(restored.resume(a, seq), server.resume(a, seq), "from {seq}");
    }

    // A tail must end at the checkpoint, without gaps.
    for bad in [
        tail[..1].to_vec(),
        vec![tail[0].clone(), tail[0].clone()],
        vec![tail[1].clone(), tail[0].clone()],
    ] {
        assert!(matches!(
            Sequencer::restore_with_tail(checkpoint.clone(), bad, NoMaterializer),
            Err(SyncError::Journal { .. })
        ));
    }
    // Nor start before the beginning of the log.
    let mut too_long = full;
    too_long.insert(
        0,
        Committed {
            seq: 0,
            ..tail[0].clone()
        },
    );
    assert!(Sequencer::restore_with_tail(checkpoint, too_long, NoMaterializer).is_err());
}

/// A server that restarts from its checkpoint and journal, compared with one
/// that did not restart but whose clients reconnected at the same moment.
#[derive(Clone, Debug)]
enum Step {
    Local {
        client: usize,
        choice: u64,
    },
    Up {
        client: usize,
    },
    Down {
        client: usize,
    },
    /// Checkpoint, keeping this many committed operations before it.
    Checkpoint {
        keep: usize,
    },
    Restart,
}

fn steps(clients: usize) -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            6 => (0..clients, any::<u64>()).prop_map(|(client, choice)| Step::Local { client, choice }),
            4 => (0..clients).prop_map(|client| Step::Up { client }),
            4 => (0..clients).prop_map(|client| Step::Down { client }),
            1 => (0..5usize).prop_map(|keep| Step::Checkpoint { keep }),
            1 => Just(Step::Restart),
        ],
        0..150,
    )
}

/// What is on disk: the checkpoint, the log tail kept before it, and the
/// materializer's file at it, and the journal since.
struct Disk {
    checkpoint: String,
    tail: String,
    file: String,
}

fn restore(disk: &Disk, journal: &[Record<ToyOp>]) -> Sequencer<ToyDoc, Mirror> {
    let checkpoint: Checkpoint<ToyDoc> = serde_json::from_str(&disk.checkpoint).unwrap();
    let tail: Vec<Committed<ToyOp>> = serde_json::from_str(&disk.tail).unwrap();
    let mut server =
        Sequencer::restore_with_tail(checkpoint, tail, Mirror::load(&disk.file)).unwrap();
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

/// `server` answers every resume exactly as `never`, a server with the same
/// history that never checkpointed, except from before `log_start`, which
/// must be what its checkpoints kept and nothing less.
fn resumes_like_never_checkpointed(
    server: &Sequencer<ToyDoc, Mirror>,
    never: &Sequencer<ToyDoc, Mirror>,
    log_start: u64,
) -> Result<(), TestCaseError> {
    prop_assert_eq!(server.checkpoint(), never.checkpoint());
    prop_assert_eq!(never.log_start(), 0);
    prop_assert_eq!(server.log_start(), log_start);
    let seqs = [
        0,
        1,
        log_start.saturating_sub(1),
        log_start,
        log_start + 1,
        (log_start + server.seq()) / 2,
        server.seq().saturating_sub(1),
        server.seq(),
        server.seq() + 1,
    ];
    for replica in (0..server.checkpoint().next_replica + 1).map(ReplicaId) {
        for seq in seqs {
            let expected = never.resume(replica, seq);
            let resumed = server.resume(replica, seq);
            if seq < log_start && expected.is_ok() {
                prop_assert_eq!(
                    resumed,
                    Err(SyncError::Compacted {
                        requested: seq,
                        oldest: log_start
                    })
                );
            } else {
                prop_assert_eq!(resumed, expected, "{} from {}", replica, seq);
            }
        }
    }
    Ok(())
}

/// Replays what `sim` journaled since `fed` into `never`.
fn feed(never: &mut Sequencer<ToyDoc, Mirror>, journal: &[Record<ToyOp>], fed: &mut usize) {
    for record in &journal[*fed..] {
        never.replay(record.clone()).unwrap();
    }
    *fed = journal.len();
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

    /// A server restarted from its checkpoint, tail and journal agrees at
    /// every step with one that kept running (whose clients reconnected at
    /// the same moments), and answers every resume within the kept tail
    /// exactly as a server that never checkpointed at all.
    #[test]
    fn a_restarted_server_is_indistinguishable_from_one_that_kept_running(steps in steps(3)) {
        let sample = ToyDoc::sample();
        let new_sim = || Simulation::with_materializer(sample.clone(), Mirror::load(&sample.string()), 3);
        let mut restarted = new_sim();
        let mut running = new_sim();
        // The same history, never checkpointed nor restarted.
        let mut never = Sequencer::with_materializer(sample.clone(), Mirror::load(&sample.string()));
        let mut fed = 0;
        feed(&mut never, restarted.journal(), &mut fed);
        let mut log_start = 0;
        // Before the first checkpoint, the disk holds the document as opened.
        let mut disk = Disk {
            checkpoint: serde_json::to_string(&Sequencer::new(sample.clone()).checkpoint()).unwrap(),
            tail: "[]".into(),
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
                Step::Checkpoint { keep } => {
                    feed(&mut never, restarted.journal(), &mut fed);
                    let (checkpoint, tail) = restarted.checkpoint_keeping(keep);
                    log_start = log_start.max(checkpoint.seq.saturating_sub(keep as u64));
                    prop_assert_eq!(tail.len() as u64, checkpoint.seq - log_start);
                    disk = Disk {
                        checkpoint: serde_json::to_string(&checkpoint).unwrap(),
                        tail: serde_json::to_string(&tail).unwrap(),
                        file: restarted.server().materializer().export(),
                    };
                    prop_assert!(restarted.journal().is_empty());
                    fed = 0;
                    running.checkpoint_keeping(keep);
                }
                Step::Restart => {
                    let server = restore(&disk, restarted.journal());
                    restarted.restart(server).unwrap();
                    running.reconnect().unwrap();
                }
            }
            feed(&mut never, restarted.journal(), &mut fed);
            same_server(restarted.server(), running.server())?;
            resumes_like_never_checkpointed(restarted.server(), &never, log_start)?;
            for client in 0..3 {
                same_client(restarted.client(client), running.client(client))?;
            }
            // The journal always restores the server as it is now.
            let replayed = restore(&disk, restarted.journal());
            same_server(&replayed, restarted.server())?;
        }
        restarted.quiesce().unwrap();
        running.quiesce().unwrap();
        feed(&mut never, restarted.journal(), &mut fed);
        same_server(restarted.server(), running.server())?;
        resumes_like_never_checkpointed(restarted.server(), &never, log_start)?;
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

#[test]
fn a_replica_within_the_kept_tail_resumes_on_reconnect() {
    let mut sim = Simulation::new(ToyDoc::sample(), 2);
    let behind = sim.client(0).id();
    for ch in ['x', 'y'] {
        let id = sim.client_mut(1).next_id();
        sim.local(
            1,
            ToyOp::Insert {
                id,
                parent: None,
                ch,
            },
        )
        .unwrap();
        sim.deliver_up(1);
        sim.deliver_down(1).unwrap();
    }
    let (checkpoint, tail) = sim.checkpoint_keeping(2);
    assert_eq!(checkpoint.seq, 2);
    assert_eq!(tail.iter().map(|c| c.seq).collect::<Vec<_>>(), [1, 2]);
    sim.reconnect().unwrap();
    assert_eq!(sim.client(0).id(), behind, "resumed, not joined again");
    assert_eq!(sim.client(0).view(), sim.server().document());
    assert!(sim.journal().is_empty());
}
