//! Cell borders: one last-writer-wins register per edge (style and colour
//! together), so concurrent changes to different edges of a cell both
//! survive; `SetBorders` draws Excel's outline / inside lines over a range.

mod common;

use common::{assert_agree, open};
use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::sim::Simulation;
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_xlsx_collab::{
    BorderLine, BorderStyle, FormatChange, JadeXlsx, RangeBorders, XlsxEffect, XlsxModel, XlsxOp,
};
use proptest::prelude::*;

fn ctx(origin: u64, seq: u64) -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(origin),
        base_seq: seq.saturating_sub(1),
        seq: Some(seq),
    }
}

fn id(n: u64) -> Id {
    Id::new(1000 + n, ReplicaId(1))
}

fn line(style: BorderStyle, color: Option<&str>) -> BorderLine {
    BorderLine {
        style,
        color: color.map(str::to_string),
    }
}

fn thin() -> BorderLine {
    line(BorderStyle::Thin, None)
}

struct Sheet {
    model: XlsxModel,
    sheet: Id,
    rows: Vec<Id>,
    columns: Vec<Id>,
}

fn blank() -> Sheet {
    let model = XlsxModel::blank(&[("Sheet1", 5, 4)]);
    let view = model.sheets().remove(0);
    Sheet {
        sheet: view.id,
        rows: view.rows,
        columns: view.columns,
        model,
    }
}

fn format(s: &Sheet, r: usize, c: usize, stamp: Id, change: FormatChange) -> XlsxOp {
    XlsxOp::SetFormat {
        sheet: s.sheet,
        rows: vec![s.rows[r]],
        columns: vec![s.columns[c]],
        stamp,
        change,
    }
}

fn borders(s: &Sheet, rows: &[usize], columns: &[usize], stamp: Id, b: RangeBorders) -> XlsxOp {
    XlsxOp::SetBorders {
        sheet: s.sheet,
        rows: rows.iter().map(|r| s.rows[*r]).collect(),
        columns: columns.iter().map(|c| s.columns[*c]).collect(),
        stamp,
        borders: b,
    }
}

/// Only the border attributes of `change`.
fn edges(change: FormatChange) -> FormatChange {
    FormatChange {
        border_left: change.border_left,
        border_right: change.border_right,
        border_top: change.border_top,
        border_bottom: change.border_bottom,
        border_diagonal: change.border_diagonal,
        diagonal_up: change.diagonal_up,
        diagonal_down: change.diagonal_down,
        ..FormatChange::default()
    }
}

#[test]
fn set_borders_is_convergent() {
    let s = blank();
    let op = borders(
        &s,
        &[0],
        &[0],
        id(1),
        RangeBorders {
            outline: Some(thin()),
            ..RangeBorders::default()
        },
    );
    assert_eq!(XlsxModel::lane(&op), Lane::Convergent);
}

#[test]
fn concurrent_changes_to_different_edges_of_a_cell_both_survive() {
    let top = FormatChange {
        border_top: Some(line(BorderStyle::Thick, Some("FF0000"))),
        ..FormatChange::default()
    };
    let left = FormatChange {
        border_left: Some(line(BorderStyle::Dashed, Some("0000FF"))),
        ..FormatChange::default()
    };
    for swap in [false, true] {
        let mut s = blank();
        let mut ops = [
            format(&s, 1, 1, id(2), top.clone()),
            format(&s, 1, 1, id(1), left.clone()),
        ];
        if swap {
            ops.reverse();
        }
        for (i, op) in ops.iter().enumerate() {
            s.model.apply(op, &ctx(1, i as u64 + 1)).unwrap();
        }
        assert_eq!(
            s.model.format(s.sheet, s.rows[1], s.columns[1]),
            FormatChange {
                border_top: top.border_top.clone(),
                border_left: left.border_left.clone(),
                ..FormatChange::default()
            }
        );
    }
}

