//! Random multi-client sessions over a simulated network with arbitrary
//! message delays: everyone converges on the server's document, the
//! document never breaks its invariants, and only sequenced operations are
//! ever rejected.

mod common;

use common::{generate, ToyDoc};
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::Lane;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Step {
    Local { client: usize, choice: u64 },
    Up { client: usize },
    Down { client: usize },
}

fn steps(clients: usize) -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            3 => (0..clients, any::<u64>()).prop_map(|(client, choice)| Step::Local { client, choice }),
            2 => (0..clients).prop_map(|client| Step::Up { client }),
            2 => (0..clients).prop_map(|client| Step::Down { client }),
        ],
        0..150,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn random_sessions_converge(steps in steps(3)) {
        let mut sim = Simulation::new(ToyDoc::sample(), 3);
        for step in &steps {
            match *step {
                Step::Local { client, choice } => {
                    let op = generate(sim.client_mut(client), choice);
                    // Locally invalid operations are refused before sending.
                    let _ = sim.local(client, op);
                }
                Step::Up { client } => {
                    sim.deliver_up(client);
                }
                Step::Down { client } => {
                    sim.deliver_down(client).unwrap();
                }
            }
            prop_assert_eq!(sim.server().document().grid.check_invariants(), Ok(()));
            for client in 0..3 {
                // The optimistic view always equals confirmed + pending replayed
                // — the fast path never drifts from a full rebuild.
                prop_assert_eq!(sim.client(client).view(), &sim.client(client).rebuilt_view());
            }
        }
        sim.quiesce().unwrap();
        for client in 0..3 {
            let replica = sim.client(client);
            prop_assert_eq!(replica.pending_len(), 0);
            prop_assert_eq!(replica.seq(), sim.server().seq());
            prop_assert_eq!(replica.view(), sim.server().document());
            prop_assert_eq!(replica.confirmed(), sim.server().document());
        }
        prop_assert_eq!(sim.stats().rejected(Lane::Convergent), 0);
    }
}
