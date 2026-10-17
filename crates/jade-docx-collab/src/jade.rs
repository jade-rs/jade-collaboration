//! Realising model effects in a jade `WordDocument`.

use futures::executor::block_on;
use jade_collab_sync::{ApplyContext, Materializer, Rejection};
use jade_docx_document_core::document::{BodyElement, Table};
use jade_docx_document_core::headless_api::apply_headless_commands_to_document;
use jade_docx_document_core::selection::TextPosition;
use jade_docx_document_core::word_document::{DocxExportError, WordDocument};
use jade_docx_document_core::Options;
use jade_docx_headless_api::{HeadlessCommand, HeadlessEditRequest};

use crate::model::{DocxEffect, DocxModel, DocxOp};

/// The server's jade document. Not `Send` (jade shares parsed parts through
/// `Rc`), so a server keeps one per document on its own thread.
pub struct JadeDocx {
    word: WordDocument,
}

impl JadeDocx {
    pub fn new(word: WordDocument) -> Self {
        Self { word }
    }

    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        block_on(WordDocument::load(bytes, &Options::default()))
            .map(Self::new)
            .map_err(|e| e.to_string())
    }

    pub fn word(&self) -> &WordDocument {
        &self.word
    }

    pub fn export(&self) -> Result<Vec<u8>, DocxExportError> {
        self.word.export_docx()
    }

    /// jade paragraph-surface indices of the body paragraphs (table cells
    /// included; notes, text boxes and equations excluded), in order. The
    /// model's paragraph ordinals index into this.
    pub fn body_surfaces(&self) -> Vec<usize> {
        (0..self.word.paragraph_count())
            .filter(|&i| self.word.is_body_paragraph_surface(i))
            .collect()
    }

    pub fn body_paragraph_texts(&self) -> Vec<String> {
        self.body_surfaces()
            .into_iter()
            .map(|i| {
                self.word
                    .paragraph(i)
                    .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// (rows, widest row) of each top-level body table.
    pub fn table_shapes(&self) -> Vec<(usize, usize)> {
        let Some(body) = self.word.document.as_ref().and_then(|d| d.body.as_ref()) else {
            return Vec::new();
        };
        body.elements
            .iter()
            .filter_map(|e| match e {
                BodyElement::Table(t) => Some((
                    t.rows.len(),
                    t.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0),
                )),
                _ => None,
            })
            .collect()
    }

    /// jade's `table_order_index` of the `k`-th table of the body's table
    /// structure (pre-order, nested tables included). jade's own order also
    /// counts tables inside text boxes, which the model does not see, so the
    /// two are matched by identity.
    fn table_order_index(&self, k: usize) -> Result<usize, String> {
        fn collect<'a>(elements: &'a [BodyElement], out: &mut Vec<&'a Table>) {
            for element in elements {
                if let BodyElement::Table(table) = element {
                    out.push(table);
                    for row in &table.rows {
                        for cell in &row.cells {
                            collect(&cell.blocks, out);
                        }
                    }
                }
            }
        }
        let body = self
            .word
            .document
            .as_ref()
            .and_then(|d| d.body.as_ref())
            .ok_or("the document has no body")?;
        let mut structure = Vec::new();
        collect(&body.elements, &mut structure);
        let target = *structure.get(k).ok_or_else(|| {
            format!(
                "table {k} is not among jade's {} body tables",
                structure.len()
            )
        })?;
        (0..)
            .map_while(|i| self.word.table_by_order(i))
            .position(|t| std::ptr::eq(t, target))
            .ok_or_else(|| format!("jade has no table order index for body table {k}"))
    }

    fn rendered(&self, surface: usize, offset: usize) -> usize {
        self.word
            .model_to_rendered_text_position(TextPosition::new(surface, offset))
            .offset
    }

    fn command(&self, effect: DocxEffect) -> Result<HeadlessCommand, String> {
        let surfaces = self.body_surfaces();
        let surface = |p: usize| {
            surfaces.get(p).copied().ok_or_else(|| {
                format!(
                    "paragraph {p} is not among jade's {} body paragraphs",
                    surfaces.len()
                )
            })
        };
        Ok(match effect {
            DocxEffect::InsertText {
                paragraph,
                offset,
                text,
            } => {
                let s = surface(paragraph)?;
                HeadlessCommand::InsertText {
                    paragraph: s,
                    offset: self.rendered(s, offset),
                    text,
                }
            }
            DocxEffect::DeleteRange {
                paragraph,
                start,
                end,
            } => {
                let s = surface(paragraph)?;
                HeadlessCommand::ReplaceRange {
                    start_paragraph: s,
                    start_offset: self.rendered(s, start),
                    end_paragraph: s,
                    end_offset: self.rendered(s, end),
                    text: String::new(),
                }
            }
            DocxEffect::SplitParagraph { paragraph, offset } => {
                let s = surface(paragraph)?;
                HeadlessCommand::InsertParagraphBreak {
                    paragraph: s,
                    offset: self.rendered(s, offset),
                    count: 1,
                }
            }
            DocxEffect::JoinParagraph { paragraph, offset } => {
                let (s, next) = (surface(paragraph)?, surface(paragraph + 1)?);
                HeadlessCommand::ReplaceRange {
                    start_paragraph: s,
                    start_offset: self.rendered(s, offset),
                    end_paragraph: next,
                    end_offset: self.rendered(next, 0),
                    text: String::new(),
                }
            }
            DocxEffect::InsertTable {
                paragraph,
                above,
                rows,
                columns,
            } => HeadlessCommand::InsertTable {
                after_paragraph: surface(paragraph)?,
                rows,
                columns,
                column_widths: Vec::new(),
                caret_offset: above.then_some(0),
            },
            DocxEffect::InsertTableRow {
                table,
                row,
                template,
            } => HeadlessCommand::InsertTableRow {
                table_order_index: self.table_order_index(table)?,
                row_index: row,
                template_row: template,
            },
            DocxEffect::DeleteTableRow { table, row } => HeadlessCommand::DeleteTableRow {
                table_order_index: self.table_order_index(table)?,
                row_index: row,
            },
            DocxEffect::InsertTableColumn {
                table,
                column,
                template,
            } => HeadlessCommand::InsertTableColumn {
                table_order_index: self.table_order_index(table)?,
                column_index: column,
                width: None,
                template_column: template,
            },
            DocxEffect::DeleteTableColumn { table, column } => HeadlessCommand::DeleteTableColumn {
                table_order_index: self.table_order_index(table)?,
                column_index: column,
            },
            DocxEffect::DeleteTable { table } => HeadlessCommand::DeleteTable {
                table_order_index: self.table_order_index(table)?,
            },
        })
    }
}

