#![allow(dead_code)]

use std::path::PathBuf;

use jade_collab_core::grid::GridOp;
use jade_collab_core::id::Id;
use jade_xlsx_collab::{column_label, CellInput, JadeXlsx, LoadOptions, XlsxModel, XlsxOp};
use jade_xlsx_headless_api::CellKind;

pub const SMALL: LoadOptions = LoadOptions {
    min_rows: 8,
    min_columns: 5,
};

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/public/fixtures")
}

pub fn open(name: &str) -> (XlsxModel, JadeXlsx) {
    let bytes = std::fs::read(fixtures().join(name)).unwrap();
    let jade = JadeXlsx::open(&bytes).expect("jade opens the fixture");
    let model = XlsxModel::from_workbook(jade.workbook(), SMALL).expect("model builds");
    (model, jade)
}

/// Every cell the model covers reads the same in jade.
pub fn assert_agree(model: &XlsxModel, jade: &JadeXlsx, context: &str) {
    for sheet in model.sheets() {
        for (r, row) in sheet.rows.iter().enumerate() {
            for (c, column) in sheet.columns.iter().enumerate() {
                let reference = format!("{}{}", column_label(c as u32), r + 1);
                let view = jade
                    .workbook()
                    .cell(&sheet.name, &reference)
                    .unwrap_or_else(|e| panic!("{context}: {}!{reference}: {e}", sheet.name));
                let ours = model.value(sheet.id, *row, *column);
                let agrees = match ours {
                    None => matches!(view.kind, CellKind::Empty | CellKind::Error),
                    Some(CellInput::Blank) => {
                        view.kind == CellKind::Empty && view.formula.is_none()
                    }
                    Some(CellInput::Formula(f)) => view.formula.as_deref() == Some(f.as_str()),
                    Some(CellInput::Number(n)) => view.formula.is_none() && view.number == Some(*n),
                    Some(CellInput::Text(t)) => {
                        view.formula.is_none() && view.text.as_deref() == Some(t.as_str())
                    }
                    Some(CellInput::Bool(b)) => {
                        view.formula.is_none()
                            && view.kind == CellKind::Bool
                            && view.number == Some(f64::from(u8::from(*b)))
                    }
                };
                assert!(
                    agrees,
                    "{context}: {}!{reference}: model {ours:?}, jade {:?} / formula {:?} / number {:?} / text {:?}",
                    sheet.name, view.kind, view.formula, view.number, view.text
                );
            }
        }
    }
}

/// A random operation on `view`; `ids(n)` reserves `n` consecutive ids.
pub fn generate(view: &XlsxModel, choice: u64, ids: &mut dyn FnMut(u64) -> Id) -> Option<XlsxOp> {
    let sheets = view.sheets();
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let sheet = &sheets[pick(sheets.len(), 4)];
    let row = sheet.rows[pick(sheet.rows.len(), 12)];
    let column = sheet.columns[pick(sheet.columns.len(), 20)];
    let a1 = |salt: u32| {
        format!(
            "{}{}",
            column_label(pick(sheet.columns.len(), salt) as u32),
            1 + pick(sheet.rows.len(), salt + 6)
        )
    };
    match choice % 12 {
        0..=6 => {
            let value = match (choice >> 28) % 6 {
                0 => CellInput::Number(((choice >> 34) % 1000) as f64 / 4.0),
                1 => CellInput::Text(["alpha", "日本語", "", " x "][pick(4, 40)].to_string()),
                2 => CellInput::Bool(choice >> 44 & 1 == 1),
                3 => CellInput::Blank,
                4 => CellInput::Formula(format!("{}*2", a1(46))),
                _ => CellInput::Formula(format!("SUM({}:{})", a1(46), a1(52))),
            };
            Some(XlsxOp::SetCell {
                sheet: sheet.id,
                row,
                column,
                stamp: ids(1),
                value,
            })
        }
        7 | 8 => Some(XlsxOp::Structure {
            sheet: sheet.id,
            op: if choice % 12 == 7 {
                GridOp::InsertRow {
                    id: ids(1),
                    after: (pick(4, 30) != 0).then_some(row),
                }
            } else {
                GridOp::InsertColumn {
                    id: ids(1),
                    after: (pick(4, 30) != 0).then_some(column),
                }
            },
        }),
        9 => Some(XlsxOp::Structure {
            sheet: sheet.id,
            op: GridOp::DeleteRow { id: row },
        }),
        10 => Some(XlsxOp::Structure {
            sheet: sheet.id,
            op: GridOp::DeleteColumn { id: column },
        }),
        _ => None,
    }
}
