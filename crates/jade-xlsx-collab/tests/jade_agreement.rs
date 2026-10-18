mod common;

use common::{
    assert_agree, fixtures, generate, masked, open, overlay, set_attribute, ATTRIBUTES, SMALL,
};
use jade_collab_core::grid::GridOp;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_xlsx_collab::{BorderLine, BorderStyle, FormatChange, JadeXlsx, XlsxModel, XlsxOp};
use proptest::prelude::*;

/// Commits `op` at the next log position, materializes it and checks the
/// whole workbook still agrees.
fn commit(model: &mut XlsxModel, jade: &mut JadeXlsx, seq: &mut u64, op: XlsxOp) {
    *seq += 1;
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: *seq - 1,
        seq: Some(*seq),
    };
    if XlsxModel::lane(&op) == Lane::Sequenced {
        jade.admit(model, &op).unwrap();
    }
    let effect = model
        .apply(&op, &ctx)
        .unwrap_or_else(|e| panic!("{op:?}: {e}"));
    jade.materialize(model, effect, &ctx)
        .unwrap_or_else(|e| panic!("jade refused {op:?}: {e}"));
    assert_agree(model, jade, &format!("after {op:?}"));
}

fn stamp(n: u64) -> Id {
    Id::new(1_000_000 + n, ReplicaId(1))
}

fn format_op(model: &XlsxModel, reference: &str, n: u64, change: FormatChange) -> XlsxOp {
    let (sheet, row, column) = model.locate("Operations", reference).unwrap();
    XlsxOp::SetFormat {
        sheet,
        rows: vec![row],
        columns: vec![column],
        stamp: stamp(n),
        change,
    }
}

fn note_op(model: &XlsxModel, reference: &str, n: u64, note: Option<&str>) -> XlsxOp {
    let (sheet, row, column) = model.locate("Operations", reference).unwrap();
    XlsxOp::SetNote {
        sheet,
        row,
        column,
        stamp: stamp(n),
        note: note.map(str::to_string),
    }
}

