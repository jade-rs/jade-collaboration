//! Several replicas edit a real document concurrently through the sequencer;
//! the server realises the log in jade. Everyone converges, and the file the
//! server writes is the document everyone sees.

mod common;

use common::{generate, open};
use jade_collab_sync::sim::Simulation;
use jade_docx_collab::JadeDocx;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Step {
    Local { client: usize, choice: u64 },
    Up { client: usize },
    Down { client: usize },
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            3 => (0..3usize, any::<u64>()).prop_map(|(client, choice)| Step::Local { client, choice }),
            2 => (0..3usize).prop_map(|client| Step::Up { client }),
            2 => (0..3usize).prop_map(|client| Step::Down { client }),
        ],
        0..60,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn concurrent_sessions_converge_and_the_file_matches(
        fixture in prop::sample::select(vec!["jade-core/hello.docx", "table-cell-spacing.docx"]),
        steps in steps(),
    ) {
        let (model, jade) = open(fixture);
        let mut sim = Simulation::with_materializer(model, jade, 3);
        for step in &steps {
            match *step {
                Step::Local { client, choice } => {
                    let view = sim.client(client).view().clone();
                    let op = generate(&view, choice, &mut |n| sim.client_mut(client).reserve(n));
                    if let Some(op) = op {
                        let _ = sim.local(client, op);
                    }
                }
                Step::Up { client } => {
                    sim.deliver_up(client);
                }
                Step::Down { client } => {
                    sim.deliver_down(client).unwrap();
                }
            }
            for client in 0..3 {
                prop_assert_eq!(sim.client(client).view(), &sim.client(client).rebuilt_view());
            }
        }
        sim.quiesce().unwrap();
        let server = sim.server();
        prop_assert!(server.faults().is_empty(), "materialization faults: {:?}", server.faults());
        for client in 0..3 {
            prop_assert_eq!(sim.client(client).view(), server.document());
        }
        prop_assert_eq!(server.materializer().body_paragraph_texts(), server.document().paragraph_texts());
        let reloaded = JadeDocx::load(&server.materializer().export().unwrap()).unwrap();
        prop_assert_eq!(reloaded.body_paragraph_texts(), server.document().paragraph_texts());
    }
}
