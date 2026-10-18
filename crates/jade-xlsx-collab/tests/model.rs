use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane};
use jade_xlsx_collab::{
    column_label, CellInput, FormatChange, HorizontalAlignment, XlsxEffect, XlsxModel, XlsxOp,
};

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

fn note(s: &Sheet, r: usize, c: usize, stamp: Id, note: Option<&str>) -> XlsxOp {
    XlsxOp::SetNote {
        sheet: s.sheet,
        row: s.rows[r],
        column: s.columns[c],
        stamp,
        note: note.map(str::to_string),
    }
}

fn format(s: &Sheet, rows: &[usize], columns: &[usize], stamp: Id, change: FormatChange) -> XlsxOp {
    XlsxOp::SetFormat {
        sheet: s.sheet,
        rows: rows.iter().map(|r| s.rows[*r]).collect(),
        columns: columns.iter().map(|c| s.columns[*c]).collect(),
        stamp,
        change,
    }
}

fn bold(value: bool) -> FormatChange {
    FormatChange {
        bold: Some(value),
        ..FormatChange::default()
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
    assert_eq!(
        XlsxModel::lane(&note(&s, 0, 0, id(1), Some("n"))),
        Lane::Convergent
    );
    assert_eq!(
        XlsxModel::lane(&note(&s, 0, 0, id(1), None)),
        Lane::Convergent
    );
    assert_eq!(
        XlsxModel::lane(&format(&s, &[0, 1], &[0], id(1), bold(true))),
        Lane::Convergent
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

#[test]
fn notes_are_last_writer_wins_registers() {
    let mut s = blank();
    let effect = s
        .model
        .apply(&note(&s, 1, 2, id(5), Some("new")), &ctx(1, 0, Some(1)))
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetNote {
            sheet: "Sheet1".into(),
            reference: "C2".into(),
            note: Some("new".into()),
        }]
    );
    let effect = s
        .model
        .apply(&note(&s, 1, 2, id(2), Some("old")), &ctx(2, 0, Some(2)))
        .unwrap();
    assert_eq!(effect, vec![]);
    assert_eq!(s.model.note(s.sheet, s.rows[1], s.columns[2]), Some("new"));
    let effect = s
        .model
        .apply(&note(&s, 1, 2, id(6), None), &ctx(2, 0, Some(3)))
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetNote {
            sheet: "Sheet1".into(),
            reference: "C2".into(),
            note: None,
        }]
    );
    assert_eq!(s.model.note(s.sheet, s.rows[1], s.columns[2]), None);
    assert_eq!(s.model.max_lamport(), id(6).lamport);
}

#[test]
fn an_empty_note_is_no_note() {
    let mut s = blank();
    s.model
        .apply(&note(&s, 0, 0, id(1), Some("x")), &ctx(1, 0, Some(1)))
        .unwrap();
    let effect = s
        .model
        .apply(&note(&s, 0, 0, id(2), Some("")), &ctx(1, 0, Some(2)))
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetNote {
            sheet: "Sheet1".into(),
            reference: "A1".into(),
            note: None,
        }]
    );
    assert_eq!(s.model.note(s.sheet, s.rows[0], s.columns[0]), None);
}

#[test]
fn formatting_different_attributes_of_a_cell_composes() {
    let mut s = blank();
    s.model
        .apply(
            &format(&s, &[0], &[0], id(2), bold(true)),
            &ctx(1, 0, Some(1)),
        )
        .unwrap();
    // Older, but about another attribute: it still lands.
    let italic = FormatChange {
        italic: Some(true),
        ..FormatChange::default()
    };
    let effect = s
        .model
        .apply(
            &format(&s, &[0], &[0], id(1), italic.clone()),
            &ctx(2, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetFormat {
            sheet: "Sheet1".into(),
            range: "A1".into(),
            change: italic,
        }]
    );
    assert_eq!(
        s.model.format(s.sheet, s.rows[0], s.columns[0]),
        FormatChange {
            bold: Some(true),
            italic: Some(true),
            ..FormatChange::default()
        }
    );
}

