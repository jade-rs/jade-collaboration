//! Position-addressed edits (A1 references, 1-based rows, column letters),
//! resolved against a view into identifier-addressed [`XlsxOp`]s — what a
//! person's UI or an agent naturally produces.

use jade_collab_core::grid::GridOp;
use jade_collab_core::id::Id;
use serde::{Deserialize, Serialize};

use crate::model::{parse_reference, XlsxModel, XlsxOp};
use crate::CellInput;

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
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum XlsxIntentError {
    #[error("no sheet named {0}")]
    NoSuchSheet(String),
    #[error("{0} is outside the editable part of the sheet")]
    NoSuchCell(String),
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
    Ok(match intent {
        XlsxIntent::SetCell {
            sheet,
            reference,
            value,
        } => {
            sheet_named(sheet)?;
            let (sheet, row, column) = view
                .locate(sheet, reference)
                .ok_or_else(|| XlsxIntentError::NoSuchCell(reference.clone()))?;
            XlsxOp::SetCell {
                sheet,
                row,
                column,
                stamp: ids(1),
                value: value.clone(),
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
                op: GridOp::InsertRow { id: ids(1), after },
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
                op: GridOp::InsertColumn { id: ids(1), after },
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
