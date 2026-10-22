use std::collections::{BTreeMap, BTreeSet};

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_mcp::read_xlsx;
use jade_collab_sync::{ApplyContext, Document};
use jade_xlsx_collab::intent::{resolve, XlsxIntent};
use jade_xlsx_collab::XlsxModel;
use proptest::prelude::*;
use serde_json::{json, Value};

fn edit(model: &mut XlsxModel, input: Value) {
    let intent: XlsxIntent = serde_json::from_value(input).unwrap();
    let mut next = model.max_lamport() + 1;
    let op = resolve(model, &intent, &mut |n| {
        let id = Id::new(next, ReplicaId(1));
        next += n;
        id
    })
    .unwrap();
    model
        .apply(
            &op,
            &ApplyContext {
                origin: ReplicaId(1),
                base_seq: 0,
                seq: None,
            },
        )
        .unwrap();
}

/// Count each cell once even when it has both a value and a format.
fn entries(page: &Value) -> usize {
    page["sheets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let cells: BTreeSet<_> = s["cells"]
                .as_object()
                .unwrap()
                .keys()
                .chain(s["formats"].as_object().unwrap().keys())
                .collect();
            cells.len()
                + s["row_formats"].as_object().unwrap().len()
                + s["column_formats"].as_object().unwrap().len()
                + s["merges"].as_array().map_or(0, Vec::len)
        })
        .sum()
}

fn model() -> XlsxModel {
    let mut model = XlsxModel::blank(&[("First", 30, 5), ("Second", 30, 5)]);
    for sheet in ["First", "Second"] {
        for row in 1..=20 {
            edit(
                &mut model,
                json!({"type":"set_format", "sheet":sheet, "range":format!("B{row}"), "change":{"bold":true}}),
            );
            edit(
                &mut model,
                json!({"type":"set_format", "sheet":sheet, "range":format!("{row}:{row}"), "change":{"italic":true}}),
            );
        }
        edit(
            &mut model,
            json!({"type":"set_format", "sheet":sheet, "range":"C:E", "change":{"wrap_text":true}}),
        );
        for reference in ["A2", "A10", "B1"] {
            edit(
                &mut model,
                json!({"type":"set_cell", "sheet":sheet, "reference":reference, "value":{"kind":"text", "value":reference}}),
            );
        }
        edit(
            &mut model,
            json!({"type":"merge_cells", "sheet":sheet, "range":"D25:E27"}),
        );
    }
    model
}

#[test]
fn one_limit_bounds_values_cell_formats_axis_formats_and_merges_across_all_sheets() {
    let model = model();
    let page = read_xlsx(&model, 0, 3);
    assert!(
        entries(&page) <= 3,
        "one page returned {} entries",
        entries(&page)
    );
    assert!(page["next"].is_u64());
    assert!(page["entry_count"].as_u64().unwrap() > page["cell_count"].as_u64().unwrap());
}

#[test]
fn paging_reassembles_values_and_metadata_once_including_blank_formatted_cells() {
    let model = model();
    let all = read_xlsx(&model, 0, usize::MAX);
    let mut seen = BTreeMap::new();
    let mut at = 0;
    let mut count = 0;
    loop {
        let page = read_xlsx(&model, at, 7);
        let n = entries(&page);
        assert!(n > 0 && n <= 7);
        count += n;
        for sheet in page["sheets"].as_array().unwrap() {
            let name = sheet["name"].as_str().unwrap();
            for field in ["cells", "formats", "row_formats", "column_formats"] {
                for (key, value) in sheet[field].as_object().unwrap() {
                    assert!(seen
                        .insert(
                            (name.to_string(), field.to_string(), key.clone()),
                            value.clone()
                        )
                        .is_none());
                }
            }
            for merge in sheet["merges"].as_array().unwrap() {
                assert!(seen
                    .insert(
                        (name.into(), "merges".into(), merge.as_str().unwrap().into()),
                        Value::Null
                    )
                    .is_none());
            }
        }
        let Some(next) = page["next"].as_u64() else {
            break;
        };
        assert!(next as usize > at);
        at = next as usize;
    }
    assert_eq!(count, all["entry_count"].as_u64().unwrap() as usize);
    for sheet in all["sheets"].as_array().unwrap() {
        let name = sheet["name"].as_str().unwrap();
        for field in ["cells", "formats", "row_formats", "column_formats"] {
            for (key, value) in sheet[field].as_object().unwrap() {
                assert_eq!(
                    seen.remove(&(name.into(), field.into(), key.clone()))
                        .as_ref(),
                    Some(value)
                );
            }
        }
        for merge in sheet["merges"].as_array().unwrap() {
            assert!(seen
                .remove(&(name.into(), "merges".into(), merge.as_str().unwrap().into()))
                .is_some());
        }
    }
    assert!(seen.is_empty());
    let beyond = read_xlsx(&model, usize::MAX, usize::MAX);
    assert_eq!(entries(&beyond), 0);
    assert!(beyond["next"].is_null());
}

