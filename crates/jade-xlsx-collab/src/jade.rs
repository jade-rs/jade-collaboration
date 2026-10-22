//! Realising model effects in a jade `HeadlessWorkbook`.

use jade_collab_sync::{ApplyContext, Materializer, Rejection};
use jade_xlsx_headless_editor::HeadlessWorkbook;

use crate::model::{column_label, from_view, XlsxEffect, XlsxModel, XlsxOp};
use crate::CellInput;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::Document;

/// The author jade records on notes written through the model; the model
/// does not track authors. (jade's own MCP server uses the same default.)
const NOTE_AUTHOR: &str = "jade";

/// The server's jade workbook. Every write patches the original package, so
/// parts the model does not describe survive untouched.
pub struct JadeXlsx {
    book: HeadlessWorkbook,
}

impl JadeXlsx {
    fn insert_column(&mut self, sheet: &str, at: u32) -> Result<(), String> {
        self.book
            .insert_columns(sheet, &column_label(at), 1)
            .map_err(|e| e.to_string())?;
        let view = self
            .book
            .workbook()
            .sheets
            .iter()
            .find(|s| s.name == sheet)
            .ok_or_else(|| "inserted column's sheet is missing".to_owned())?;
        if view
            .column_span(at)
            .and_then(|span| span.style_index)
            .is_none()
        {
            return Ok(());
        }
        let part = view.part_name.clone();
        let mut package =
            jade_opc::OpcPackage::open(self.book.bytes()).map_err(|e| e.to_string())?;
        let xml = package
            .required_part_str(&part)
            .map_err(|e| e.to_string())?;
        if let Some(xml) = crate::inserted_column::clear_style(&xml, at + 1)? {
            package.replace(&part, xml.into_bytes());
            self.book = HeadlessWorkbook::open(&package.write().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn new(book: HeadlessWorkbook) -> Self {
        Self { book }
    }

    pub fn open(bytes: &[u8]) -> Result<Self, String> {
        HeadlessWorkbook::open(bytes)
            .map(Self::new)
            .map_err(|e| e.to_string())
    }

    pub fn workbook(&self) -> &HeadlessWorkbook {
        &self.book
    }

    pub fn export(&self) -> Result<Vec<u8>, String> {
        self.book.export_bytes().map_err(|e| e.to_string())
    }
}

impl Materializer<XlsxModel> for JadeXlsx {
    fn may_derive(&self, _: &XlsxModel, cause: &XlsxOp) -> bool {
        matches!(cause, XlsxOp::Structure { .. } | XlsxOp::SetHeader { .. })
    }
    fn fork(&self, _: &XlsxModel) -> Result<Self, String> {
        Self::open(&self.export()?)
    }

    /// After a change that Excel follows up on its own inside a table — a
    /// structural change or a header rename — every cell of the sheet's
    /// tables is compared with the model; what jade holds differently (a new
    /// column's `ColumnN` header, a normalised header, a filled calculated
    /// column) comes back as derived operations.
    fn derived(&mut self, model: &XlsxModel, cause: &XlsxOp) -> Result<Vec<XlsxOp>, Rejection> {
        let sheet = match cause {
            XlsxOp::Structure { sheet, .. } | XlsxOp::SetHeader { sheet, .. } => *sheet,
            _ => return Ok(Vec::new()),
        };
        let Some(name) = model.sheet_name(sheet) else {
            return Ok(Vec::new());
        };
        let mut lamport = model.max_lamport();
        let mut derived = Vec::new();
        for (row, column, reference) in model.table_cells(sheet) {
            let Ok(view) = self.book.cell(name, &reference) else {
                continue;
            };
            let theirs = from_view(&view);
            let ours = model
                .value(sheet, row, column)
                .filter(|v| **v != CellInput::Blank);
            if theirs.as_ref() != ours {
                lamport = lamport
                    .checked_add(1)
                    .ok_or_else(|| Rejection::new("derived identifier space exhausted"))?;
                derived.push(XlsxOp::Derived {
                    sheet,
                    row,
                    column,
                    stamp: Id::new(lamport, ReplicaId::ORIGIN),
                    value: theirs.unwrap_or(CellInput::Blank),
                });
            }
        }
        Ok(derived)
    }

    fn materialize(
        &mut self,
        _: &XlsxModel,
        effects: Vec<XlsxEffect>,
        _: &ApplyContext,
    ) -> Result<(), String> {
        for effect in effects {
            let description = format!("{effect:?}");
            let result = match effect {
                XlsxEffect::SetCell {
                    sheet,
                    reference,
                    value,
                } => self.book.set(&sheet, &reference, value),
                XlsxEffect::InsertRow { sheet, at } => {
                    self.book.insert_rows(&sheet, at + 1, 1).map(drop)
                }
                XlsxEffect::DeleteRow { sheet, at } => {
                    self.book.delete_rows(&sheet, at + 1, 1).map(drop)
                }
                XlsxEffect::InsertColumn { sheet, at } => {
                    self.insert_column(&sheet, at)?;
                    continue;
                }
                XlsxEffect::DeleteColumn { sheet, at } => self
                    .book
                    .delete_columns(&sheet, &column_label(at), 1)
                    .map(drop),
                XlsxEffect::SetNote {
                    sheet,
                    reference,
                    note: Some(text),
                } => self
                    .book
                    .set_note(&sheet, &reference, &text, NOTE_AUTHOR)
                    .map(drop),
                XlsxEffect::SetNote {
                    sheet,
                    reference,
                    note: None,
                } => self.book.delete_note(&sheet, &reference).map(drop),
                XlsxEffect::SetFormat {
                    sheet,
                    range,
                    change,
                } => self.book.set_format(&sheet, &range, &change).map(drop),

                XlsxEffect::MergeCells { sheet, range } => {
                    self.book.merge_cells(&sheet, &range).map(drop)
                }
                XlsxEffect::UnmergeCells { sheet, range } => {
                    self.book.unmerge_cells(&sheet, &range).map(drop)
                }
                XlsxEffect::AddSheet { name, after } => {
                    self.book.add_sheet(&name, after.as_deref()).map(drop)
                }
                XlsxEffect::RenameSheet { from, to } => self.book.rename_sheet(&from, &to),
                XlsxEffect::DeleteSheet { name } => self.book.delete_sheet(&name),
                XlsxEffect::MoveSheet { name, to } => self.book.move_sheet(&name, to as usize),
            };
            result.map_err(|e| format!("{description}: {e}"))?;
        }
        Ok(())
    }
}