fn arb_change() -> impl Strategy<Value = FormatChange> {
    use prop::option::of;
    use prop::sample::select;
    let text = |options: &'static [&'static str]| of(select(options).prop_map(str::to_string));
    (
        (of(any::<bool>()), of(any::<bool>()), of(any::<bool>())),
        text(&[
            "FF0000", "#00ff00", "red", "12345", "GG0000", "80FF0000", "",
        ]),
        of(select(
            &[
                0.5f32,
                1.0,
                11.0,
                12.25,
                409.0,
                410.0,
                f32::NAN,
                f32::INFINITY,
            ][..],
        )),
        text(&["", "Arial", "Yu Gothic", "A & <B>"]),
        text(&["", "C0FFEE", "#c0ffee", "blue", "c0ffee00"]),
        text(&[
            "",
            "general",
            "General",
            "0.00",
            "@",
            "[Red]0.0;-0.0",
            "yyyy/mm/dd",
        ]),
        (
            of((
                select(&BorderStyle::ALL[..]),
                text(&["FF0000", "#00b050", "blue", "12345", "80FF0000", ""]),
            )),
            of(any::<bool>()),
        ),
    )
        .prop_map(
            |(
                (bold, italic, wrap_text),
                font_color,
                font_size,
                font_name,
                fill_color,
                number_format,
                (border, diagonal_up),
            )| {
                FormatChange {
                    bold,
                    italic,
                    wrap_text,
                    font_color,
                    font_size,
                    font_name,
                    fill_color,
                    number_format,
                    border_top: border.map(|(style, color)| BorderLine { style, color }),
                    diagonal_up,
                    ..FormatChange::default()
                }
            },
        )
}

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
                    // As the sequencer does: what jade did on its own.
                    for derived in jade.derived(&model, &op) {
                        let ctx = ApplyContext { origin: ReplicaId::ORIGIN, base_seq: seq, seq: Some(seq) };
                        let effect = model.apply(&derived, &ctx)
                            .unwrap_or_else(|e| panic!("{fixture}: derived {derived:?}: {e}"));
                        jade.materialize(&model, effect, &ctx).unwrap();
                    }
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

    /// Replicas format one cell concurrently, one attribute per write, with
    /// messages delivered in a random order. Every attribute anyone set
    /// survives, and each holds the value of its newest write — on every
    /// replica, on the server and in jade.
    #[test]
    fn concurrent_formatting_keeps_every_attribute_and_the_newest_of_each(
        fixture in prop::sample::select(EDIT_FIXTURES),
        target in prop::sample::select(&["A1", "B3", "E1", "E3"][..]),
        steps in prop::collection::vec((0..3usize, 0..4u8, 0..ATTRIBUTES, any::<u64>()), 1..30),
    ) {
        let (model, jade) = open(fixture);
        let name = model.sheets()[0].name.clone();
        let (sheet, row, column) = model.locate(&name, target).unwrap();
        let mut sim = Simulation::with_materializer(model, jade, 3);
        let mut writes = Vec::new();
        for (client, kind, attribute, value) in steps {
            match kind {
                0 | 1 => {
                    let mut change = FormatChange::default();
                    set_attribute(&mut change, attribute, value);
                    let stamp = sim.client_mut(client).reserve(1);
                    let op = XlsxOp::SetFormat {
                        sheet,
                        rows: vec![row],
                        columns: vec![column],
                        stamp,
                        change: change.clone(),
                    };
                    prop_assert!(sim.local(client, op).is_ok());
                    writes.push((stamp, change));
                }
                2 => {
                    sim.deliver_up(client);
                }
                _ => {
                    sim.deliver_down(client).unwrap();
                }
            }
        }
        sim.quiesce().unwrap();
        writes.sort_by_key(|(stamp, _)| *stamp);
        let expected = writes
            .iter()
            .fold(FormatChange::default(), |under, (_, over)| overlay(&under, over));
        let server = sim.server();
        prop_assert!(server.faults().is_empty(), "{:?}", server.faults());
        prop_assert_eq!(server.document().format(sheet, row, column), expected.clone());
        for client in 0..3 {
            prop_assert_eq!(sim.client(client).view(), server.document());
        }
        let theirs = server.materializer().workbook().format_at(&name, target).unwrap();
        prop_assert_eq!(masked(&theirs, &expected), expected);
        assert_agree(server.document(), server.materializer(), "after the session");
    }

    /// What the model accepts, jade can write — a convergent operation must
    /// commit wherever it lands, so the server can never be left with one
    /// jade refuses — and reads back as the model stored it.
    #[test]
    fn a_format_the_model_accepts_is_one_jade_writes_and_reads_back(
        fixture in prop::sample::select(EDIT_FIXTURES),
        change in arb_change(),
        cell in prop::sample::select(&["A1", "B3", "E1", "F1", "D7"][..]),
    ) {
        let (mut model, mut jade) = open(fixture);
        let op = format_op(&model, cell, 1, change);
        let ctx = ApplyContext { origin: ReplicaId(1), base_seq: 0, seq: Some(1) };
        let before = model.clone();
        match model.apply(&op, &ctx) {
            Ok(effect) => {
                jade.materialize(&model, effect, &ctx)
                    .unwrap_or_else(|e| panic!("jade refused {op:?}: {e}"));
                assert_agree(&model, &jade, &format!("after {op:?}"));
                let reopened = JadeXlsx::open(&jade.export().unwrap()).unwrap();
                assert_agree(&model, &reopened, "after export");
            }
            Err(_) => prop_assert_eq!(&model, &before),
        }
    }
}

/// jade leaves a table's header text and column name alone when only its
/// format or note changes, so neither needs the read-only protection values
/// have.
#[test]
fn table_header_cells_take_notes_and_formats() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let header = |jade: &JadeXlsx| jade.workbook().cell("Operations", "A1").unwrap();
    let before = header(&jade);
    assert!(before.table.as_ref().is_some_and(|t| t.header));
    let mut seq = 0;
    let op = note_op(&model, "A1", 1, Some("header note"));
    commit(&mut model, &mut jade, &mut seq, op);
    let op = format_op(
        &model,
        "A1",
        2,
        FormatChange {
            bold: Some(true),
            fill_color: Some("FFFF00".into()),
            number_format: Some("0.00".into()),
            ..FormatChange::default()
        },
    );
    commit(&mut model, &mut jade, &mut seq, op);
    let after = header(&jade);
    assert_eq!(after.text, before.text);
    assert_eq!(after.table, before.table);
    assert_eq!(after.comment.map(|c| c.text), Some("header note".into()));
}