#[test]
fn an_edge_s_style_and_colour_travel_together() {
    // The newer write names a style with no colour (automatic): an older
    // write's colour does not leak into it, whatever the order.
    let newer = FormatChange {
        border_bottom: Some(line(BorderStyle::Double, None)),
        ..FormatChange::default()
    };
    let older = FormatChange {
        border_bottom: Some(line(BorderStyle::Thin, Some("00FF00"))),
        ..FormatChange::default()
    };
    for swap in [false, true] {
        let mut s = blank();
        let mut ops = [
            format(&s, 0, 0, id(5), newer.clone()),
            format(&s, 0, 0, id(4), older.clone()),
        ];
        if swap {
            ops.reverse();
        }
        for (i, op) in ops.iter().enumerate() {
            s.model.apply(op, &ctx(1, i as u64 + 1)).unwrap();
        }
        assert_eq!(s.model.format(s.sheet, s.rows[0], s.columns[0]), newer);
    }
}

#[test]
fn an_outline_draws_the_range_s_edges_and_leaves_its_inside_alone() {
    let mut s = blank();
    let medium = line(BorderStyle::Medium, Some("1F3864"));
    let effect = s
        .model
        .apply(
            &borders(
                &s,
                &[1, 2, 3],
                &[0, 1, 2],
                id(1),
                RangeBorders {
                    outline: Some(medium.clone()),
                    ..RangeBorders::default()
                },
            ),
            &ctx(1, 1),
        )
        .unwrap();
    let at = |r: usize, c: usize| edges(s.model.format(s.sheet, s.rows[r], s.columns[c]));
    assert_eq!(
        at(1, 0),
        FormatChange {
            border_top: Some(medium.clone()),
            border_left: Some(medium.clone()),
            ..FormatChange::default()
        }
    );
    assert_eq!(
        at(3, 2),
        FormatChange {
            border_bottom: Some(medium.clone()),
            border_right: Some(medium.clone()),
            ..FormatChange::default()
        }
    );
    assert_eq!(
        at(2, 2),
        FormatChange {
            border_right: Some(medium.clone()),
            ..FormatChange::default()
        }
    );
    // The middle cell, and the cells outside, have nothing.
    assert_eq!(at(2, 1), FormatChange::default());
    assert_eq!(at(0, 0), FormatChange::default());
    assert_eq!(at(1, 3), FormatChange::default());
    // One set_format per rectangle of cells that won the same edges: the
    // middle cell gets none.
    let ranges: Vec<&str> = effect
        .iter()
        .map(|e| match e {
            XlsxEffect::SetFormat { range, .. } => range.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        ranges,
        ["A2", "B2", "C2", "A3", "C3", "A4", "B4", "C4"],
        "{effect:?}"
    );
}

#[test]
fn inside_lines_and_explicit_edges() {
    let mut s = blank();
    let dotted = line(BorderStyle::Dotted, None);
    let thick = line(BorderStyle::Thick, Some("000000"));
    s.model
        .apply(
            &borders(
                &s,
                &[0, 1],
                &[0, 1],
                id(1),
                RangeBorders {
                    outline: Some(thin()),
                    // An explicit edge wins over the outline.
                    bottom: Some(thick.clone()),
                    inside: Some(dotted.clone()),
                    // And an explicit direction over `inside`.
                    inside_vertical: Some(BorderLine::none()),
                    ..RangeBorders::default()
                },
            ),
            &ctx(1, 1),
        )
        .unwrap();
    let at = |r: usize, c: usize| edges(s.model.format(s.sheet, s.rows[r], s.columns[c]));
    assert_eq!(
        at(0, 0),
        FormatChange {
            border_top: Some(thin()),
            border_left: Some(thin()),
            border_bottom: Some(dotted.clone()),
            border_right: Some(BorderLine::none()),
            ..FormatChange::default()
        }
    );
    assert_eq!(
        at(1, 1),
        FormatChange {
            border_top: Some(dotted),
            border_left: Some(BorderLine::none()),
            border_bottom: Some(thick),
            border_right: Some(thin()),
            ..FormatChange::default()
        }
    );
}

#[test]
fn diagonals_apply_to_every_cell() {
    let mut s = blank();
    let red = line(BorderStyle::Thin, Some("FF0000"));
    s.model
        .apply(
            &borders(
                &s,
                &[0, 1],
                &[2],
                id(1),
                RangeBorders {
                    diagonal: Some(red.clone()),
                    diagonal_down: Some(true),
                    ..RangeBorders::default()
                },
            ),
            &ctx(1, 1),
        )
        .unwrap();
    for r in [0, 1] {
        assert_eq!(
            s.model.format(s.sheet, s.rows[r], s.columns[2]),
            FormatChange {
                border_diagonal: Some(red.clone()),
                diagonal_down: Some(true),
                ..FormatChange::default()
            }
        );
    }
}

#[test]
fn border_lines_are_stored_as_jade_reads_them_back_and_bad_ones_are_refused() {
    let mut s = blank();
    let effect = s
        .model
        .apply(
            &format(
                &s,
                0,
                0,
                id(1),
                FormatChange {
                    border_left: Some(line(BorderStyle::Thin, Some("#c0ffee"))),
                    // No line has no colour.
                    border_top: Some(line(BorderStyle::None, Some("FF0000"))),
                    ..FormatChange::default()
                },
            ),
            &ctx(1, 1),
        )
        .unwrap();
    let stored = FormatChange {
        border_left: Some(line(BorderStyle::Thin, Some("C0FFEE"))),
        border_top: Some(BorderLine::none()),
        ..FormatChange::default()
    };
    assert_eq!(
        effect,
        vec![XlsxEffect::SetFormat {
            sheet: "Sheet1".into(),
            range: "A1".into(),
            change: stored.clone(),
        }]
    );
    assert_eq!(s.model.format(s.sheet, s.rows[0], s.columns[0]), stored);

    for op in [
        format(
            &s,
            0,
            0,
            id(2),
            FormatChange {
                border_right: Some(line(BorderStyle::Thin, Some("blue"))),
                ..FormatChange::default()
            },
        ),
        borders(
            &s,
            &[0],
            &[0],
            id(2),
            RangeBorders {
                inside: Some(line(BorderStyle::Thin, Some("12345"))),
                ..RangeBorders::default()
            },
        ),
        // Nothing named.
        borders(&s, &[0], &[0], id(2), RangeBorders::default()),
        // No cells.
        borders(
            &s,
            &[],
            &[0],
            id(2),
            RangeBorders {
                outline: Some(thin()),
                ..RangeBorders::default()
            },
        ),
    ] {
        let before = s.model.clone();
        assert!(s.model.apply(&op, &ctx(1, 2)).is_err(), "{op:?}");
        assert_eq!(s.model, before);
    }
}

#[test]
fn borders_round_trip_through_json_and_older_checkpoints_still_load() {
    let mut s = blank();
    let op = borders(
        &s,
        &[0, 1],
        &[0, 1],
        id(1),
        RangeBorders {
            outline: Some(line(BorderStyle::MediumDashDot, Some("7030A0"))),
            ..RangeBorders::default()
        },
    );
    s.model.apply(&op, &ctx(1, 1)).unwrap();
    let json = serde_json::to_string(&s.model).unwrap();
    assert_eq!(serde_json::from_str::<XlsxModel>(&json).unwrap(), s.model);
    let op_json = serde_json::to_string(&op).unwrap();
    assert_eq!(serde_json::from_str::<XlsxOp>(&op_json).unwrap(), op);

    // A checkpoint written before borders existed has no border registers.
    let mut s = blank();
    s.model
        .apply(
            &format(
                &s,
                0,
                0,
                id(1),
                FormatChange {
                    bold: Some(true),
                    ..FormatChange::default()
                },
            ),
            &ctx(1, 1),
        )
        .unwrap();
    let mut value = serde_json::to_value(&s.model).unwrap();
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                map.retain(|key, _| !key.starts_with("border_") && !key.starts_with("diagonal_"));
                map.values_mut().for_each(strip);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut value);
    assert_eq!(serde_json::from_value::<XlsxModel>(value).unwrap(), s.model);
}