#[test]
fn only_the_attributes_a_write_wins_reach_jade() {
    let mut s = blank();
    let newer = FormatChange {
        bold: Some(true),
        fill_color: Some("FF0000".into()),
        ..FormatChange::default()
    };
    let older = FormatChange {
        bold: Some(false),
        italic: Some(true),
        ..FormatChange::default()
    };
    s.model
        .apply(&format(&s, &[1], &[1], id(5), newer), &ctx(1, 0, Some(1)))
        .unwrap();
    let effect = s
        .model
        .apply(&format(&s, &[1], &[1], id(3), older), &ctx(2, 0, Some(2)))
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetFormat {
            sheet: "Sheet1".into(),
            range: "B2".into(),
            change: FormatChange {
                italic: Some(true),
                ..FormatChange::default()
            },
        }]
    );
    assert_eq!(
        s.model.format(s.sheet, s.rows[1], s.columns[1]),
        FormatChange {
            bold: Some(true),
            italic: Some(true),
            fill_color: Some("FF0000".into()),
            ..FormatChange::default()
        }
    );
}

#[test]
fn the_newest_write_of_an_attribute_wins_in_either_order() {
    let fill = |color: &str| FormatChange {
        fill_color: Some(color.into()),
        ..FormatChange::default()
    };
    let mut results = Vec::new();
    for swap in [false, true] {
        let mut s = blank();
        let mut ops = [
            format(&s, &[0], &[0], id(7), fill("00FF00")),
            format(&s, &[0], &[0], id(4), fill("0000FF")),
        ];
        if swap {
            ops.reverse();
        }
        for (i, op) in ops.iter().enumerate() {
            s.model.apply(op, &ctx(1, 0, Some(i as u64 + 1))).unwrap();
        }
        results.push(s.model.format(s.sheet, s.rows[0], s.columns[0]));
    }
    assert_eq!(results[0], fill("00FF00"));
    assert_eq!(results[1], fill("00FF00"));
}

#[test]
fn a_range_is_formatted_with_one_effect_per_rectangle() {
    let mut s = blank();
    let effect = s
        .model
        .apply(
            &format(&s, &[0, 1], &[0, 1], id(2), bold(true)),
            &ctx(1, 0, Some(1)),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetFormat {
            sheet: "Sheet1".into(),
            range: "A1:B2".into(),
            change: bold(true),
        }]
    );

    // B3 already has a newer bold, so the range loses there.
    s.model
        .apply(
            &format(&s, &[2], &[1], id(9), bold(true)),
            &ctx(1, 0, Some(2)),
        )
        .unwrap();
    let effect = s
        .model
        .apply(
            &format(&s, &[2, 3], &[0, 1], id(8), bold(false)),
            &ctx(2, 0, Some(3)),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![
            XlsxEffect::SetFormat {
                sheet: "Sheet1".into(),
                range: "A3".into(),
                change: bold(false),
            },
            XlsxEffect::SetFormat {
                sheet: "Sheet1".into(),
                range: "A4:B4".into(),
                change: bold(false),
            },
        ]
    );
    assert_eq!(s.model.format(s.sheet, s.rows[2], s.columns[1]), bold(true));
}

