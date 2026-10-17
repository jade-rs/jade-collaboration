use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_xlsx_collab::{column_label, CellInput, XlsxEffect, XlsxModel, XlsxOp};

fn ctx(origin: u64, base_seq: u64, seq: Option<u64>) -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(origin),
        base_seq,
        seq,
    }
}

fn id(n: u64) -> Id {
    Id::new(1000 + n, ReplicaId(1))
}

struct Sheet {
    model: XlsxModel,
    sheet: Id,
    rows: Vec<Id>,
    columns: Vec<Id>,
}

fn blank() -> Sheet {
    let model = XlsxModel::blank(&[("Sheet1", 4, 3)]);
    let view = model.sheets().remove(0);
    Sheet {
        sheet: view.id,
        rows: view.rows,
        columns: view.columns,
        model,
    }
}

fn set(s: &Sheet, r: usize, c: usize, stamp: Id, value: CellInput) -> XlsxOp {
    XlsxOp::SetCell {
        sheet: s.sheet,
        row: s.rows[r],
        column: s.columns[c],
        stamp,
        value,
    }
}

#[test]
fn column_labels() {
    assert_eq!(column_label(0), "A");
    assert_eq!(column_label(25), "Z");
    assert_eq!(column_label(26), "AA");
    assert_eq!(column_label(701), "ZZ");
    assert_eq!(column_label(702), "AAA");
}

#[test]
fn setting_a_cell_names_it_in_a1() {
    let mut s = blank();
    let op = set(&s, 0, 1, id(1), CellInput::Number(4.0));
    let effect = s.model.apply(&op, &ctx(1, 0, Some(1))).unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetCell {
            sheet: "Sheet1".into(),
            reference: "B1".into(),
            value: CellInput::Number(4.0)
        }]
    );
    assert_eq!(
        s.model.value(s.sheet, s.rows[0], s.columns[1]),
        Some(&CellInput::Number(4.0))
    );
}

#[test]
fn an_older_write_changes_nothing() {
    let mut s = blank();
    s.model
        .apply(
            &set(&s, 0, 0, id(5), CellInput::Text("new".into())),
            &ctx(1, 0, Some(1)),
        )
        .unwrap();
    let effect = s
        .model
        .apply(
            &set(&s, 0, 0, id(2), CellInput::Text("old".into())),
            &ctx(2, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(effect, vec![]);
    assert_eq!(
        s.model.value(s.sheet, s.rows[0], s.columns[0]),
        Some(&CellInput::Text("new".into()))
    );
}

#[test]
fn lanes() {
    let s = blank();
    assert_eq!(
        XlsxModel::lane(&set(&s, 0, 0, id(1), CellInput::Number(1.0))),
        Lane::Convergent
    );
    assert_eq!(
        XlsxModel::lane(&set(&s, 0, 0, id(1), CellInput::Blank)),
        Lane::Convergent
    );
    assert_eq!(
        XlsxModel::lane(&set(&s, 0, 0, id(1), CellInput::Formula("A2".into()))),
        Lane::Sequenced
    );
    assert_eq!(
        XlsxModel::lane(&XlsxOp::Structure {
            sheet: s.sheet,
            op: GridOp::DeleteRow { id: s.rows[0] }
        }),
        Lane::Sequenced
    );
}

#[test]
fn inserting_a_row_moves_references_like_jade_does() {
    let mut s = blank();
    s.model
        .apply(
            &set(&s, 0, 0, id(1), CellInput::Formula("=A3*2".into())),
            &ctx(1, 0, Some(1)),
        )
        .unwrap();
    let effect = s
        .model
        .apply(
            &XlsxOp::Structure {
                sheet: s.sheet,
                op: GridOp::InsertRow {
                    id: id(2),
                    after: Some(s.rows[0]),
                },
            },
            &ctx(1, 1, Some(2)),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::InsertRow {
            sheet: "Sheet1".into(),
            at: 1
        }]
    );
    assert_eq!(
        s.model.value(s.sheet, s.rows[0], s.columns[0]),
        Some(&CellInput::Formula("A4*2".into())),
        "stored without '=' and shifted"
    );
}

#[test]
fn a_formula_written_against_an_older_grid_is_rebased() {
    let mut s = blank();
    // Replica 2 inserts a row above row 3 (committed at seq 1)…
    s.model
        .apply(
            &XlsxOp::Structure {
                sheet: s.sheet,
                op: GridOp::InsertRow {
                    id: id(2),
                    after: Some(s.rows[1]),
                },
            },
            &ctx(2, 0, Some(1)),
        )
        .unwrap();
    // …while replica 1, still at seq 0, typed a formula pointing at row 3.
    let effect = s
        .model
        .apply(
            &set(&s, 0, 0, id(3), CellInput::Formula("A3".into())),
            &ctx(1, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetCell {
            sheet: "Sheet1".into(),
            reference: "A1".into(),
            value: CellInput::Formula("A4".into())
        }]
    );
}

#[test]
fn a_formula_is_not_moved_by_its_authors_own_earlier_changes() {
    let mut s = blank();
    s.model
        .apply(
            &XlsxOp::Structure {
                sheet: s.sheet,
                op: GridOp::InsertRow {
                    id: id(2),
                    after: Some(s.rows[1]),
                },
            },
            &ctx(1, 0, Some(1)),
        )
        .unwrap();
    s.model
        .apply(
            &set(&s, 0, 0, id(3), CellInput::Formula("A3".into())),
            &ctx(1, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(
        s.model.value(s.sheet, s.rows[0], s.columns[0]),
        Some(&CellInput::Formula("A3".into()))
    );
}

#[test]
fn a_value_in_a_deleted_row_is_kept_but_not_shown() {
    let mut s = blank();
    s.model
        .apply(
            &XlsxOp::Structure {
                sheet: s.sheet,
                op: GridOp::DeleteRow { id: s.rows[2] },
            },
            &ctx(2, 0, Some(1)),
        )
        .unwrap();
    let effect = s
        .model
        .apply(
            &set(&s, 2, 0, id(4), CellInput::Number(1.0)),
            &ctx(1, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(effect, vec![]);
    assert_eq!(s.model.sheets()[0].rows.len(), 3);
}

#[test]
fn merges_are_refused() {
    let mut s = blank();
    let err = s
        .model
        .apply(
            &XlsxOp::Structure {
                sheet: s.sheet,
                op: GridOp::Merge(Merge {
                    top: s.rows[0],
                    left: s.columns[0],
                    bottom: s.rows[1],
                    right: s.columns[0],
                }),
            },
            &ctx(1, 0, Some(1)),
        )
        .unwrap_err();
    assert!(err.message().contains("merge"), "{err}");
}

#[test]
fn unknown_cells_are_refused() {
    let mut s = blank();
    let op = XlsxOp::SetCell {
        sheet: s.sheet,
        row: id(77),
        column: s.columns[0],
        stamp: id(78),
        value: CellInput::Number(1.0),
    };
    assert!(s.model.apply(&op, &ctx(1, 0, None)).is_err());
}

#[test]
fn the_model_round_trips_through_json() {
    let mut s = blank();
    s.model
        .apply(
            &set(&s, 1, 1, id(1), CellInput::Text("t".into())),
            &ctx(1, 0, Some(1)),
        )
        .unwrap();
    let json = serde_json::to_string(&s.model).unwrap();
    assert_eq!(serde_json::from_str::<XlsxModel>(&json).unwrap(), s.model);
}
