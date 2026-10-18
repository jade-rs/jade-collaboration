use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_xlsx_collab::intent::{resolve, XlsxIntent};
use jade_xlsx_collab::{BorderLine, BorderStyle, CellInput, FormatChange, XlsxModel, XlsxOp};

fn apply(model: &mut XlsxModel, intent: &str) -> Result<(), String> {
    let intent: XlsxIntent = serde_json::from_str(intent).map_err(|e| e.to_string())?;
    let mut next = model.max_lamport() + 1;
    let op = resolve(model, &intent, &mut |n| {
        let id = Id::new(next, ReplicaId(3));
        next += n;
        id
    })
    .map_err(|e| e.to_string())?;
    let ctx = ApplyContext {
        origin: ReplicaId(3),
        base_seq: 0,
        seq: None,
    };
    model.apply(&op, &ctx).map(drop).map_err(|e| e.to_string())
}

fn at(model: &XlsxModel, reference: &str) -> Option<CellInput> {
    let (sheet, row, column) = model.locate("S", reference)?;
    model.value(sheet, row, column).cloned()
}

#[test]
fn set_cells_by_a1_reference() {
    let mut model = XlsxModel::blank(&[("S", 5, 3)]);
    apply(
        &mut model,
        r#"{"type":"set_cell","sheet":"S","reference":"B2","value":{"kind":"number","value":3}}"#,
    )
    .unwrap();
    apply(&mut model, r#"{"type":"set_cell","sheet":"S","reference":"C1","value":{"kind":"formula","value":"=B2*2"}}"#).unwrap();
    assert_eq!(at(&model, "B2"), Some(CellInput::Number(3.0)));
    assert_eq!(at(&model, "C1"), Some(CellInput::Formula("B2*2".into())));
}

#[test]
fn insert_and_delete_rows_and_columns_by_position() {
    let mut model = XlsxModel::blank(&[("S", 3, 3)]);
    apply(
        &mut model,
        r#"{"type":"set_cell","sheet":"S","reference":"A2","value":{"kind":"text","value":"x"}}"#,
    )
    .unwrap();
    apply(
        &mut model,
        r#"{"type":"insert_row","sheet":"S","before":1}"#,
    )
    .unwrap();
    assert_eq!(at(&model, "A3"), Some(CellInput::Text("x".into())));
    apply(
        &mut model,
        r#"{"type":"insert_column","sheet":"S","before":"A"}"#,
    )
    .unwrap();
    assert_eq!(at(&model, "B3"), Some(CellInput::Text("x".into())));
    apply(&mut model, r#"{"type":"delete_row","sheet":"S","row":1}"#).unwrap();
    apply(
        &mut model,
        r#"{"type":"delete_column","sheet":"S","column":"A"}"#,
    )
    .unwrap();
    assert_eq!(at(&model, "A2"), Some(CellInput::Text("x".into())));
}

#[test]
fn rejects_unknown_sheets_and_cells() {
    let mut model = XlsxModel::blank(&[("S", 2, 2)]);
    assert!(apply(
        &mut model,
        r#"{"type":"set_cell","sheet":"T","reference":"A1","value":{"kind":"blank"}}"#
    )
    .is_err());
    assert!(apply(
        &mut model,
        r#"{"type":"set_cell","sheet":"S","reference":"Z99","value":{"kind":"blank"}}"#
    )
    .is_err());
    assert!(apply(&mut model, r#"{"type":"delete_row","sheet":"S","row":0}"#).is_err());
}

fn note_at(model: &XlsxModel, reference: &str) -> Option<String> {
    let (sheet, row, column) = model.locate("S", reference)?;
    model.note(sheet, row, column).map(str::to_string)
}

fn format_at(model: &XlsxModel, reference: &str) -> FormatChange {
    let (sheet, row, column) = model.locate("S", reference).unwrap();
    model.format(sheet, row, column)
}

#[test]
fn set_and_delete_notes_by_a1_reference() {
    let mut model = XlsxModel::blank(&[("S", 3, 3)]);
    apply(
        &mut model,
        r#"{"type":"set_note","sheet":"S","reference":"B2","text":"check this"}"#,
    )
    .unwrap();
    assert_eq!(note_at(&model, "B2"), Some("check this".into()));
    apply(
        &mut model,
        r#"{"type":"delete_note","sheet":"S","reference":"B2"}"#,
    )
    .unwrap();
    assert_eq!(note_at(&model, "B2"), None);
    assert!(apply(
        &mut model,
        r#"{"type":"set_note","sheet":"S","reference":"D9","text":"x"}"#
    )
    .is_err());
}

#[test]
fn format_a_range_or_a_cell_by_a1_reference() {
    let mut model = XlsxModel::blank(&[("S", 4, 4)]);
    apply(
        &mut model,
        r#"{"type":"set_format","sheet":"S","range":"B2:C3","change":{"bold":true,"fill_color":"FFFF00"}}"#,
    )
    .unwrap();
    let expected = FormatChange {
        bold: Some(true),
        fill_color: Some("FFFF00".into()),
        ..FormatChange::default()
    };
    for reference in ["B2", "B3", "C2", "C3"] {
        assert_eq!(format_at(&model, reference), expected, "{reference}");
    }
    for reference in ["A1", "D2", "B4"] {
        assert_eq!(
            format_at(&model, reference),
            FormatChange::default(),
            "{reference}"
        );
    }
    // Corners in either order; a single cell.
    apply(
        &mut model,
        r#"{"type":"set_format","sheet":"S","range":"D4:C4","change":{"italic":true}}"#,
    )
    .unwrap();
    apply(
        &mut model,
        r#"{"type":"set_format","sheet":"S","range":"A1","change":{"number_format":"0.00"}}"#,
    )
    .unwrap();
    assert_eq!(format_at(&model, "C4").italic, Some(true));
    assert_eq!(format_at(&model, "D4").italic, Some(true));
    assert_eq!(format_at(&model, "A1").number_format, Some("0.00".into()));
}

#[test]
fn draw_borders_by_a1_range() {
    let mut model = XlsxModel::blank(&[("S", 4, 4)]);
    apply(
        &mut model,
        r##"{"type":"set_borders","sheet":"S","range":"B2:C3","borders":{"outline":{"style":"medium","color":"#c00000"},"inside":{"style":"thin"}}}"##,
    )
    .unwrap();
    let medium = Some(BorderLine {
        style: BorderStyle::Medium,
        color: Some("C00000".into()),
    });
    let thin = Some(BorderLine {
        style: BorderStyle::Thin,
        color: None,
    });
    assert_eq!(
        format_at(&model, "B2"),
        FormatChange {
            border_top: medium.clone(),
            border_left: medium.clone(),
            border_bottom: thin.clone(),
            border_right: thin.clone(),
            ..FormatChange::default()
        }
    );
    assert_eq!(
        format_at(&model, "C3"),
        FormatChange {
            border_top: thin.clone(),
            border_left: thin,
            border_bottom: medium.clone(),
            border_right: medium,
            ..FormatChange::default()
        }
    );
    assert_eq!(format_at(&model, "D4"), FormatChange::default());

    // Every cell of a set_format takes the edges it names.
    apply(
        &mut model,
        r#"{"type":"set_format","sheet":"S","range":"A4:B4","change":{"border_bottom":{"style":"double"},"diagonal_up":true}}"#,
    )
    .unwrap();
    for reference in ["A4", "B4"] {
        let format = format_at(&model, reference);
        assert_eq!(
            format.border_bottom.map(|l| l.style),
            Some(BorderStyle::Double)
        );
        assert_eq!(format.diagonal_up, Some(true));
    }

    // A style OOXML does not have, and an unknown key, are not understood.
    for intent in [
        r#"{"type":"set_borders","sheet":"S","range":"A1","borders":{"outline":{"style":"wavy"}}}"#,
        r#"{"type":"set_borders","sheet":"S","range":"A1","borders":{"around":{"style":"thin"}}}"#,
    ] {
        assert!(apply(&mut model, intent).is_err(), "{intent}");
    }
    // Nothing named, or outside the sheet.
    for intent in [
        r#"{"type":"set_borders","sheet":"S","range":"A1","borders":{}}"#,
        r#"{"type":"set_borders","sheet":"S","range":"A1:E5","borders":{"outline":{"style":"thin"}}}"#,
    ] {
        assert!(apply(&mut model, intent).is_err(), "{intent}");
    }
}

/// A range names the rows and columns the author saw, so it means the same
/// cells wherever it lands in the log.
#[test]
fn a_range_intent_resolves_to_the_ids_it_covers() {
    let model = XlsxModel::blank(&[("S", 4, 4)]);
    let intent: XlsxIntent = serde_json::from_str(
        r#"{"type":"set_format","sheet":"S","range":"B2:C3","change":{"bold":true}}"#,
    )
    .unwrap();
    let op = resolve(&model, &intent, &mut |_| Id::new(99, ReplicaId(3))).unwrap();
    let sheet = &model.sheets()[0];
    assert_eq!(
        op,
        XlsxOp::SetFormat {
            sheet: sheet.id,
            rows: sheet.rows[1..3].to_vec(),
            columns: sheet.columns[1..3].to_vec(),
            stamp: Id::new(99, ReplicaId(3)),
            change: FormatChange {
                bold: Some(true),
                ..FormatChange::default()
            },
        }
    );
}

#[test]
fn rejects_ranges_outside_the_sheet() {
    let mut model = XlsxModel::blank(&[("S", 2, 2)]);
    for range in ["A1:C1", "Z99", "A0", "A1:", "nonsense"] {
        let intent = format!(
            r#"{{"type":"set_format","sheet":"S","range":"{range}","change":{{"bold":true}}}}"#
        );
        assert!(apply(&mut model, &intent).is_err(), "{range}");
    }
    assert!(apply(
        &mut model,
        r#"{"type":"set_format","sheet":"T","range":"A1","change":{"bold":true}}"#
    )
    .is_err());
}

#[test]
fn merge_and_sheet_intents() {
    let mut model = XlsxModel::blank(&[("S", 4, 4)]);
    apply(
        &mut model,
        r#"{"type":"merge_cells","sheet":"S","range":"A1:B2"}"#,
    )
    .unwrap();
    assert!(apply(
        &mut model,
        r#"{"type":"merge_cells","sheet":"S","range":"B2:C3"}"#
    )
    .is_err());
    apply(
        &mut model,
        r#"{"type":"unmerge_cells","sheet":"S","range":"A1:B2"}"#,
    )
    .unwrap();
    apply(&mut model, r#"{"type":"add_sheet","name":"T","after":"S"}"#).unwrap();
    apply(
        &mut model,
        r#"{"type":"rename_sheet","sheet":"T","name":"Totals"}"#,
    )
    .unwrap();
    apply(
        &mut model,
        r#"{"type":"move_sheet","sheet":"Totals","to":0}"#,
    )
    .unwrap();
    let names: Vec<String> = model.sheets().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["Totals", "S"]);
    apply(&mut model, r#"{"type":"delete_sheet","sheet":"S"}"#).unwrap();
    assert!(apply(&mut model, r#"{"type":"delete_sheet","sheet":"Totals"}"#).is_err());
}

#[test]
fn setting_a_table_header_renames_the_column() {
    use jade_xlsx_collab::XlsxOp;
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../jade-core/public/fixtures/grid-operations-workbook.xlsx"
    ))
    .unwrap();
    let jade = jade_xlsx_collab::JadeXlsx::open(&bytes).unwrap();
    let model = XlsxModel::from_workbook(jade.workbook(), Default::default()).unwrap();
    let intent: XlsxIntent = serde_json::from_str(
        r#"{"type":"set_cell","sheet":"Operations","reference":"A1","value":{"kind":"text","value":"Label"}}"#,
    )
    .unwrap();
    let mut next = model.max_lamport() + 1;
    let op = resolve(&model, &intent, &mut |n| {
        let id = Id::new(next, ReplicaId(3));
        next += n;
        id
    })
    .unwrap();
    assert!(
        matches!(op, XlsxOp::SetHeader { ref text, .. } if text == "Label"),
        "{op:?}"
    );
    // A header holds text only.
    let number: XlsxIntent = serde_json::from_str(
        r#"{"type":"set_cell","sheet":"Operations","reference":"A1","value":{"kind":"number","value":1}}"#,
    )
    .unwrap();
    assert!(resolve(&model, &number, &mut |_| Id::new(1, ReplicaId(3))).is_err());
}
