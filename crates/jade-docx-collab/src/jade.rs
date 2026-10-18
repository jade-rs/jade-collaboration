//! Realising model effects in a jade `WordDocument`.

use std::collections::{BTreeMap, BTreeSet};

use futures::executor::block_on;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Materializer, Rejection};
use jade_docx_document_core::document::{BodyElement, Table};
use jade_docx_document_core::headless_api::apply_headless_commands_to_document;
use jade_docx_document_core::selection::TextPosition;
use jade_docx_document_core::word_document::{DocxExportError, WordDocument};
use jade_docx_document_core::Options;
use jade_docx_headless_api::{HeadlessCommand, HeadlessEditRequest};

use crate::model::{DocxEffect, DocxModel, DocxOp, FormatChange};

/// The server's jade document. Not `Send` (jade shares parsed parts through
/// `Rc`), so a server keeps one per document on its own thread.
pub struct JadeDocx {
    word: WordDocument,
    /// jade's `w:id` of each comment made in the session. jade numbers new
    /// comments itself (reusing freed numbers), so the materializer records
    /// the number each one got rather than predicting it.
    comments: BTreeMap<Id, String>,
}

impl JadeDocx {
    pub fn new(word: WordDocument) -> Self {
        Self {
            word,
            comments: BTreeMap::new(),
        }
    }

    /// jade's `w:id` of the model's comment `id`: the number jade gave it, or
    /// the one it had in the file.
    pub fn comment_id(&self, model: &DocxModel, id: Id) -> Option<String> {
        self.comments
            .get(&id)
            .cloned()
            .or_else(|| model.comment_file_id(id).map(str::to_string))
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

    /// Applies the same effects to a copy of the document and reports what
    /// jade says — how `admit` learns that jade cannot realise an operation
    /// the model accepts (a reply whose root's text is gone, say).
    fn preflight(&self, model: &DocxModel, op: &DocxOp) -> Result<(), Rejection> {
        let ctx = ApplyContext {
            origin: ReplicaId::ORIGIN,
            base_seq: 0,
            seq: None,
        };
        let mut after = model.clone();
        let Ok(effects) = after.apply(op, &ctx) else {
            return Ok(()); // the model rejects it itself
        };
        if effects.is_empty() {
            return Ok(());
        }
        let mut trial = JadeDocx {
            word: self.word.clone(),
            comments: self.comments.clone(),
        };
        trial
            .materialize(&after, effects, &ctx)
            .map_err(|e| Rejection::new(format!("jade cannot apply this: {e}")))
    }

    fn jade_comment(&self, model: &DocxModel, id: Id) -> Result<String, String> {
        self.comment_id(model, id)
            .ok_or_else(|| format!("comment {id} has no jade comment"))
    }

    fn format_command(
        &self,
        s: usize,
        start: usize,
        end: usize,
        change: FormatChange,
    ) -> Result<HeadlessCommand, String> {
        // Unlike the text commands, jade's run-formatting commands count
        // offsets in the runs' text (the model's), not in rendered text.
        let (start_paragraph, end_paragraph) = (s, s);
        let (start_offset, end_offset) = (start, end);
        Ok(match change {
            FormatChange::Bold { value } => HeadlessCommand::ApplyBold {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                enabled: value,
            },
            FormatChange::Italic { value } => HeadlessCommand::ApplyItalic {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                enabled: value,
            },
            FormatChange::Underline { value, style } => HeadlessCommand::ApplyUnderline {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                kind: style,
                enabled: value,
            },
            FormatChange::Strike { value, double } => HeadlessCommand::ApplyStrike {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                double,
                enabled: value,
            },
            FormatChange::FontSize { half_points } => HeadlessCommand::SetFontSize {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                font_size: half_points as f32 / 2.0,
            },
            FormatChange::FontFamily { family } => HeadlessCommand::SetFontFamily {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                font_family: family,
            },
            FormatChange::FontColor { color } => HeadlessCommand::SetFontColor {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                color,
            },
            FormatChange::Highlight { color } => HeadlessCommand::SetHighlightColor {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                color,
            },
            FormatChange::ClearFormatting => HeadlessCommand::ClearFormatting {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
            },
            FormatChange::Alignment { .. } | FormatChange::ParagraphStyle { .. } => {
                return Err(format!("{change:?} formats paragraphs, not text"))
            }
        })
    }

    fn command(&self, model: &DocxModel, effect: DocxEffect) -> Result<HeadlessCommand, String> {
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
            DocxEffect::FormatText {
                paragraph,
                start,
                end,
                change,
            } => self.format_command(surface(paragraph)?, start, end, change)?,
            DocxEffect::FormatParagraph { paragraph, change } => {
                let s = surface(paragraph)?;
                match change {
                    FormatChange::Alignment { alignment } => {
                        HeadlessCommand::SetParagraphAlignment {
                            start_paragraph: s,
                            end_paragraph: s,
                            alignment: alignment.as_jade().to_string(),
                        }
                    }
                    FormatChange::ParagraphStyle { style_id } => {
                        HeadlessCommand::SetParagraphStyle {
                            start_paragraph: s,
                            end_paragraph: s,
                            style_id,
                        }
                    }
                    other => return Err(format!("{other:?} formats text, not paragraphs")),
                }
            }
            DocxEffect::AddComment {
                start_paragraph,
                start,
                end_paragraph,
                end,
                content,
                ..
            } => {
                let (first, last) = (surface(start_paragraph)?, surface(end_paragraph)?);
                HeadlessCommand::AddComment {
                    start_paragraph: first,
                    // Like formatting, comments count offsets in the runs' text.
                    start_offset: Some(start),
                    end_paragraph: last,
                    end_offset: Some(end),
                    text: content.text,
                    author: content.author,
                    initials: content.initials,
                    date: content.date,
                }
            }
            DocxEffect::ReplyToComment {
                comment, content, ..
            } => HeadlessCommand::ReplyToComment {
                comment_id: self.jade_comment(model, comment)?,
                text: content.text,
                author: content.author,
                initials: content.initials,
                date: content.date,
            },
            DocxEffect::ResolveComment { comment, resolved } => HeadlessCommand::ResolveComment {
                comment_id: self.jade_comment(model, comment)?,
                resolved,
            },
            DocxEffect::DeleteComment { comment } => HeadlessCommand::DeleteComment {
                comment_id: self.jade_comment(model, comment)?,
            },
        })
    }
}