// ── against jade ────────────────────────────────────────────────────

fn commit(model: &mut XlsxModel, jade: &mut JadeXlsx, seq: &mut u64, op: XlsxOp) {
    *seq += 1;
    let ctx = ctx(1, *seq);
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

/// Rows and columns of `range` (`B6:D8`) on the Operations sheet.
fn lines(model: &XlsxModel, range: &str) -> (Id, Vec<Id>, Vec<Id>) {
    let (first, last) = range.split_once(':').unwrap();
    let (sheet, top, left) = model.locate("Operations", first).unwrap();
    let (_, bottom, right) = model.locate("Operations", last).unwrap();
    let view = model.sheets().into_iter().find(|s| s.id == sheet).unwrap();
    let span = |all: &[Id], a: Id, b: Id| {
        let a = all.iter().position(|l| *l == a).unwrap();
        let b = all.iter().position(|l| *l == b).unwrap();
        all[a..=b].to_vec()
    };
    (
        sheet,
        span(&view.rows, top, bottom),
        span(&view.columns, left, right),
    )
}

fn jade_edges(jade: &JadeXlsx, reference: &str) -> FormatChange {
    edges(jade.workbook().format_at("Operations", reference).unwrap())
}

#[test]
fn an_outline_reaches_jade_and_survives_export() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let (sheet, rows, columns) = lines(&model, "B6:D8");
    let box_line = line(BorderStyle::Medium, Some("C00000"));
    let mut seq = 0;
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::SetBorders {
            sheet,
            rows,
            columns,
            stamp: stamp(1),
            borders: RangeBorders {
                outline: Some(box_line.clone()),
                inside: Some(thin()),
                ..RangeBorders::default()
            },
        },
    );
    let reopened = JadeXlsx::open(&jade.export().unwrap()).unwrap();
    assert_agree(&model, &reopened, "after export");
    let corner = jade_edges(&reopened, "B6");
    assert_eq!(corner.border_top, Some(box_line.clone()));
    assert_eq!(corner.border_left, Some(box_line.clone()));
    assert_eq!(corner.border_bottom, Some(thin()));
    assert_eq!(corner.border_right, Some(thin()));
    // Outside the range nothing changed.
    assert_eq!(
        jade_edges(&reopened, "A6").border_right,
        Some(BorderLine::none())
    );
}

