//! Position-addressed edits (A1 references, 1-based rows, column letters),
//! resolved against a view into identifier-addressed [`XlsxOp`]s — what a
//! person's UI or an agent naturally produces.

use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, IdError};
use serde::{Deserialize, Serialize};

use crate::model::{parse_reference, LineAxis, LoadOptions, RangeBorders, XlsxModel, XlsxOp};
use crate::{CellInput, FormatChange};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum XlsxIntent {
    SetCell {
        sheet: String,
        reference: String,
        value: CellInput,
    },
    /// A new row that becomes row `before` (1-based); the old one moves down.
    InsertRow {
        sheet: String,
        before: u32,
    },
    DeleteRow {
        sheet: String,
        row: u32,
    },
    /// A new column that becomes column `before` (`C`).
    InsertColumn {
        sheet: String,
        before: String,
    },
    DeleteColumn {
        sheet: String,
        column: String,
    },
    SetNote {
        sheet: String,
        reference: String,
        text: String,
    },
    DeleteNote {
        sheet: String,
        reference: String,
    },
    /// Formats `range` (`B2:D5`, or one cell `B2`): the attributes `change`
    /// names, keeping the rest. The range is fixed to the cells it covers
    /// now, so a row another replica inserts into it meanwhile is left out.
    /// Whole rows (`3:5`) or columns (`B:D`) are formatted as lines: every
    /// cell they hold, also ones inserted later.
    SetFormat {
        sheet: String,
        range: String,
        change: FormatChange,
    },
    /// Draws lines over `range` as Excel's border dialog does: `outline`
    /// around it, `inside` between its cells, or each edge on its own. Like
    /// a format, it covers the cells the range covers now.
    SetBorders {
        sheet: String,
        range: String,
        borders: RangeBorders,
    },
    /// Merges `range` (`B2:C3`): the top-left value stays, the rest clear.
    MergeCells {
        sheet: String,
        range: String,
    },
    /// Undoes the merge starting at `range`'s top-left cell.
    UnmergeCells {
        sheet: String,
        range: String,
    },
    /// A new empty sheet after `after` (or at the end).
    AddSheet {
        name: String,
        #[serde(default)]
        after: Option<String>,
    },
    RenameSheet {
        sheet: String,
        name: String,
    },
    DeleteSheet {
        sheet: String,
    },
    /// To tab position `to` (0 is the first tab).
    MoveSheet {
        sheet: String,
        to: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum XlsxIntentError {
    #[error(transparent)]
    Identifiers(#[from] IdError),
    #[error("no sheet named {0}")]
    NoSuchSheet(String),
    #[error("{0} is outside the editable part of the sheet")]
    NoSuchCell(String),
    #[error("{0} is an Excel table header: it holds the column's name, as text")]
    HeaderText(String),
}

/// The rows (`3:5`) or columns (`B:D`) a whole-line range names; `None`
/// when `range` is not one.
fn whole_lines(
    rows: &[Id],
    columns: &[Id],
    range: &str,
) -> Option<Result<(LineAxis, Vec<Id>), XlsxIntentError>> {
    let (first, last) = range.split_once(':')?;
    let (first, last) = (first.trim_start_matches('$'), last.trim_start_matches('$'));
    let outside = || XlsxIntentError::NoSuchCell(range.to_string());
    let span = |lines: &[Id], a: u32, b: u32| {
        lines
            .get(a.min(b) as usize..=a.max(b) as usize)
            .map(<[Id]>::to_vec)
            .ok_or_else(outside)
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let letters = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphabetic());
    if digits(first) && digits(last) {
        let number = |text: &str| text.parse::<u32>().ok().and_then(|n| n.checked_sub(1));
        return Some(match (number(first), number(last)) {
            (Some(a), Some(b)) => span(rows, a, b).map(|lines| (LineAxis::Rows, lines)),
            _ => Err(outside()),
        });
    }
    if letters(first) && letters(last) {
        return Some(match (column_index(first), column_index(last)) {
            (Some(a), Some(b)) => span(columns, a, b).map(|lines| (LineAxis::Columns, lines)),
            _ => Err(outside()),
        });
    }
    None
}

/// The rows and columns `range` (`B2:D5` or `B2`) covers, top-left first.
fn rectangle(
    rows: &[Id],
    columns: &[Id],
    range: &str,
) -> Result<(Vec<Id>, Vec<Id>), XlsxIntentError> {
    let outside = || XlsxIntentError::NoSuchCell(range.to_string());
    let (first, last) = range.split_once(':').unwrap_or((range, range));
    let (top, left) = parse_reference(first).ok_or_else(outside)?;
    let (bottom, right) = parse_reference(last).ok_or_else(outside)?;
    let span = |lines: &[Id], a: u32, b: u32| {
        lines
            .get(a.min(b) as usize..=a.max(b) as usize)
            .map(<[Id]>::to_vec)
            .ok_or_else(outside)
    };
    Ok((span(rows, top, bottom)?, span(columns, left, right)?))
}

fn column_index(label: &str) -> Option<u32> {
    parse_reference(&format!("{label}1")).map(|(_, column)| column)
}

/// Resolves `intent` against `view`; `ids(n)` reserves `n` fresh ids.
pub fn resolve(
    view: &XlsxModel,
    intent: &XlsxIntent,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Result<XlsxOp, XlsxIntentError> {
    try_resolve(view, intent, &mut |n| Ok(ids(n)))
}

/// Resolves with an allocator that can report identifier exhaustion.
pub fn try_resolve(
    view: &XlsxModel,
    intent: &XlsxIntent,
    ids: &mut dyn FnMut(u64) -> Result<Id, IdError>,
) -> Result<XlsxOp, XlsxIntentError> {
    let sheet_named = |name: &str| {
        view.sheets()
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| XlsxIntentError::NoSuchSheet(name.to_string()))
    };
    let line = |lines: &[Id], index: Option<u32>, what: String| {
        index
            .and_then(|i| lines.get(i as usize).copied())
            .ok_or(XlsxIntentError::NoSuchCell(what))
    };
    let cell = |sheet: &str, reference: &str| {
        sheet_named(sheet)?;
        view.locate(sheet, reference)
            .ok_or_else(|| XlsxIntentError::NoSuchCell(reference.to_string()))
    };
    Ok(match intent {
        XlsxIntent::SetNote {
            sheet,
            reference,
            text,
        } => {
            let (sheet, row, column) = cell(sheet, reference)?;
            XlsxOp::SetNote {
                sheet,
                row,
                column,
                stamp: ids(1)?,
                note: Some(text.clone()),
            }
        }
        XlsxIntent::DeleteNote { sheet, reference } => {
            let (sheet, row, column) = cell(sheet, reference)?;
            XlsxOp::SetNote {
                sheet,
                row,
                column,
                stamp: ids(1)?,
                note: None,
            }
        }
        XlsxIntent::SetFormat {
            sheet,
            range,
            change,
        } => {
            let s = sheet_named(sheet)?;
            if let Some(lines) = whole_lines(&s.rows, &s.columns, range) {
                let (axis, lines) = lines?;
                return Ok(XlsxOp::SetLineFormat {
                    sheet: s.id,
                    axis,
                    lines,
                    stamp: ids(1)?,
                    change: change.clone(),
                });
            }
            let (rows, columns) = rectangle(&s.rows, &s.columns, range)?;
            XlsxOp::SetFormat {
                sheet: s.id,
                rows,
                columns,
                stamp: ids(1)?,
                change: change.clone(),
            }
        }
        XlsxIntent::SetBorders {
            sheet,
            range,
            borders,
        } => {
            let s = sheet_named(sheet)?;
            let (rows, columns) = rectangle(&s.rows, &s.columns, range)?;
            XlsxOp::SetBorders {
                sheet: s.id,
                rows,
                columns,
                stamp: ids(1)?,
                borders: borders.clone(),
            }
        }
        XlsxIntent::MergeCells { sheet, range } => {
            let s = sheet_named(sheet)?;
            let (rows, columns) = rectangle(&s.rows, &s.columns, range)?;
            XlsxOp::MergeCells {
                sheet: s.id,
                merge: Merge {
                    top: rows[0],
                    left: columns[0],
                    bottom: *rows.last().expect("non-empty"),
                    right: *columns.last().expect("non-empty"),
                },
                stamp: ids(1)?,
            }
        }
        XlsxIntent::UnmergeCells { sheet, range } => {
            let s = sheet_named(sheet)?;
            let (rows, columns) = rectangle(&s.rows, &s.columns, range)?;
            XlsxOp::UnmergeCells {
                sheet: s.id,
                top: rows[0],
                left: columns[0],
            }
        }
        XlsxIntent::AddSheet { name, after } => {
            let after = after.as_deref().map(sheet_named).transpose()?.map(|s| s.id);
            let defaults = LoadOptions::default();
            let (rows, columns) = (defaults.min_rows, defaults.min_columns);
            XlsxOp::AddSheet {
                id: ids(1 + u64::from(rows) + u64::from(columns))?,
                name: name.clone(),
                after,
                rows,
                columns,
            }
        }
        XlsxIntent::RenameSheet { sheet, name } => XlsxOp::RenameSheet {
            sheet: sheet_named(sheet)?.id,
            name: name.clone(),
        },
        XlsxIntent::DeleteSheet { sheet } => XlsxOp::DeleteSheet {
            sheet: sheet_named(sheet)?.id,
        },
        XlsxIntent::MoveSheet { sheet, to } => XlsxOp::MoveSheet {
            sheet: sheet_named(sheet)?.id,
            to: *to,
        },
        XlsxIntent::SetCell {
            sheet,
            reference,
            value,
        } => {
            sheet_named(sheet)?;
            let (sheet, row, column) = view
                .locate(sheet, reference)
                .ok_or_else(|| XlsxIntentError::NoSuchCell(reference.clone()))?;
            // An Excel table's header names its column: a rename.
            if view.is_table_header(sheet, row, column) {
                let text = match value {
                    CellInput::Text(text) => text.clone(),
                    CellInput::Blank => String::new(),
                    _ => return Err(XlsxIntentError::HeaderText(reference.clone())),
                };
                XlsxOp::SetHeader {
                    sheet,
                    row,
                    column,
                    stamp: ids(1)?,
                    text,
                }
            } else {
                XlsxOp::SetCell {
                    sheet,
                    row,
                    column,
                    stamp: ids(1)?,
                    value: value.clone(),
                }
            }
        }
        XlsxIntent::InsertRow { sheet, before } => {
            let s = sheet_named(sheet)?;
            let after = match before.checked_sub(1) {
                None => return Err(XlsxIntentError::NoSuchCell(format!("row {before}"))),
                Some(0) => None,
                Some(above) => Some(line(&s.rows, Some(above - 1), format!("row {before}"))?),
            };
            XlsxOp::Structure {
                sheet: s.id,
                op: GridOp::InsertRow { id: ids(1)?, after },
            }
        }
        XlsxIntent::DeleteRow { sheet, row } => {
            let s = sheet_named(sheet)?;
            XlsxOp::Structure {
                sheet: s.id,
                op: GridOp::DeleteRow {
                    id: line(&s.rows, row.checked_sub(1), format!("row {row}"))?,
                },
            }
        }
        XlsxIntent::InsertColumn { sheet, before } => {
            let s = sheet_named(sheet)?;
            let index =
                column_index(before).ok_or_else(|| XlsxIntentError::NoSuchCell(before.clone()))?;
            let after = match index {
                0 => None,
                _ => Some(line(&s.columns, Some(index - 1), before.clone())?),
            };
            XlsxOp::Structure {
                sheet: s.id,
                op: GridOp::InsertColumn { id: ids(1)?, after },
            }
        }
        XlsxIntent::DeleteColumn { sheet, column } => {
            let s = sheet_named(sheet)?;
            XlsxOp::Structure {
                sheet: s.id,
                op: GridOp::DeleteColumn {
                    id: line(&s.columns, column_index(column), column.clone())?,
                },
            }
        }
    })
}