#[test]
fn a_format_only_workbook_has_a_cursor_and_numeric_row_major_cell_order() {
    let mut model = XlsxModel::blank(&[("S", 20, 3)]);
    for reference in ["A10", "B1", "A2"] {
        edit(
            &mut model,
            json!({"type":"set_format", "sheet":"S", "range":reference, "change":{"bold":true}}),
        );
    }
    let mut keys = Vec::new();
    for start in 0..3 {
        let page = read_xlsx(&model, start, 1);
        assert_eq!(page["cell_count"], json!(0));
        assert_eq!(entries(&page), 1);
        keys.extend(
            page["sheets"][0]["formats"]
                .as_object()
                .unwrap()
                .keys()
                .cloned(),
        );
    }
    assert_eq!(keys, ["B1", "A2", "A10"]);
}

#[test]
fn notes_on_blank_cells_are_readable_and_share_the_cell_page_item() {
    let mut model = XlsxModel::blank(&[("S", 10, 3)]);
    for reference in ["A3", "B7"] {
        edit(
            &mut model,
            json!({"type":"set_note", "sheet":"S", "reference":reference, "text":"Review 日本🙂"}),
        );
    }
    let page = read_xlsx(&model, 0, 1);
    assert_eq!(page["entry_count"], json!(2));
    assert_eq!(page["cell_count"], json!(0));
    assert_eq!(page["sheets"][0]["notes"], json!({"A3":"Review 日本🙂"}));
    assert_eq!(page["next"], json!(1));
    let page = read_xlsx(&model, 1, 1);
    assert_eq!(page["sheets"][0]["notes"], json!({"B7":"Review 日本🙂"}));
    assert_eq!(entries(&page), 1);
}

#[test]
fn a_million_cell_merge_is_one_geometry_item_and_one_anchor_item() {
    use jade_collab_core::grid::{Grid, Merge};
    let model = XlsxModel::blank(&[("S", 10_000, 100)]);
    let sheet = &model.sheets()[0];
    // A loaded merge does not allocate per-cell registers. Construct that
    // valid checkpoint shape directly rather than clearing a million cells.
    let grid = Grid::with_merges_unchecked(
        sheet.rows.clone(),
        sheet.columns.clone(),
        vec![Merge {
            top: sheet.rows[0],
            bottom: sheet.rows[9_999],
            left: sheet.columns[0],
            right: sheet.columns[99],
        }],
    );
    let mut json = serde_json::to_value(&model).unwrap();
    json["sheets"][0]["grid"] = serde_json::to_value(grid).unwrap();
    let model: XlsxModel = serde_json::from_value(json).unwrap();
    let page = read_xlsx(&model, 0, 2);
    assert_eq!(page["entry_count"], json!(2));
    assert_eq!(page["sheets"][0]["merges"], json!(["A1:CV10000"]));
    assert_eq!(entries(&page), 2);
    assert!(serde_json::to_vec(&page).unwrap().len() < 1000);
}

