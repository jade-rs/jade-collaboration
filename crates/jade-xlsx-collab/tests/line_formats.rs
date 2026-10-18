//! Whole-row and whole-column formats (Excel's `<row s=…>` and
//! `<col style=…>`): a register per line and attribute. A cell shows, for
//! each attribute, the newest write among its own, its row's and its
//! column's — whatever order they arrive in — and cells a line gains later
//! (a row inserted under a formatted column) show the line's format, as in
//! jade.

mod common;

use common::{assert_agree, masked, open};
use jade_collab_core::grid::GridOp;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_xlsx_collab::intent::{resolve, XlsxIntent};
use jade_xlsx_collab::{
    BorderLine, BorderStyle, FormatChange, JadeXlsx, LineAxis, XlsxEffect, XlsxModel, XlsxOp,
};
use proptest::prelude::*;

fn ctx(seq: u64) -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: seq.saturating_sub(1),
        seq: Some(seq),
    }
}

fn id(n: u64) -> Id {
    Id::new(1000 + n, ReplicaId(1))
}

fn bold(value: bool) -> FormatChange {
    FormatChange {
        bold: Some(value),
        ..FormatChange::default()
    }
}

fn italic(value: bool) -> FormatChange {
    FormatChange {
        italic: Some(value),
        ..FormatChange::default()
    }
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

fn cell(s: &Sheet, r: usize, c: usize, stamp: Id, change: FormatChange) -> XlsxOp {
    XlsxOp::SetFormat {
        sheet: s.sheet,
        rows: vec![s.rows[r]],
        columns: vec![s.columns[c]],
        stamp,
        change,
    }
}

fn rows(s: &Sheet, rows: &[usize], stamp: Id, change: FormatChange) -> XlsxOp {
    XlsxOp::SetLineFormat {
        sheet: s.sheet,
        axis: LineAxis::Rows,
        lines: rows.iter().map(|r| s.rows[*r]).collect(),
        stamp,
        change,
    }
}

fn columns(s: &Sheet, columns: &[usize], stamp: Id, change: FormatChange) -> XlsxOp {
    XlsxOp::SetLineFormat {
        sheet: s.sheet,
        axis: LineAxis::Columns,
        lines: columns.iter().map(|c| s.columns[*c]).collect(),
        stamp,
        change,
    }
}

#[test]
fn line_formats_are_convergent() {
    let s = blank();
    assert_eq!(
        XlsxModel::lane(&columns(&s, &[0], id(1), bold(true))),
        Lane::Convergent
    );
}

#[test]
fn a_column_format_reaches_every_cell_of_the_column() {
    let mut s = blank();
    let effect = s
        .model
        .apply(&columns(&s, &[1, 2], id(1), bold(true)), &ctx(1))
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetFormat {
            sheet: "Sheet1".into(),
            range: "B:C".into(),
            change: bold(true),
        }]
    );
    for r in 0..4 {
        assert_eq!(s.model.format(s.sheet, s.rows[r], s.columns[1]), bold(true));
        assert_eq!(
            s.model.format(s.sheet, s.rows[r], s.columns[0]),
            FormatChange::default()
        );
    }
    let effect = s
        .model
        .apply(&rows(&s, &[2], id(2), italic(true)), &ctx(2))
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetFormat {
            sheet: "Sheet1".into(),
            range: "3:3".into(),
            change: italic(true),
        }]
    );
    assert_eq!(
        s.model.format(s.sheet, s.rows[2], s.columns[1]),
        FormatChange {
            bold: Some(true),
            italic: Some(true),
            ..FormatChange::default()
        }
    );
}