#[test]
fn format_values_are_stored_as_jade_reads_them_back() {
    let mut s = blank();
    let effect = s
        .model
        .apply(
            &format(
                &s,
                &[0],
                &[0],
                id(1),
                FormatChange {
                    font_color: Some("#ff00aa".into()),
                    fill_color: Some("c0ffee".into()),
                    number_format: Some("".into()),
                    ..FormatChange::default()
                },
            ),
            &ctx(1, 0, Some(1)),
        )
        .unwrap();
    let stored = FormatChange {
        font_color: Some("FF00AA".into()),
        fill_color: Some("C0FFEE".into()),
        number_format: Some("General".into()),
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
    s.model
        .apply(
            &format(
                &s,
                &[0],
                &[0],
                id(2),
                FormatChange {
                    number_format: Some("GENERAL".into()),
                    horizontal: Some(HorizontalAlignment::Center),
                    ..FormatChange::default()
                },
            ),
            &ctx(1, 0, Some(2)),
        )
        .unwrap();
    assert_eq!(
        s.model
            .format(s.sheet, s.rows[0], s.columns[0])
            .number_format,
        Some("General".into())
    );
}

#[test]
fn formats_jade_cannot_write_are_refused() {
    let invalid = [
        FormatChange::default(),
        FormatChange {
            font_color: Some("red".into()),
            ..FormatChange::default()
        },
        FormatChange {
            fill_color: Some("12345".into()),
            ..FormatChange::default()
        },
        FormatChange {
            font_size: Some(0.5),
            ..FormatChange::default()
        },
        FormatChange {
            font_size: Some(410.0),
            ..FormatChange::default()
        },
        FormatChange {
            font_size: Some(f32::NAN),
            ..FormatChange::default()
        },
        FormatChange {
            font_name: Some(String::new()),
            ..FormatChange::default()
        },
        // A valid attribute does not save an invalid one.
        FormatChange {
            bold: Some(true),
            font_color: Some("GG0000".into()),
            ..FormatChange::default()
        },
    ];
    for change in invalid {
        let mut s = blank();
        let before = s.model.clone();
        let op = format(&s, &[0], &[0], id(1), change.clone());
        assert!(
            s.model.apply(&op, &ctx(1, 0, Some(1))).is_err(),
            "{change:?}"
        );
        assert_eq!(s.model, before, "{change:?}");
    }
    let mut s = blank();
    let empty = format(&s, &[], &[0], id(1), bold(true));
    assert!(s.model.apply(&empty, &ctx(1, 0, Some(1))).is_err());
}

#[test]
fn notes_and_formats_in_a_deleted_row_are_kept_but_not_shown() {
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
        .apply(&note(&s, 2, 0, id(4), Some("n")), &ctx(1, 0, Some(2)))
        .unwrap();
    assert_eq!(effect, vec![]);
    assert_eq!(s.model.note(s.sheet, s.rows[2], s.columns[0]), Some("n"));
    // Rows 2 and 4 survive; row 3 is gone, so the range splits around it.
    let effect = s
        .model
        .apply(
            &format(&s, &[1, 2, 3], &[0], id(5), bold(true)),
            &ctx(1, 0, Some(3)),
        )
        .unwrap();
    assert_eq!(
        effect,
        vec![XlsxEffect::SetFormat {
            sheet: "Sheet1".into(),
            range: "A2:A3".into(),
            change: bold(true),
        }]
    );
    assert_eq!(s.model.format(s.sheet, s.rows[2], s.columns[0]), bold(true));
}

#[test]
fn notes_and_formats_on_unknown_cells_are_refused() {
    let mut s = blank();
    let stray = XlsxOp::SetNote {
        sheet: s.sheet,
        row: id(77),
        column: s.columns[0],
        stamp: id(78),
        note: Some("n".into()),
    };
    assert!(s.model.apply(&stray, &ctx(1, 0, None)).is_err());
    let stray = XlsxOp::SetFormat {
        sheet: s.sheet,
        rows: vec![s.rows[0]],
        columns: vec![s.columns[0], id(79)],
        stamp: id(80),
        change: bold(true),
    };
    let before = s.model.clone();
    assert!(s.model.apply(&stray, &ctx(1, 0, None)).is_err());
    assert_eq!(s.model, before);
}

#[test]
fn notes_and_formats_round_trip_through_json() {
    let mut s = blank();
    s.model
        .apply(&note(&s, 1, 1, id(1), Some("n")), &ctx(1, 0, Some(1)))
        .unwrap();
    s.model
        .apply(
            &format(
                &s,
                &[0, 1],
                &[2],
                id(2),
                FormatChange {
                    font_size: Some(14.5),
                    horizontal: Some(HorizontalAlignment::Right),
                    ..FormatChange::default()
                },
            ),
            &ctx(1, 0, Some(2)),
        )
        .unwrap();
    let json = serde_json::to_string(&s.model).unwrap();
    assert_eq!(serde_json::from_str::<XlsxModel>(&json).unwrap(), s.model);
    let op = format(&s, &[0], &[0], id(3), bold(true));
    let json = serde_json::to_string(&op).unwrap();
    assert_eq!(serde_json::from_str::<XlsxOp>(&json).unwrap(), op);
}