/// Excel fills an inserted row with the format of the row above (its
/// "Format Same As Above" default); jade leaves it unformatted. The model
/// follows jade: a new row is not part of a range drawn before it existed,
/// so a box drawn around rows 6–8 is open on the left and right of the row
/// inserted inside it, and the model knows no format for its cells.
#[test]
fn a_row_inserted_inside_a_box_is_not_bordered_as_in_jade() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let (sheet, rows, columns) = lines(&model, "B6:D8");
    let mut seq = 0;
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::SetBorders {
            sheet,
            rows: rows.clone(),
            columns: columns.clone(),
            stamp: stamp(1),
            borders: RangeBorders {
                outline: Some(thin()),
                ..RangeBorders::default()
            },
        },
    );
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::Structure {
            sheet,
            op: GridOp::InsertRow {
                id: stamp(2),
                after: Some(rows[0]),
            },
        },
    );
    assert_eq!(
        model.format(sheet, stamp(2), columns[0]),
        FormatChange::default()
    );
    assert_eq!(
        jade_edges(&jade, "B7").border_left,
        Some(BorderLine::none())
    );
    // The box's edges moved with their cells.
    assert_eq!(jade_edges(&jade, "B6").border_left, Some(thin()));
    assert_eq!(jade_edges(&jade, "B8").border_left, Some(thin()));
    assert_eq!(jade_edges(&jade, "D9").border_bottom, Some(thin()));

    // Deleting the box's top row takes its top edge with it, as in Excel:
    // the next row does not gain one.
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::Structure {
            sheet,
            op: GridOp::DeleteRow { id: rows[0] },
        },
    );
    assert_eq!(jade_edges(&jade, "B6").border_top, Some(BorderLine::none()));
}

