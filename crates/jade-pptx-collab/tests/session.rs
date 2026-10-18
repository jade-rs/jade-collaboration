//! Several replicas edit a deck concurrently through the sequencer. Every
//! replica's optimistic view stays equal to its confirmed state with the
//! pending operations replayed (the convergent fast path never drifts),
//! everyone converges, and — with jade as the materializer — the server's
//! file is the deck everyone sees, with no materialization faults.

mod common;

use common::{assert_agree, assert_agree_with, generate, open};
use jade_collab_sync::sim::Simulation;
use jade_pptx_collab::{JadePptx, PptxModel};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Step {
    Local { client: usize, choice: u64 },
    Up { client: usize },
    Down { client: usize },
}

fn steps(len: usize) -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            3 => (0..3usize, any::<u64>()).prop_map(|(client, choice)| Step::Local { client, choice }),
            2 => (0..3usize).prop_map(|client| Step::Up { client }),
            2 => (0..3usize).prop_map(|client| Step::Down { client }),
        ],
        0..len,
    )
}

fn run<M: jade_collab_sync::Materializer<PptxModel>>(
    sim: &mut Simulation<PptxModel, M>,
    steps: &[Step],
) -> Result<(), TestCaseError> {
    for step in steps {
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
            prop_assert_eq!(
                sim.client(client).view(),
                &sim.client(client).rebuilt_view()
            );
        }
    }
    sim.quiesce().unwrap();
    for client in 0..3 {
        prop_assert_eq!(sim.client(client).view(), sim.server().document());
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The model alone, many cases: concurrent duplications, deletions and
    /// moves interleaved with typing and geometry edits.
    #[test]
    fn concurrent_sessions_converge_on_the_model(steps in steps(120)) {
        let model = PptxModel::from_texts(&[&["Title", "Body\ntext"], &["Second"], &["Third"]]);
        let mut sim = Simulation::new(model, 3);
        run(&mut sim, &steps)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn concurrent_sessions_converge_and_the_file_matches(
        fixture in prop::sample::select(vec!["sample-deck.pptx", "grouped-deck.pptx"]),
        steps in steps(50),
    ) {
        let (model, jade) = open(fixture);
        let mut sim = Simulation::with_materializer(model, jade, 3);
        run(&mut sim, &steps)?;
        let server = sim.server();
        prop_assert!(server.faults().is_empty(), "materialization faults: {:?}", server.faults());
        assert_agree(server.document(), server.materializer(), "after the session");
        let reopened = JadePptx::open(&server.materializer().export().unwrap()).unwrap();
        assert_agree_with(server.document(), server.materializer(), &reopened, "after export");
    }
}