fn keys(page: &Value) -> BTreeSet<(u8, String)> {
    let sheet = &page["sheets"][0];
    let mut keys: BTreeSet<_> = sheet["cells"]
        .as_object()
        .unwrap()
        .keys()
        .chain(sheet["formats"].as_object().unwrap().keys())
        .map(|r| (0, r.clone()))
        .collect();
    for (kind, field) in [(1, "row_formats"), (2, "column_formats")] {
        keys.extend(
            sheet[field]
                .as_object()
                .unwrap()
                .keys()
                .map(|r| (kind, r.clone())),
        );
    }
    keys
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Independent row/column tokens and sets of declared styles track the
    /// sparse read items while values are cleared and lines move or disappear.
    #[test]
    fn page_histories_preserve_all_sparse_items_through_edits_structure_and_checkpoints(
        steps in prop::collection::vec((0u8..12, any::<u8>(), any::<bool>(), 1usize..8), 1..35),
    ) {
        let mut model = XlsxModel::blank(&[("S", 8, 4)]);
        let (mut rows, mut columns): (Vec<usize>, Vec<usize>) = ((0..8).collect(), (0..4).collect());
        let (mut next_row, mut next_column) = (8, 4);
        let mut values = BTreeMap::new();
        let mut notes = BTreeMap::new();
        let mut formats = BTreeSet::new();
        let mut row_formats = BTreeSet::new();
        let mut column_formats = BTreeSet::new();
        for (kind, choice, on, width) in steps {
            let r = usize::from(choice) % rows.len();
            let c = (usize::from(choice) / 7) % columns.len();
            let reference = format!("{}{}", jade_xlsx_collab::column_label(c as u32), r + 1);
            match kind {
                0 => {
                    edit(&mut model, json!({"type":"set_cell", "sheet":"S", "reference":reference,
                        "value":if on { json!({"kind":"text", "value":"日本🙂"}) } else { json!({"kind":"blank"}) }}));
                    if on { values.insert((rows[r], columns[c]), "日本🙂".to_string()); }
                    else { values.remove(&(rows[r], columns[c])); }
                }
                1 => {
                    edit(&mut model, json!({"type":"set_format", "sheet":"S", "range":reference, "change":{"bold":on}}));
                    formats.insert((rows[r], columns[c]));
                }
                2 => {
                    edit(&mut model, json!({"type":"set_format", "sheet":"S", "range":format!("{}:{}", r + 1, r + 1), "change":{"italic":on}}));
                    row_formats.insert(rows[r]);
                }
                3 => {
                    let column = jade_xlsx_collab::column_label(c as u32);
                    edit(&mut model, json!({"type":"set_format", "sheet":"S", "range":format!("{column}:{column}"), "change":{"wrap_text":on}}));
                    column_formats.insert(columns[c]);
                }
                4 => {
                    edit(&mut model, json!({"type":"insert_row", "sheet":"S", "before":r + 1}));
                    rows.insert(r, next_row); next_row += 1;
                }
                5 if rows.len() > 1 => {
                    edit(&mut model, json!({"type":"delete_row", "sheet":"S", "row":r + 1}));
                    rows.remove(r);
                }
                6 => {
                    edit(&mut model, json!({"type":"insert_column", "sheet":"S", "before":jade_xlsx_collab::column_label(c as u32)}));
                    columns.insert(c, next_column); next_column += 1;
                }
                7 if columns.len() > 1 => {
                    edit(&mut model, json!({"type":"delete_column", "sheet":"S", "column":jade_xlsx_collab::column_label(c as u32)}));
                    columns.remove(c);
                }
                8 => { model = serde_json::from_slice(&serde_json::to_vec(&model).unwrap()).unwrap(); }
                10 => {
                    edit(&mut model, json!({"type":"set_note", "sheet":"S", "reference":reference, "text":"Check 日本"}));
                    notes.insert((rows[r], columns[c]), "Check 日本".to_string());
                }
                11 => {
                    edit(&mut model, json!({"type":"delete_note", "sheet":"S", "reference":reference}));
                    notes.remove(&(rows[r], columns[c]));
                }
                _ => {}
            }
            let position = |(row, column): &(usize, usize)| -> Option<String> {
                Some(format!("{}{}", jade_xlsx_collab::column_label(columns.iter().position(|c| c == column)? as u32), rows.iter().position(|r| r == row)? + 1))
            };
            let mut expected: BTreeSet<_> = values.keys().chain(formats.iter()).chain(notes.keys()).filter_map(|key| position(key).map(|r| (0, r))).collect();
            expected.extend(row_formats.iter().filter_map(|row| rows.iter().position(|r| r == row).map(|r| (1, (r + 1).to_string()))));
            expected.extend(column_formats.iter().filter_map(|column| columns.iter().position(|c| c == column).map(|c| (2, jade_xlsx_collab::column_label(c as u32)))));
            let expected_values: BTreeMap<_, _> = values.iter().filter_map(|(key, value)| position(key).map(|r| (r, json!({"kind":"text", "value":value})))).collect();
            let expected_notes: BTreeMap<_, _> = notes.iter().filter_map(|(key, note)| position(key).map(|r| (r, json!(note)))).collect();
            let all = read_xlsx(&model, 0, usize::MAX);
            prop_assert_eq!(keys(&all), expected.clone());
            prop_assert_eq!(all["cell_count"].as_u64().unwrap() as usize, expected_values.len());
            prop_assert_eq!(all["entry_count"].as_u64().unwrap() as usize, expected.len());
            let mut seen = BTreeSet::new();
            let mut seen_values = BTreeMap::new();
            let mut seen_notes = BTreeMap::new();
            let mut at = 0;
            loop {
                let page = read_xlsx(&model, at, width);
                prop_assert!(entries(&page) <= width);
                for key in keys(&page) { prop_assert!(seen.insert(key), "duplicated page item"); }
                for (reference, value) in page["sheets"][0]["cells"].as_object().unwrap() { seen_values.insert(reference.clone(), value.clone()); }
                for (reference, note) in page["sheets"][0]["notes"].as_object().unwrap() { prop_assert!(seen_notes.insert(reference.clone(), note.clone()).is_none()); }
                let Some(next) = page["next"].as_u64() else { break };
                prop_assert!(next as usize > at);
                at = next as usize;
            }
            prop_assert_eq!(seen, expected);
            prop_assert_eq!(seen_values, expected_values);
            prop_assert_eq!(seen_notes, expected_notes);
        }
    }
}