/// The newest of a cell's, its row's and its column's write shows, in any
/// arrival order; jade, told only what won in log order, ends there too:
/// a line write that loses at some cells is followed by those cells'
/// newer values.
#[test]
fn the_newest_of_cell_row_and_column_wins_in_every_order() {
    let s = blank();
    let writes = [
        columns(&s, &[1], id(2), bold(true)),
        rows(&s, &[1], id(3), bold(false)),
        cell(&s, 1, 1, id(1), bold(true)),
        cell(&s, 2, 1, id(4), bold(false)),
    ];
    let orders: [[usize; 4]; 6] = [
        [0, 1, 2, 3],
        [1, 0, 3, 2],
        [2, 3, 0, 1],
        [3, 2, 1, 0],
        [1, 2, 3, 0],
        [2, 0, 3, 1],
    ];
    for order in orders {
        let mut s = blank();
        for (seq, i) in order.iter().enumerate() {
            s.model.apply(&writes[*i], &ctx(seq as u64 + 1)).unwrap();
        }
        let at = |r: usize, c: usize| s.model.format(s.sheet, s.rows[r], s.columns[c]).bold;
        // B2: the row (3) beats the column (2) and the cell (1).
        assert_eq!(at(1, 1), Some(false), "{order:?}");
        // B3: its own write (4) beats the column (2).
        assert_eq!(at(2, 1), Some(false), "{order:?}");
        // B1, B4: the column.
        assert_eq!(at(0, 1), Some(true), "{order:?}");
        assert_eq!(at(3, 1), Some(true), "{order:?}");
        // A2: the row.
        assert_eq!(at(1, 0), Some(false), "{order:?}");
        assert_eq!(at(0, 0), None, "{order:?}");
    }
}

#[test]
fn a_line_write_that_loses_at_a_cell_restores_it_after_jade_s_line_format() {
    let mut s = blank();
    s.model
        .apply(&cell(&s, 1, 1, id(5), bold(false)), &ctx(1))
        .unwrap();
    let effect = s
        .model
        .apply(&columns(&s, &[1], id(4), bold(true)), &ctx(2))
        .unwrap();
    assert_eq!(
        effect,
        vec![
            XlsxEffect::SetFormat {
                sheet: "Sheet1".into(),
                range: "B:B".into(),
                change: bold(true),
            },
            XlsxEffect::SetFormat {
                sheet: "Sheet1".into(),
                range: "B2".into(),
                change: bold(false),
            },
        ]
    );
    // An older cell write under a newer line write reaches nothing.
    let effect = s
        .model
        .apply(&cell(&s, 2, 1, id(3), bold(false)), &ctx(3))
        .unwrap();
    assert_eq!(effect, Vec::new());
    assert_eq!(s.model.format(s.sheet, s.rows[2], s.columns[1]), bold(true));
}

#[test]
fn line_formats_follow_jade_s_rules_for_lines_inserted_later() {
    let mut s = blank();
    s.model
        .apply(&columns(&s, &[1], id(1), bold(true)), &ctx(1))
        .unwrap();
    s.model
        .apply(&rows(&s, &[1], id(2), italic(true)), &ctx(2))
        .unwrap();
    // A new row is under column B: its cell there is bold.
    s.model
        .apply(
            &XlsxOp::Structure {
                sheet: s.sheet,
                op: GridOp::InsertRow {
                    id: id(3),
                    after: Some(s.rows[2]),
                },
            },
            &ctx(3),
        )
        .unwrap();
    assert_eq!(s.model.format(s.sheet, id(3), s.columns[1]), bold(true));
    // A new column crosses row 2: its cell there is italic.
    s.model
        .apply(
            &XlsxOp::Structure {
                sheet: s.sheet,
                op: GridOp::InsertColumn {
                    id: id(4),
                    after: Some(s.columns[2]),
                },
            },
            &ctx(4),
        )
        .unwrap();
    assert_eq!(s.model.format(s.sheet, s.rows[1], id(4)), italic(true));
}

#[test]
fn line_formats_are_refused_like_cell_formats() {
    let s = blank();
    for op in [
        rows(&s, &[], id(1), bold(true)),
        columns(&s, &[0], id(1), FormatChange::default()),
        columns(
            &s,
            &[0],
            id(1),
            FormatChange {
                fill_color: Some("blue".into()),
                ..FormatChange::default()
            },
        ),
        XlsxOp::SetLineFormat {
            sheet: s.sheet,
            axis: LineAxis::Rows,
            lines: vec![s.columns[0]],
            stamp: id(1),
            change: bold(true),
        },
    ] {
        let mut model = s.model.clone();
        assert!(model.apply(&op, &ctx(1)).is_err(), "{op:?}");
        assert_eq!(model, s.model);
    }
}