/// As in jade (and Excel), merging gives the covered cells the anchor's
/// format — its borders included — and unmerging leaves them there.
#[test]
fn merged_cells_take_the_anchor_s_borders_like_in_jade() {
    let (mut model, mut jade) = open("grid-operations-workbook.xlsx");
    let (sheet, rows, columns) = lines(&model, "B6:C7");
    let mut seq = 0;
    let boxed = RangeBorders {
        outline: Some(line(BorderStyle::Double, Some("0070C0"))),
        ..RangeBorders::default()
    };
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::SetBorders {
            sheet,
            rows: vec![rows[0]],
            columns: vec![columns[0]],
            stamp: stamp(1),
            borders: boxed,
        },
    );
    let anchor = edges(model.format(sheet, rows[0], columns[0]));
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::MergeCells {
            sheet,
            merge: Merge {
                top: rows[0],
                left: columns[0],
                bottom: rows[1],
                right: columns[1],
            },
            stamp: stamp(2),
        },
    );
    assert_eq!(edges(model.format(sheet, rows[1], columns[1])), anchor);
    commit(
        &mut model,
        &mut jade,
        &mut seq,
        XlsxOp::UnmergeCells {
            sheet,
            top: rows[0],
            left: columns[0],
        },
    );
    assert_eq!(edges(model.format(sheet, rows[1], columns[1])), anchor);
    assert_eq!(common::masked(&jade_edges(&jade, "C7"), &anchor), anchor);
}

// ── convergence ─────────────────────────────────────────────────────

fn arb_line() -> impl Strategy<Value = BorderLine> {
    (
        prop::sample::select(&BorderStyle::ALL[..]),
        prop::option::of(prop::sample::select(&["FF0000", "#00b050", "0070C0"][..])),
    )
        .prop_map(|(style, color)| BorderLine {
            style,
            color: color.map(str::to_string),
        })
}

fn arb_borders() -> impl Strategy<Value = RangeBorders> {
    use prop::option::of;
    (
        (
            of(arb_line()),
            of(arb_line()),
            of(arb_line()),
            of(arb_line()),
        ),
        (
            of(arb_line()),
            of(arb_line()),
            of(arb_line()),
            of(arb_line()),
        ),
        (of(arb_line()), of(any::<bool>()), of(any::<bool>())),
    )
        .prop_map(
            |(
                (outline, inside, top, bottom),
                (left, right, inside_horizontal, inside_vertical),
                (diagonal, diagonal_up, diagonal_down),
            )| RangeBorders {
                outline,
                inside,
                top,
                bottom,
                left,
                right,
                inside_horizontal,
                inside_vertical,
                diagonal,
                diagonal_up,
                diagonal_down,
            },
        )
}

/// What a `SetBorders` on `rows` × `columns` writes at row `r`, column `c`
/// (positions inside the range) — an independent statement of Excel's
/// border dialog.
fn expected_at(b: &RangeBorders, r: usize, c: usize, rows: usize, columns: usize) -> FormatChange {
    let pick = |own: &Option<BorderLine>, general: &Option<BorderLine>| {
        own.clone().or_else(|| general.clone())
    };
    let horizontal = pick(&b.inside_horizontal, &b.inside);
    let vertical = pick(&b.inside_vertical, &b.inside);
    let canonical = |line: Option<BorderLine>| {
        line.map(|l| BorderLine {
            color: l
                .color
                .filter(|_| l.style != BorderStyle::None)
                .map(|c| c.trim_start_matches('#').to_ascii_uppercase()),
            style: l.style,
        })
    };
    FormatChange {
        border_top: canonical(if r == 0 {
            pick(&b.top, &b.outline)
        } else {
            horizontal.clone()
        }),
        border_bottom: canonical(if r + 1 == rows {
            pick(&b.bottom, &b.outline)
        } else {
            horizontal
        }),
        border_left: canonical(if c == 0 {
            pick(&b.left, &b.outline)
        } else {
            vertical.clone()
        }),
        border_right: canonical(if c + 1 == columns {
            pick(&b.right, &b.outline)
        } else {
            vertical
        }),
        border_diagonal: canonical(b.diagonal.clone()),
        diagonal_up: b.diagonal_up,
        diagonal_down: b.diagonal_down,
        ..FormatChange::default()
    }
}

