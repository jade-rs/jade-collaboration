//! Realising model effects in a jade `HeadlessWorkbook`.

use jade_collab_sync::{ApplyContext, Materializer};
use jade_xlsx_headless_editor::HeadlessWorkbook;

use crate::model::{column_label, XlsxEffect, XlsxModel};

/// The author jade records on notes written through the model; the model
/// does not track authors. (jade's own MCP server uses the same default.)
const NOTE_AUTHOR: &str = "jade";

/// The server's jade workbook. Every write patches the original package, so
/// parts the model does not describe survive untouched.
pub struct JadeXlsx {
    book: HeadlessWorkbook,
}

impl JadeXlsx {
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
                XlsxEffect::InsertColumn { sheet, at } => self
                    .book
                    .insert_columns(&sheet, &column_label(at), 1)
                    .map(drop),
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
            };
            result.map_err(|e| format!("{description}: {e}"))?;
        }
        Ok(())
    }
}
