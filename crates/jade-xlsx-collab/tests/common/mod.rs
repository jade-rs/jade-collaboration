#![allow(dead_code)]

use std::path::PathBuf;

use jade_collab_core::grid::GridOp;
use jade_collab_core::id::Id;
use jade_xlsx_collab::{
    column_label, CellInput, FormatChange, HorizontalAlignment, JadeXlsx, LoadOptions,
    VerticalAlignment, XlsxModel, XlsxOp,
};
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

/// The attributes of `theirs` that `ours` names, so a format jade reports
/// can be compared with the attributes the model records.
pub fn masked(theirs: &FormatChange, ours: &FormatChange) -> FormatChange {
    let FormatChange {
        bold,
        italic,
        underline,
        strike,
        font_color,
        font_size,
        font_name,
        fill_color,
        number_format,
        horizontal,
        vertical,
        wrap_text,
    } = ours;
    fn keep<T: Clone>(named: bool, value: &Option<T>) -> Option<T> {
        value.clone().filter(|_| named)
    }
    FormatChange {
        bold: keep(bold.is_some(), &theirs.bold),
        italic: keep(italic.is_some(), &theirs.italic),
        underline: keep(underline.is_some(), &theirs.underline),
        strike: keep(strike.is_some(), &theirs.strike),
        font_color: keep(font_color.is_some(), &theirs.font_color),
        font_size: keep(font_size.is_some(), &theirs.font_size),
        font_name: keep(font_name.is_some(), &theirs.font_name),
        fill_color: keep(fill_color.is_some(), &theirs.fill_color),
        number_format: keep(number_format.is_some(), &theirs.number_format),
        horizontal: keep(horizontal.is_some(), &theirs.horizontal),
        vertical: keep(vertical.is_some(), &theirs.vertical),
        wrap_text: keep(wrap_text.is_some(), &theirs.wrap_text),
    }
}

/// `over`'s attributes on top of `under`'s.
pub fn overlay(under: &FormatChange, over: &FormatChange) -> FormatChange {
    let FormatChange {
        bold,
        italic,
        underline,
        strike,
        font_color,
        font_size,
        font_name,
        fill_color,
        number_format,
        horizontal,
        vertical,
        wrap_text,
    } = over.clone();
    FormatChange {
        bold: bold.or(under.bold),
        italic: italic.or(under.italic),
        underline: underline.or(under.underline),
        strike: strike.or(under.strike),
        font_color: font_color.or(under.font_color.clone()),
        font_size: font_size.or(under.font_size),
        font_name: font_name.or(under.font_name.clone()),
        fill_color: fill_color.or(under.fill_color.clone()),
        number_format: number_format.or(under.number_format.clone()),
        horizontal: horizontal.or(under.horizontal),
        vertical: vertical.or(under.vertical),
        wrap_text: wrap_text.or(under.wrap_text),
    }
}

/// Every cell the model covers reads the same in jade: its value, its note,
/// and every format attribute the model records (jade's `format_at`, which
/// reports a merged cell's anchor, as Excel shows it).
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
                assert_eq!(
                    model.note(sheet.id, *row, *column),
                    view.comment.as_ref().map(|note| note.text.as_str()),
                    "{context}: {}!{reference}: note",
                    sheet.name
                );
                let format = model.format(sheet.id, *row, *column);
                if format != FormatChange::default() {
                    let theirs = jade
                        .workbook()
                        .format_at(&sheet.name, &reference)
                        .unwrap_or_else(|e| panic!("{context}: {}!{reference}: {e}", sheet.name));
                    assert_eq!(
                        masked(&theirs, &format),
                        format,
                        "{context}: {}!{reference}: format",
                        sheet.name
                    );
                }
            }
        }
    }
}

/// The number of format attributes [`set_attribute`] knows.
pub const ATTRIBUTES: u64 = 12;

