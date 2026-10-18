//! Operations the server commits itself, as the document's origin, outside
//! any replica's submission — how it tells everyone who a replica is (a
//! docx revision's author) when the replica joins with a named token.

mod common;

use common::{generate, origin, ToyDoc, ToyOp};
use jade_collab_core::grid::GridOp;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{Record, Sequencer};
use proptest::prelude::*;

fn origin_row(n: u64) -> ToyOp {
    ToyOp::Grid(GridOp::InsertRow {
        id: Id::new(n, ReplicaId::ORIGIN),
        after: Some(origin(10)),
    })
}

#[test]
fn the_server_commits_origin_operations_into_the_log() {
    let mut server = Sequencer::new(ToyDoc::sample());
    let a = server.join().replica;
    let committed = server.commit_origin(origin_row(500)).unwrap();
    assert_eq!(committed.origin, ReplicaId::ORIGIN);
    assert_eq!(committed.seq, 1);
    assert_eq!(server.seq(), 1);
    assert_eq!(server.log(), [committed.clone()]);
    assert_eq!(server.processed_client_seq(a), 0, "no replica's submission");
    // One the model refuses is not committed.
    assert!(server.commit_origin(origin_row(500)).is_err());
    assert_eq!(server.seq(), 1);
    // A journal replays it like any commit.
    let mut replayed = Sequencer::new(ToyDoc::sample());
    replayed.replay(Record::Joined { replica: a }).unwrap();
    replayed.replay(Record::Committed(committed)).unwrap();
    assert_eq!(replayed.document(), server.document());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Origin operations committed at any point reach every replica in log
    /// order; everyone converges and views stay equal to their rebuilds.
    #[test]
    fn sessions_with_origin_operations_converge(
        steps in prop::collection::vec((0usize..4, 0u8..4, any::<u64>()), 0..120),
    ) {
        let mut sim = Simulation::new(ToyDoc::sample(), 3);
        let mut next = 1_000u64;
        for (client, kind, choice) in steps {
            match kind {
                0 if client == 3 => {
                    next += 1;
                    let _ = sim.commit_origin(origin_row(next));
                }
                0 | 3 => {
                    let client = client % 3;
                    let op = generate(sim.client_mut(client), choice);
                    let _ = sim.local(client, op);
                }
                1 => {
                    sim.deliver_up(client % 3);
                }
                _ => {
                    sim.deliver_down(client % 3).unwrap();
                }
            }
            for client in 0..3 {
                prop_assert_eq!(sim.client(client).view(), &sim.client(client).rebuilt_view());
            }
        }
        sim.quiesce().unwrap();
        for client in 0..3 {
            prop_assert_eq!(sim.client(client).view(), sim.server().document());
        }
    }
}
