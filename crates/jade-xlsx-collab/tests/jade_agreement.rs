mod common;

use common::{assert_agree, fixtures, generate, open, SMALL};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_xlsx_collab::{JadeXlsx, XlsxModel};
use proptest::prelude::*;

#[test]
fn every_fixture_loads_into_an_agreeing_model() {
    let mut checked = 0;
    let mut paths: Vec<_> = std::fs::read_dir(fixtures())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "xlsx"))
        .collect();
    paths.sort();
    for path in paths {
        let Ok(jade) = JadeXlsx::open(&std::fs::read(&path).unwrap()) else {
            continue;
        };
        let model = XlsxModel::from_workbook(jade.workbook(), SMALL).unwrap();
        assert_agree(&model, &jade, &path.display().to_string());
        checked += 1;
    }
    assert!(checked >= 15, "only {checked} fixtures were checked");
}

const EDIT_FIXTURES: &[&str] = &["grid-operations-workbook.xlsx", "calculation-open-workbook.xlsx"];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn random_edits_keep_model_and_jade_in_agreement(
        fixture in prop::sample::select(EDIT_FIXTURES),
        choices in prop::collection::vec(any::<u64>(), 1..15),
    ) {
        let (mut model, mut jade) = open(fixture);
        let mut next = 1_000_000u64;
        for (step, choice) in choices.into_iter().enumerate() {
            let mut ids = |n: u64| {
                let id = Id::new(next, ReplicaId(1));
                next += n;
                id
            };
            let Some(op) = generate(&model, choice, &mut ids) else { continue };
            let seq = step as u64 + 1;
            let ctx = ApplyContext { origin: ReplicaId(1), base_seq: seq - 1, seq: Some(seq) };
            if XlsxModel::lane(&op) == Lane::Sequenced && jade.admit(&model, &op).is_err() {
                continue;
            }
            let before = model.clone();
            match model.apply(&op, &ctx) {
                Ok(effect) => {
                    jade.materialize(&model, effect, &ctx)
                        .unwrap_or_else(|e| panic!("{fixture}: jade refused {op:?}: {e}"));
                    assert_agree(&model, &jade, &format!("{fixture} after {op:?}"));
                }
                Err(_) => prop_assert_eq!(&model, &before),
            }
        }
        let reopened = JadeXlsx::open(&jade.export().unwrap()).unwrap();
        assert_agree(&model, &reopened, "after export");
    }

    #[test]
    fn concurrent_sessions_converge_and_the_workbook_matches(
        steps in prop::collection::vec((0..3usize, 0..3u8, any::<u64>()), 0..40),
    ) {
        let (model, jade) = open("grid-operations-workbook.xlsx");
        let mut sim = Simulation::with_materializer(model, jade, 3);
        for (client, kind, choice) in steps {
            match kind {
                0 => {
                    let view = sim.client(client).view().clone();
                    if let Some(op) = generate(&view, choice, &mut |n| sim.client_mut(client).reserve(n)) {
                        let _ = sim.local(client, op);
                    }
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
        let server = sim.server();
        prop_assert!(server.faults().is_empty(), "{:?}", server.faults());
        for client in 0..3 {
            prop_assert_eq!(sim.client(client).view(), server.document());
        }
        assert_agree(server.document(), server.materializer(), "after the session");
    }
}