/// jade reports a merged cell's format at the merge's anchor. The model
/// knows the file's merges and moves them as jade does, so formatting a
/// covered cell shows only once a deletion makes it the anchor.
#[test]
fn a_covered_cell_shows_its_merge_anchors_format() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let italic = FormatChange {
        italic: Some(true),
        ..FormatChange::default()
    };
    let bold = FormatChange {
        bold: Some(true),
        ..FormatChange::default()
    };
    let (sheet, row1, column_f) = model.locate("Operations", "F1").unwrap();
    let (_, _, column_e) = model.locate("Operations", "E1").unwrap();
    let mut seq = 0;
    let op = format_op(&model, "F1", 1, italic.clone());
    commit(&mut model, &mut jade, &mut seq, op);
    assert_eq!(model.format(sheet, row1, column_f), FormatChange::default());
    let op = format_op(&model, "E1", 2, bold.clone());
    commit(&mut model, &mut jade, &mut seq, op);
    assert_eq!(model.format(sheet, row1, column_f), bold);
    // A column inserted inside the merge is covered too.
    let op = XlsxOp::Structure {
        sheet,
        op: GridOp::InsertColumn {
            id: stamp(3),
            after: Some(column_e),
        },
    };
    commit(&mut model, &mut jade, &mut seq, op);
    assert_eq!(model.format(sheet, row1, stamp(3)), bold);
    let op = XlsxOp::Structure {
        sheet,
        op: GridOp::DeleteColumn { id: column_e },
    };
    commit(&mut model, &mut jade, &mut seq, op);
    // The inserted column is the anchor now.
    assert_eq!(model.format(sheet, row1, stamp(3)), FormatChange::default());
    assert_eq!(model.format(sheet, row1, column_f), FormatChange::default());
    let op = XlsxOp::Structure {
        sheet,
        op: GridOp::DeleteColumn { id: stamp(3) },
    };
    commit(&mut model, &mut jade, &mut seq, op);
    assert_eq!(model.format(sheet, row1, column_f), italic);
}

/// Notes stay on their cell as rows and columns come and go, and go with a
/// deleted row — as jade moves the comments and VML parts.
#[test]
fn notes_follow_their_cells_like_in_jade() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let (sheet, row6, column_e) = model.locate("Operations", "E6").unwrap();
    let mut seq = 0;
    let op = note_op(&model, "E6", 1, Some("follow me"));
    commit(&mut model, &mut jade, &mut seq, op);
    let op = note_op(&model, "B7", 2, Some("second"));
    commit(&mut model, &mut jade, &mut seq, op);
    let op = XlsxOp::Structure {
        sheet,
        op: GridOp::InsertRow {
            id: stamp(3),
            after: None,
        },
    };
    commit(&mut model, &mut jade, &mut seq, op);
    let op = XlsxOp::Structure {
        sheet,
        op: GridOp::InsertColumn {
            id: stamp(4),
            after: model.locate("Operations", "D1").map(|(_, _, c)| c),
        },
    };
    commit(&mut model, &mut jade, &mut seq, op);
    assert_eq!(
        jade.workbook()
            .cell("Operations", "F7")
            .unwrap()
            .comment
            .map(|c| c.text),
        Some("follow me".into())
    );
    assert_eq!(model.note(sheet, row6, column_e), Some("follow me"));
    let op = XlsxOp::Structure {
        sheet,
        op: GridOp::DeleteRow { id: row6 },
    };
    commit(&mut model, &mut jade, &mut seq, op);
    assert!(jade
        .workbook()
        .cell("Operations", "F7")
        .unwrap()
        .comment
        .is_none());
    let op = note_op(&model, "B7", 5, None);
    commit(&mut model, &mut jade, &mut seq, op);
    let reopened = JadeXlsx::open(&jade.export().unwrap()).unwrap();
    assert_agree(&model, &reopened, "after export");
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