impl Materializer<DocxModel> for JadeDocx {
    fn admit(&self, model: &DocxModel, op: &DocxOp) -> Result<(), Rejection> {
        if matches!(
            op,
            DocxOp::Format { .. }
                | DocxOp::AddComment { .. }
                | DocxOp::ReplyToComment { .. }
                | DocxOp::ResolveComment { .. }
                | DocxOp::DeleteComment { .. }
        ) {
            return self.preflight(model, op);
        }
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
        model: &DocxModel,
        effects: Vec<DocxEffect>,
        _: &ApplyContext,
    ) -> Result<(), String> {
        for effect in effects {
            let description = format!("{effect:?}");
            let created = match effect {
                DocxEffect::AddComment { comment, .. } => Some(comment),
                DocxEffect::ReplyToComment { reply, .. } => Some(reply),
                _ => None,
            };
            let before: BTreeSet<String> = self.word.comments.keys().cloned().collect();
            let command = self.command(model, effect)?;
            apply_headless_commands_to_document(
                &mut self.word,
                &HeadlessEditRequest {
                    commands: vec![command],
                },
            )
            .map_err(|e| format!("{description}: {e}"))?;
            if let Some(comment) = created {
                // The one comment jade did not have before is this one.
                let mut new = self
                    .word
                    .comments
                    .keys()
                    .filter(|key| !before.contains(*key));
                match (new.next(), new.next()) {
                    (Some(key), None) => {
                        self.comments.insert(comment, key.clone());
                    }
                    _ => return Err(format!("{description}: jade made no single new comment")),
                }
            }
        }
        Ok(())
    }
}