/// Sets one attribute (`attribute % ATTRIBUTES`) of `change` to a value
/// picked by `value`, spelled the way jade reads it back.
pub fn set_attribute(change: &mut FormatChange, attribute: u64, value: u64) {
    let pick = |options: &[&str]| options[(value % options.len() as u64) as usize].to_string();
    let flag = value & 1 == 1;
    match attribute % ATTRIBUTES {
        0 => change.bold = Some(flag),
        1 => change.italic = Some(flag),
        2 => change.underline = Some(flag),
        3 => change.strike = Some(flag),
        4 => change.font_color = Some(pick(&["FF0000", "00AA00", "0000FF"])),
        5 => change.font_size = Some([8.0, 11.0, 14.5][(value % 3) as usize]),
        6 => change.font_name = Some(pick(&["Arial", "Meiryo", "Times New Roman"])),
        7 => change.fill_color = Some(pick(&["FFFF00", "", "C0FFEE"])),
        8 => change.number_format = Some(pick(&["0.00", "General", "yyyy/mm/dd", "#,##0", "0%"])),
        9 => {
            change.horizontal = Some(
                [
                    HorizontalAlignment::General,
                    HorizontalAlignment::Left,
                    HorizontalAlignment::Center,
                    HorizontalAlignment::Right,
                ][(value % 4) as usize],
            )
        }
        10 => {
            change.vertical = Some(
                [
                    VerticalAlignment::Top,
                    VerticalAlignment::Center,
                    VerticalAlignment::Bottom,
                ][(value % 3) as usize],
            )
        }
        _ => change.wrap_text = Some(flag),
    }
}

/// One or two attributes, some spelled loosely (`#c0ffee`, `general`, an
/// empty number format) — the model stores what jade reads back.
pub fn format_change(bits: u64) -> FormatChange {
    let mut change = FormatChange::default();
    for k in 0..=(bits & 1) {
        set_attribute(&mut change, bits >> (1 + 10 * k), bits >> (5 + 10 * k));
    }
    if bits >> 30 & 1 == 1 {
        for color in [&mut change.font_color, &mut change.fill_color]
            .into_iter()
            .flatten()
        {
            if !color.is_empty() {
                *color = format!("#{}", color.to_ascii_lowercase());
            }
        }
        if let Some(code) = &mut change.number_format {
            if code == "General" {
                *code = ["general", ""][(bits >> 31 & 1) as usize].to_string();
            }
        }
    }
    change
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
    match choice % 16 {
        0..=5 => {
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
        6 | 7 => Some(XlsxOp::Structure {
            sheet: sheet.id,
            op: if choice % 16 == 6 {
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
        8 => Some(XlsxOp::Structure {
            sheet: sheet.id,
            op: GridOp::DeleteRow { id: row },
        }),
        9 => Some(XlsxOp::Structure {
            sheet: sheet.id,
            op: GridOp::DeleteColumn { id: column },
        }),
        // Notes go to a 2 × 2 corner, so they are replaced and deleted
        // often, not just written once.
        10..=12 => Some(XlsxOp::SetNote {
            sheet: sheet.id,
            row: sheet.rows[pick(sheet.rows.len().min(2), 12)],
            column: sheet.columns[pick(sheet.columns.len().min(2), 20)],
            stamp: ids(1),
            note: [
                Some("note"),
                Some("日本語\nメモ"),
                Some(""),
                None,
                Some(" <a&b> "),
                Some("x_x000D_y\r"),
            ][pick(6, 28)]
            .map(str::to_string),
        }),
        13..=15 => {
            // A rectangle of up to 2 × 3 cells.
            let top = pick(sheet.rows.len(), 12);
            let left = pick(sheet.columns.len(), 20);
            let bottom = (top + pick(2, 26)).min(sheet.rows.len() - 1);
            let right = (left + pick(3, 27)).min(sheet.columns.len() - 1);
            Some(XlsxOp::SetFormat {
                sheet: sheet.id,
                rows: sheet.rows[top..=bottom].to_vec(),
                columns: sheet.columns[left..=right].to_vec(),
                stamp: ids(1),
                change: format_change(choice >> 30),
            })
        }
        _ => unreachable!(),
    }
}