#[test]
fn line_formats_round_trip_through_json_and_older_checkpoints_still_load() {
    let mut s = blank();
    let op = rows(&s, &[0, 1], id(1), italic(true));
    s.model.apply(&op, &ctx(1)).unwrap();
    let json = serde_json::to_string(&s.model).unwrap();
    assert_eq!(serde_json::from_str::<XlsxModel>(&json).unwrap(), s.model);
    let op_json = serde_json::to_string(&op).unwrap();
    assert_eq!(serde_json::from_str::<XlsxOp>(&op_json).unwrap(), op);

    let s = blank();
    let mut value = serde_json::to_value(&s.model).unwrap();
    for sheet in value["sheets"].as_array_mut().unwrap() {
        let sheet = sheet.as_object_mut().unwrap();
        assert!(sheet.remove("row_formats").is_some());
        assert!(sheet.remove("column_formats").is_some());
    }
    assert_eq!(serde_json::from_value::<XlsxModel>(value).unwrap(), s.model);
}

#[test]
fn set_format_on_whole_rows_or_columns_resolves_to_line_formats() {
    let s = blank();
    let mut next = 50;
    let mut ids = |n: u64| {
        next += n;
        Id::new(next, ReplicaId(2))
    };
    let intent = |range: &str| -> XlsxIntent {
        serde_json::from_value(serde_json::json!({
            "type": "set_format", "sheet": "Sheet1", "range": range, "change": { "bold": true }
        }))
        .unwrap()
    };
    match resolve(&s.model, &intent("B:C"), &mut ids).unwrap() {
        XlsxOp::SetLineFormat { axis, lines, .. } => {
            assert_eq!(axis, LineAxis::Columns);
            assert_eq!(lines, s.columns[1..3].to_vec());
        }
        other => panic!("{other:?}"),
    }
    match resolve(&s.model, &intent("4:2"), &mut ids).unwrap() {
        XlsxOp::SetLineFormat { axis, lines, .. } => {
            assert_eq!(axis, LineAxis::Rows);
            assert_eq!(lines, s.rows[1..4].to_vec());
        }
        other => panic!("{other:?}"),
    }
    match resolve(&s.model, &intent("C"), &mut ids) {
        Err(_) | Ok(XlsxOp::SetFormat { .. }) => {}
        other => panic!("{other:?}"),
    }
    for range in ["B:Z", "2:9", "0:1", "B:2"] {
        assert!(
            resolve(&s.model, &intent(range), &mut ids).is_err(),
            "{range}"
        );
    }
}

// ── against jade ────────────────────────────────────────────────────

fn commit(model: &mut XlsxModel, jade: &mut JadeXlsx, seq: &mut u64, op: XlsxOp) {
    *seq += 1;
    let ctx = ctx(*seq);
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

#[test]
fn line_formats_reach_jade_and_new_lines_take_them_as_in_jade() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let (sheet, row3, column_b) = model.locate("Operations", "B3").unwrap();
    let mut seq = 0;
    let fill = FormatChange {
        fill_color: Some("FFFF00".into()),
        border_bottom: Some(BorderLine {
            style: BorderStyle::Thin,
            color: None,
        }),
        ..FormatChange::default()
    };
    let line = |axis, line, n, change| XlsxOp::SetLineFormat {
        sheet,
        axis,
        lines: vec![line],
        stamp: stamp(n),
        change,
    };
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        line(LineAxis::Columns, column_b, 1, bold(true)),
    );
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        line(LineAxis::Rows, row3, 3, fill.clone()),
    );
    // An older row write under the newer column loses at B6.
    let (_, row6, _) = model.locate("Operations", "B6").unwrap();
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        line(LineAxis::Rows, row6, 2, bold(false)),
    );
    assert_eq!(model.format(sheet, row6, column_b).bold, Some(true));
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::Structure {
            sheet,
            op: GridOp::InsertRow {
                id: stamp(4),
                after: Some(row6),
            },
        },
    );
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::Structure {
            sheet,
            op: GridOp::InsertColumn {
                id: stamp(5),
                after: Some(column_b),
            },
        },
    );
    assert_eq!(model.format(sheet, stamp(4), column_b), bold(true));
    assert_eq!(
        model.format(sheet, row3, stamp(5)).fill_color,
        Some("FFFF00".into())
    );
    let reopened = JadeXlsx::open(&jade.export().unwrap()).unwrap();
    assert_agree(&model, &reopened, "after export");
    // Column B's format reaches far past the modelled rows; the column
    // inserted after it is not part of it.
    let far = |reference: &str| {
        reopened
            .workbook()
            .format_at("Operations", reference)
            .unwrap()
    };
    assert_eq!(far("B500").bold, Some(true));
    assert_eq!(far("C500").bold, Some(false));
}

