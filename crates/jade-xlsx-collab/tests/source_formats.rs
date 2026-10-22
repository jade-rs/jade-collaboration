mod common;

use common::{fixtures, SMALL};
use jade_collab_core::id::Id;
use jade_collab_sync::{sim::Simulation, Sequencer};
use jade_xlsx_collab::{BorderLine, BorderStyle, FormatChange, LoadOptions, XlsxModel};
use jade_xlsx_collab::{JadeXlsx, LineAxis, XlsxOp};
use jade_xlsx_headless_editor::HeadlessWorkbook;
use proptest::prelude::*;

fn source() -> HeadlessWorkbook {
    HeadlessWorkbook::open(
        &std::fs::read(fixtures().join("grid-operations-workbook.xlsx")).unwrap(),
    )
    .unwrap()
}

fn assert_exact(model: &XlsxModel, book: &HeadlessWorkbook) {
    for sheet in model.sheets() {
        for (r, row) in sheet.rows.iter().enumerate() {
            for (c, column) in sheet.columns.iter().enumerate() {
                let reference = format!("{}{}", jade_xlsx_collab::column_label(c as u32), r + 1);
                assert_eq!(
                    model.format(sheet.id, *row, *column),
                    book.format_at(&sheet.name, &reference).unwrap(),
                    "{}!{reference}",
                    sheet.name
                );
            }
        }
    }
}

#[test]
fn imported_cells_rows_columns_and_default_styles_are_reported_without_rewriting_the_file() {
    let mut book = source();
    for (range, change) in [
        (
            "C:C",
            FormatChange {
                italic: Some(true),
                font_color: Some("FF0000".into()),
                ..Default::default()
            },
        ),
        (
            "5:5",
            FormatChange {
                bold: Some(true),
                fill_color: Some("00FF00".into()),
                ..Default::default()
            },
        ),
        (
            "C5",
            FormatChange {
                bold: Some(false),
                number_format: Some("0.000%".into()),
                border_bottom: Some(BorderLine {
                    style: BorderStyle::Double,
                    color: Some("123456".into()),
                }),
                ..Default::default()
            },
        ),
        (
            "J20",
            FormatChange {
                font_name: Some("Yu Gothic".into()),
                font_size: Some(14.5),
                ..Default::default()
            },
        ),
    ] {
        book.set_format("Operations", range, &change).unwrap();
    }
    let before = book.bytes().to_vec();
    let model = XlsxModel::from_workbook(&book, SMALL).unwrap();
    assert_exact(&model, &book);
    assert_eq!(book.bytes(), before);
    let checkpoint: XlsxModel =
        serde_json::from_slice(&serde_json::to_vec(&model).unwrap()).unwrap();
    assert_eq!(checkpoint, model);
}

