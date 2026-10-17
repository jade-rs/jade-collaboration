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

const EDIT_FIXTURES: &[&str] = &[
    "grid-operations-workbook.xlsx",
    "calculation-open-workbook.xlsx",
];

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

/// Excel normalises a table's header and totals cells (a blank header
/// becomes "Column1"), which the model cannot predict; every replica refuses
/// to write them, so the refusal is local and never reaches the server.
#[test]
fn table_header_cells_are_read_only() {
    use jade_xlsx_collab::{CellInput, XlsxOp};

    let (model, jade) = open("grid-operations-workbook.xlsx");
    let sheet = &model.sheets()[0];
    let mut header = None;
    'find: for (r, row) in sheet.rows.iter().enumerate() {
        for (c, column) in sheet.columns.iter().enumerate() {
            let reference = format!("{}{}", jade_xlsx_collab::column_label(c as u32), r + 1);
            let view = jade.workbook().cell(&sheet.name, &reference).unwrap();
            if view.table.is_some_and(|t| t.header) {
                header = Some((*row, *column));
                break 'find;
            }
        }
    }
    let (row, column) = header.expect("the fixture has an Excel table");
    let mut model = model;
    let op = XlsxOp::SetCell {
        sheet: sheet.id,
        row,
        column,
        stamp: Id::new(1_000_000, ReplicaId(1)),
        value: CellInput::Blank,
    };
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: None,
    };
    let err = model.apply(&op, &ctx).unwrap_err();
    assert!(err.message().contains("table"), "{err}");
}

/// A formula and a row inserted above what it points at, outside the Excel
/// table: the model moves the reference exactly as jade does.
#[test]
fn formulas_move_with_inserted_rows_like_in_jade() {
    use jade_collab_core::grid::GridOp;
    use jade_xlsx_collab::{CellInput, XlsxOp};

    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let (sheet, row6, column_e) = model.locate("Operations", "E6").unwrap();
    let (_, row5, _) = model.locate("Operations", "E5").unwrap();
    let mut seq = 0;
    let mut run = |model: &mut XlsxModel, jade: &mut JadeXlsx, op: XlsxOp| {
        seq += 1;
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: seq - 1,
            seq: Some(seq),
        };
        jade.admit(model, &op).unwrap();
        let effect = model
            .apply(&op, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        jade.materialize(model, effect, &ctx).unwrap();
        assert_agree(model, jade, &format!("after {op:?}"));
    };
    run(
        &mut model,
        &mut jade,
        XlsxOp::SetCell {
            sheet,
            row: row6,
            column: column_e,
            stamp: Id::new(1_000_000, ReplicaId(1)),
            value: CellInput::Formula("=D7+1".into()),
        },
    );
    run(
        &mut model,
        &mut jade,
        XlsxOp::Structure {
            sheet,
            op: GridOp::InsertRow {
                id: Id::new(1_000_001, ReplicaId(1)),
                after: Some(row5),
            },
        },
    );
    assert_eq!(
        model.value(sheet, row6, column_e),
        Some(&CellInput::Formula("D8+1".into()))
    );
    assert_eq!(
        model.locate("Operations", "E7").map(|(_, r, _)| r),
        Some(row6)
    );
}