// ── convergence ─────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Three replicas format cells, rows and columns of a corner of the
    /// sheet concurrently. Every replica, the server and jade end with the
    /// newest write per cell and attribute among the cell's, its row's and
    /// its column's.
    #[test]
    fn concurrent_cell_row_and_column_formats_converge_on_the_newest(
        steps in prop::collection::vec(
            (0..3usize, 0..4u8, 0..3u8, 0..3usize, 0..common::ATTRIBUTES, any::<u64>()),
            1..25,
        ),
    ) {
        let (model, jade) = open("grid-operations-workbook.xlsx");
        let view = model.sheets().remove(0);
        let (sheet, rows, columns) = (view.id, view.rows[4..7].to_vec(), view.columns[1..4].to_vec());
        let mut sim = Simulation::with_materializer(model, jade, 3);
        // (stamp, which rows, which columns, change)
        let mut writes: Vec<(Id, Vec<usize>, Vec<usize>, FormatChange)> = Vec::new();
        for (client, kind, target, line, attribute, value) in steps {
            match kind {
                0 | 1 => {
                    let mut change = FormatChange::default();
                    common::set_attribute(&mut change, attribute, value);
                    let stamp = sim.client_mut(client).reserve(1);
                    let (op, r, c) = match target {
                        0 => (
                            XlsxOp::SetFormat {
                                sheet,
                                rows: vec![rows[line]],
                                columns: vec![columns[(value >> 8) as usize % 3]],
                                stamp,
                                change: change.clone(),
                            },
                            vec![line],
                            vec![(value >> 8) as usize % 3],
                        ),
                        1 => (
                            XlsxOp::SetLineFormat {
                                sheet,
                                axis: LineAxis::Rows,
                                lines: vec![rows[line]],
                                stamp,
                                change: change.clone(),
                            },
                            vec![line],
                            vec![0, 1, 2],
                        ),
                        _ => (
                            XlsxOp::SetLineFormat {
                                sheet,
                                axis: LineAxis::Columns,
                                lines: vec![columns[line]],
                                stamp,
                                change: change.clone(),
                            },
                            vec![0, 1, 2],
                            vec![line],
                        ),
                    };
                    prop_assert!(sim.local(client, op).is_ok());
                    writes.push((stamp, r, c, change));
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
        writes.sort_by_key(|(stamp, ..)| *stamp);
        let server = sim.server();
        prop_assert!(server.faults().is_empty(), "{:?}", server.faults());
        for client in 0..3 {
            prop_assert_eq!(sim.client(client).view(), server.document());
        }
        for r in 0..3 {
            for c in 0..3 {
                let expected = writes
                    .iter()
                    .filter(|(_, rs, cs, _)| rs.contains(&r) && cs.contains(&c))
                    .fold(FormatChange::default(), |under, (.., over)| common::overlay(&under, over));
                let ours = server.document().format(sheet, rows[r], columns[c]);
                prop_assert_eq!(&ours, &expected);
                let reference = format!("{}{}", jade_xlsx_collab::column_label(c as u32 + 1), r + 5);
                let theirs = server.materializer().workbook().format_at("Operations", &reference).unwrap();
                prop_assert_eq!(masked(&theirs, &expected), expected);
            }
        }
        assert_agree(server.document(), server.materializer(), "after the session");
    }
}
