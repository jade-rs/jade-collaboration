//! Derived operations: what the real document does on its own (a
//! spreadsheet naming a new table column, filling a calculated column) is
//! committed by the server as operations of the document's origin, right
//! after the operation that caused it, so every replica learns it.

mod common;

use common::{generate, origin, ToyDoc, ToyOp};
use jade_collab_core::grid::GridOp;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::protocol::Submission;
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{ApplyContext, Materializer, Sequencer, SubmitOutcome};
use proptest::prelude::*;

/// Stands in for a real document that fills the first column of every row
/// inserted with 42, and counts how often it was asked.
#[derive(Clone, Default)]
struct Filler {
    asked: usize,
}

impl Materializer<ToyDoc> for Filler {
    fn may_derive(&self, _: &ToyDoc, op: &ToyOp) -> bool {
        matches!(op, ToyOp::Grid(GridOp::InsertRow { .. }))
    }
    fn fork(&self, _: &ToyDoc) -> Result<Self, String> {
        Ok(self.clone())
    }

    fn materialize(&mut self, _: &ToyDoc, _: (), _: &ApplyContext) -> Result<(), String> {
        Ok(())
    }

    fn derived(
        &mut self,
        document: &ToyDoc,
        op: &ToyOp,
    ) -> Result<Vec<ToyOp>, jade_collab_sync::Rejection> {
        self.asked += 1;
        Ok(match op {
            ToyOp::Grid(GridOp::InsertRow { id, .. }) => vec![ToyOp::SetCell {
                row: *id,
                col: document.grid.all_columns()[0],
                stamp: Id::new(
                    jade_collab_sync::Document::max_lamport(document)
                        .checked_add(1)
                        .ok_or_else(|| jade_collab_sync::Rejection::new("derived id exhausted"))?,
                    ReplicaId::ORIGIN,
                ),
                value: 42,
            }],
            _ => Vec::new(),
        })
    }
}

fn insert_row(client_seq: u64, id: u64) -> Submission<ToyOp> {
    Submission {
        client_seq,
        base_seq: 0,
        op: ToyOp::Grid(GridOp::InsertRow {
            id: Id::new(id, ReplicaId(1)),
            after: Some(origin(10)),
        }),
    }
}

#[test]
fn derived_operations_follow_their_cause_as_origin_operations() {
    let mut server = Sequencer::with_materializer(ToyDoc::sample(), Filler::default());
    let a = server.join().replica;
    let outcome = server.submit(a, insert_row(1, 100));
    assert!(matches!(outcome, SubmitOutcome::Committed(_)));
    let derived = server.take_derived();
    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].origin, ReplicaId::ORIGIN);
    assert_eq!(derived[0].seq, 2);
    assert_eq!(server.seq(), 2);
    assert!(server.take_derived().is_empty(), "taken once");
    let row = Id::new(100, ReplicaId(1));
    assert_eq!(server.document().cell(row, origin(20)), Some(42));
    // A rejected or convergent op derives nothing here.
    server.submit(a, insert_row(2, 100));
    assert!(server.take_derived().is_empty());
}

#[test]
fn a_journal_replays_derived_operations_without_deriving_them_again() {
    let mut server = Sequencer::with_materializer(ToyDoc::sample(), Filler::default());
    let start = server.checkpoint();
    let a = server.join().replica;
    let mut journal = vec![jade_collab_sync::Record::Joined { replica: a }];
    for (k, id) in [(1, 100), (2, 101)] {
        let outcome = server.submit(a, insert_row(k, id));
        journal.extend(outcome.record(a));
        journal.extend(
            server
                .take_derived()
                .into_iter()
                .map(jade_collab_sync::Record::Committed),
        );
    }
    let mut replayed = Sequencer::restore(start, Filler::default());
    for record in journal {
        replayed.replay(record).unwrap();
    }
    assert_eq!(replayed.document(), server.document());
    assert_eq!(replayed.log(), server.log());
    assert_eq!(
        replayed.materializer().asked,
        0,
        "replay re-derives nothing"
    );
    assert!(replayed.take_derived().is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Replicas converge on the server's document, derived operations
    /// included, under any message schedule.
    #[test]
    fn sessions_with_derived_operations_converge(
        steps in prop::collection::vec((0usize..3, 0u8..3, any::<u64>()), 0..120),
    ) {
        let mut sim = Simulation::with_materializer(ToyDoc::sample(), Filler::default(), 3);
        for (client, kind, choice) in steps {
            match kind {
                0 => {
                    let op = generate(sim.client_mut(client), choice);
                    let _ = sim.local(client, op);
                }
                1 => {
                    sim.deliver_up(client);
                }
                _ => {
                    sim.deliver_down(client).unwrap();
                }
            }
        }
        sim.quiesce().unwrap();
        for client in 0..3 {
            prop_assert_eq!(sim.client(client).view(), sim.server().document());
        }
        // Every committed row insertion was followed by its derived write
        // (later writes may overwrite the value: last writer wins).
        let log = sim.server().log();
        for (k, entry) in log.iter().enumerate() {
            if let ToyOp::Grid(GridOp::InsertRow { id, .. }) = entry.op {
                let next = &log[k + 1];
                prop_assert_eq!(next.origin, ReplicaId::ORIGIN);
                let fills = matches!(next.op, ToyOp::SetCell { row, value: 42, .. } if row == id);
                prop_assert!(fills, "{:?} after inserting {:?}", next.op, id);
            }
        }
    }
}