#[test]
fn an_explicit_default_cell_style_hides_row_and_column_attributes_including_unknown_colours() {
    use jade_xlsx_reader::CellRef;
    use jade_xlsx_writer::{write_xlsx, CellEdit, NewValue, SheetEdits};
    let mut book = source();
    book.set_format(
        "Operations",
        "J:J",
        &FormatChange {
            font_color: Some("FF0000".into()),
            italic: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    book.set_format(
        "Operations",
        "20:20",
        &FormatChange {
            bold: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let bytes = write_xlsx(
        book.bytes(),
        &[SheetEdits {
            part_name: book.workbook().sheets[0].part_name.clone(),
            cells: vec![CellEdit {
                reference: CellRef::parse("J20").unwrap(),
                value: NewValue::Unchanged,
                style_index: Some(0),
            }],
        }],
    )
    .unwrap();
    let book = HeadlessWorkbook::open(&bytes).unwrap();
    let model = XlsxModel::from_workbook(&book, SMALL).unwrap();
    let (sheet, row, column) = model.locate("Operations", "J20").unwrap();
    let format = model.format(sheet, row, column);
    assert_eq!(format, book.format_at("Operations", "J20").unwrap());
    assert_eq!(format.bold, Some(false));
    assert_eq!(format.italic, Some(false));
    assert_eq!(format.font_color, None);
}

#[test]
fn checkpoints_without_source_formats_keep_their_existing_collaborative_writes() {
    use jade_collab_core::id::ReplicaId;
    use jade_collab_sync::{ApplyContext, Document};
    let mut model = XlsxModel::from_workbook(&source(), SMALL).unwrap();
    let (sheet, row, column) = model
        .locate("Operations", "J20")
        .unwrap_or_else(|| model.locate("Operations", "A1").unwrap());
    let change = FormatChange {
        italic: Some(true),
        ..Default::default()
    };
    let stamp = Id::new(model.max_lamport() + 1, ReplicaId(1));
    model
        .apply(
            &XlsxOp::SetFormat {
                sheet,
                rows: vec![row],
                columns: vec![column],
                stamp,
                change: change.clone(),
            },
            &ApplyContext {
                origin: ReplicaId(1),
                base_seq: 0,
                seq: None,
            },
        )
        .unwrap();
    let mut json = serde_json::to_value(&model).unwrap();
    for sheet in json["sheets"].as_array_mut().unwrap() {
        sheet
            .as_object_mut()
            .unwrap()
            .remove("source_formats")
            .unwrap();
    }
    let legacy: XlsxModel = serde_json::from_value(json).unwrap();
    assert_eq!(legacy.format(sheet, row, column), change);
    assert_eq!(legacy.default_format(sheet), FormatChange::default());
}

#[test]
fn every_supported_fixture_imports_the_same_base_format_as_the_editor() {
    let mut checked = 0;
    for entry in std::fs::read_dir(fixtures()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "xlsx") {
            continue;
        }
        let Ok(book) = HeadlessWorkbook::open(&std::fs::read(&path).unwrap()) else {
            continue;
        };
        let model = XlsxModel::from_workbook(&book, SMALL).unwrap();
        assert_exact(&model, &book);
        checked += 1;
    }
    assert!(checked >= 15);
}

#[test]
fn an_inserted_column_does_not_inherit_an_imported_columns_style_span() {
    use jade_collab_core::{grid::GridOp, id::ReplicaId};
    use jade_collab_sync::{ApplyContext, Document, Materializer};
    let mut book = source();
    book.set_format(
        "Operations",
        "D:F",
        &FormatChange {
            fill_color: Some("FF0000".into()),
            italic: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    book.set_format(
        "Operations",
        "6:6",
        &FormatChange {
            bold: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let mut model = XlsxModel::from_workbook(&book, SMALL).unwrap();
    let mut jade = JadeXlsx::new(book);
    let (sheet, _, column) = model.locate("Operations", "D1").unwrap();
    let op = XlsxOp::Structure {
        sheet,
        op: GridOp::InsertColumn {
            id: Id::new(model.max_lamport() + 1, ReplicaId(1)),
            after: Some(column),
        },
    };
    let ctx = ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: Some(1),
    };
    jade.admit(&model, &op).unwrap();
    let effects = model.apply(&op, &ctx).unwrap();
    jade.materialize(&model, effects, &ctx).unwrap();
    assert_exact(&model, jade.workbook());
    let reopened = HeadlessWorkbook::open(&jade.export().unwrap()).unwrap();
    let reimported = XlsxModel::from_workbook(&reopened, SMALL).unwrap();
    assert_exact(&model, &reopened);
    assert_exact(&reimported, &reopened);
    assert_eq!(
        reopened.format_at("Operations", "E6").unwrap().bold,
        Some(true)
    );
    assert_eq!(
        reopened.format_at("Operations", "E500").unwrap().italic,
        Some(false)
    );
}

#[test]
fn importing_axis_defaults_does_not_expand_the_entire_grid_into_cell_registers() {
    let mut book = source();
    book.set_format(
        "Operations",
        "A:XFD",
        &FormatChange {
            bold: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let model = XlsxModel::from_workbook(
        &book,
        LoadOptions {
            min_rows: 10_000,
            min_columns: 100,
        },
    )
    .unwrap();
    let (sheet, row, column) = model.locate("Operations", "CV10000").unwrap();
    assert_eq!(model.format(sheet, row, column).bold, Some(true));
    let serialized = serde_json::to_value(&model).unwrap();
    let cells = serialized["sheets"][0]["formats"].as_array().unwrap();
    assert!(
        cells.len() < 500,
        "a sparse source must remain sparse: {} cell formats",
        cells.len()
    );
    let source = &serialized["sheets"][0]["source_formats"];
    assert!(source["cells"].as_array().unwrap().len() < 500);
    assert!(source["styles"].as_object().unwrap().len() < 20);
}

const OPTIONS: LoadOptions = LoadOptions {
    min_rows: 24,
    min_columns: 12,
};
const REGION: [(&str, usize, usize); 4] = [
    ("J20", 19, 9),
    ("K20", 19, 10),
    ("J21", 20, 9),
    ("K21", 20, 10),
];
type Write = (Id, u8, usize, FormatChange);

fn check_history(
    sim: &Simulation<XlsxModel, JadeXlsx>,
    baseline: &[FormatChange],
    writes: &[Write],
) {
    let mut ordered = writes.to_vec();
    ordered.sort_by_key(|w| w.0);
    for (i, (reference, r, c)) in REGION.iter().enumerate() {
        let mut expected = baseline[i].clone();
        for (_, kind, target, change) in &ordered {
            let (_, wr, wc) = REGION[*target];
            if (*kind == 0 && *target == i) || (*kind == 1 && wr == *r) || (*kind == 2 && wc == *c)
            {
                expected = common::overlay(&expected, change);
            }
        }
        let model = sim.server().document();
        let (sheet, row, column) = model.locate("Operations", reference).unwrap();
        assert_eq!(
            model.format(sheet, row, column),
            expected,
            "reference at {reference}"
        );
        assert_eq!(
            common::masked(
                &sim.server()
                    .materializer()
                    .workbook()
                    .format_at("Operations", reference)
                    .unwrap(),
                &expected
            ),
            expected,
            "file at {reference}"
        );
    }
    for client in 0..3 {
        assert_eq!(sim.client(client).view(), sim.server().document());
    }
    assert!(sim.server().faults().is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// The reference is four complete native source styles plus an ordered
    /// list of partial writes, independent of the model's layered registers.
    #[test]
    fn source_styles_and_partial_concurrent_writes_survive_networks_checkpoints_and_reimports(
        steps in prop::collection::vec((0u8..9, 0usize..3, 0usize..4, any::<bool>()), 1..30),
    ) {
        let mut book = source();
        for (range, change) in [
            ("J:J", FormatChange { italic: Some(true), font_color: Some("123456".into()), ..Default::default() }),
            ("20:20", FormatChange { bold: Some(true), ..Default::default() }),
            ("J20", FormatChange { number_format: Some("0.00".into()), ..Default::default() }),
        ] { book.set_format("Operations", range, &change).unwrap(); }
        let mut baseline: Vec<_> = REGION.iter().map(|(r, ..)| book.format_at("Operations", r).unwrap()).collect();
        let model = XlsxModel::from_workbook(&book, OPTIONS).unwrap();
        let mut sim = Simulation::with_materializer(model, JadeXlsx::new(book), 3);
        let mut writes = Vec::new();
        for (kind, client, target, on) in steps {
            match kind {
                0..=2 => {
                    let view = sim.client(client).view();
                    let (sheet, row, column) = view.locate("Operations", REGION[target].0).unwrap();
                    let change = match target {
                        0 => FormatChange { bold: Some(on), ..Default::default() },
                        1 => FormatChange { italic: Some(on), fill_color: Some(if on { "FF0000" } else { "" }.into()), ..Default::default() },
                        2 => FormatChange { border_top: Some(if on { BorderLine { style: BorderStyle::Thin, color: None } } else { BorderLine::none() }), ..Default::default() },
                        _ => FormatChange { number_format: Some(if on { "0.000" } else { "General" }.into()), ..Default::default() },
                    };
                    let stamp = sim.client_mut(client).reserve(1);
                    let op = match kind {
                        0 => XlsxOp::SetFormat { sheet, rows: vec![row], columns: vec![column], stamp, change: change.clone() },
                        _ => XlsxOp::SetLineFormat { sheet, axis: if kind == 1 { LineAxis::Rows } else { LineAxis::Columns }, lines: vec![if kind == 1 { row } else { column }], stamp, change: change.clone() },
                    };
                    sim.local(client, op).unwrap();
                    writes.push((stamp, kind, target, change));
                }
                3 => { sim.deliver_up(client); }
                4 => { sim.deliver_down(client).unwrap(); }
                8 => { sim.reconnect().unwrap(); }
                _ => {
                    sim.quiesce().unwrap();
                    check_history(&sim, &baseline, &writes);
                    let bytes = sim.server().materializer().export().unwrap();
                    if kind == 5 {
                        let checkpoint = sim.checkpoint();
                        let checkpoint = serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
                        let restored = Sequencer::restore(checkpoint, JadeXlsx::open(&bytes).unwrap());
                        sim.restart(restored).unwrap();
                    } else {
                        let jade = JadeXlsx::open(&bytes).unwrap();
                        let model = XlsxModel::from_workbook(jade.workbook(), OPTIONS).unwrap();
                        baseline = REGION.iter().map(|(r, ..)| jade.workbook().format_at("Operations", r).unwrap()).collect();
                        writes.clear();
                        sim = Simulation::with_materializer(model, jade, 3);
                    }
                }
            }
        }
        sim.quiesce().unwrap();
        check_history(&sim, &baseline, &writes);
    }
}