impl Materializer<DocxModel> for JadeDocx {
    fn admit(&self, model: &DocxModel, op: &DocxOp) -> Result<(), Rejection> {
        if let DocxOp::JoinParagraph { story, mark } = op {
            let Some(paragraph) = model.paragraph_ending_with(*story, *mark) else {
                return Ok(()); // the model rejects it
            };
            let surfaces = self.body_surfaces();
            // Between two body paragraphs jade may list a text box's or a
            // note's paragraphs; a range across them would remove those.
            if let (Some(a), Some(b)) = (surfaces.get(paragraph), surfaces.get(paragraph + 1)) {
                if a + 1 != *b {
                    return Err(Rejection::new(
                        "joining a paragraph across a text box or note is not supported yet",
                    ));
                }
            }
        }
        Ok(())
    }

    fn materialize(
        &mut self,
        _: &DocxModel,
        effects: Vec<DocxEffect>,
        _: &ApplyContext,
    ) -> Result<(), String> {
        for effect in effects {
            let description = format!("{effect:?}");
            let command = self.command(effect)?;
            apply_headless_commands_to_document(
                &mut self.word,
                &HeadlessEditRequest {
                    commands: vec![command],
                },
            )
            .map_err(|e| format!("{description}: {e}"))?;
        }
        Ok(())
    }
}