fn overlay(under: &FormatChange, over: &FormatChange) -> FormatChange {
    FormatChange {
        border_left: over.border_left.clone().or(under.border_left.clone()),
        border_right: over.border_right.clone().or(under.border_right.clone()),
        border_top: over.border_top.clone().or(under.border_top.clone()),
        border_bottom: over.border_bottom.clone().or(under.border_bottom.clone()),
        border_diagonal: over
            .border_diagonal
            .clone()
            .or(under.border_diagonal.clone()),
        diagonal_up: over.diagonal_up.or(under.diagonal_up),
        diagonal_down: over.diagonal_down.or(under.diagonal_down),
        ..FormatChange::default()
    }
}

const REGION: [&str; 4] = ["B6", "C6", "B7", "C7"];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Three replicas draw borders over overlapping ranges of a 2 × 2
    /// region, with messages delivered in a random order. Every replica,
    /// the server and jade end with, for each cell and edge, the line of the
    /// newest write that drew it — so concurrent changes to different edges
    /// both survive.
    #[test]
    fn concurrent_border_edits_converge_on_the_newest_line_per_edge(
        steps in prop::collection::vec(
            (0..3usize, 0..4u8, (0..2usize, 0..2usize, 0..2usize, 0..2usize), arb_borders()),
            1..20,
        ),
    ) {
        let (model, jade) = open("grid-operations-workbook.xlsx");
        let (sheet, rows, columns) = lines(&model, "B6:C7");
        let mut sim = Simulation::with_materializer(model, jade, 3);
        let mut writes: Vec<(Id, usize, usize, FormatChange)> = Vec::new();
        for (client, kind, (r0, r1, c0, c1), b) in steps {
            match kind {
                0 | 1 => {
                    let (top, bottom) = (r0.min(r1), r0.max(r1));
                    let (left, right) = (c0.min(c1), c0.max(c1));
                    let stamp = sim.client_mut(client).reserve(1);
                    let op = XlsxOp::SetBorders {
                        sheet,
                        rows: rows[top..=bottom].to_vec(),
                        columns: columns[left..=right].to_vec(),
                        stamp,
                        borders: b.clone(),
                    };
                    if sim.local(client, op).is_ok() {
                        for r in top..=bottom {
                            for c in left..=right {
                                let change = expected_at(
                                    &b, r - top, c - left, bottom - top + 1, right - left + 1,
                                );
                                writes.push((stamp, r, c, change));
                            }
                        }
                    }
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
        for (r, row) in rows.iter().enumerate() {
            for (c, column) in columns.iter().enumerate() {
                let expected = writes
                    .iter()
                    .filter(|(_, wr, wc, _)| (*wr, *wc) == (r, c))
                    .fold(FormatChange::default(), |under, (.., over)| overlay(&under, over));
                prop_assert_eq!(edges(server.document().format(sheet, *row, *column)), expected.clone());
                if expected != FormatChange::default() {
                    let theirs = jade_edges(server.materializer(), REGION[r * 2 + c]);
                    prop_assert_eq!(common::masked(&theirs, &expected), expected);
                }
            }
        }
        assert_agree(server.document(), server.materializer(), "after the session");
    }
}
