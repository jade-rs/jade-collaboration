use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_xlsx_collab::intent::{resolve, XlsxIntent};
use jade_xlsx_collab::{CellInput, XlsxModel};

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
