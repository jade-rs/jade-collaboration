//! Realising model effects in a jade `WordDocument`.

use std::collections::{BTreeMap, BTreeSet};

use futures::executor::block_on;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Materializer, Rejection};
use jade_docx_document_core::document::RevisionKind as JadeKind;
use jade_docx_document_core::document::{BodyElement, Note, Table};
use jade_docx_document_core::headless_api::apply_headless_commands_to_document;
use jade_docx_document_core::selection::TextPosition;
use jade_docx_document_core::word_document::{
    DocxExportError, ParagraphAnchorKind, ParagraphStory, WordDocument,
};
use jade_docx_document_core::Options;
use jade_docx_headless_api::{
    HeadlessCommand, HeadlessEditRequest, HeadlessRevisionKindFilter, HeadlessRevisionTarget,
    HeadlessShapeKind, HeadlessTableCellSelection,
};

use crate::model::{DocxEffect, DocxModel, DocxOp, FormatChange, NoteKind, Numbered, StoryId};

/// The server's jade document. Not `Send` (jade shares parsed parts through
/// `Rc`), so a server keeps one per document on its own thread.
#[derive(Clone)]
///
/// It holds the *base* document: the file, every text and structural edit,
/// and the sequenced formatting, in log order — but none of the model's
/// marks. The marks are realised on a copy when the file is written
/// ([`JadeDocx::formatted`], [`JadeDocx::export`]), as explicit values on
/// exactly the characters they cover. So jade's typing rules only ever see
/// the base formatting: text typed next to a mark gets the mark's
/// formatting when the mark's expand rule covers it, and the formatting
/// around it otherwise — never jade's guess from the mark's run.
pub struct JadeDocx {
    word: WordDocument,
    /// The number jade gave each comment (and note) made since the document
    /// was loaded. jade numbers them itself, reusing freed numbers, so the
    /// materializer records what it got rather than predicting it, and the
    /// server commits it to the model as a derived [`DocxOp::Numbered`] —
    /// which is how a restored server, whose jade has just read the file,
    /// still knows them.
    numbered: BTreeMap<Numbered, String>,
    /// Numbers learnt while realising the current operation, not yet derived.
    fresh: Vec<(Numbered, String)>,
}

impl JadeDocx {
    pub fn new(mut word: WordDocument) -> Self {
        crate::bindings::refresh(&mut word);
        Self {
            word,
            numbered: BTreeMap::new(),
            fresh: Vec::new(),
        }
    }

    /// jade's `w:id` of the model's comment `id`: the one it had in the file
    /// or the number jade gave it.
    pub fn comment_id(&self, model: &DocxModel, id: Id) -> Option<String> {
        model
            .comment_file_id(id)
            .map(str::to_string)
            .or_else(|| self.numbered.get(&Numbered::Comment { id }).cloned())
    }

    /// The `w:id`s of jade's notes of `kind`.
    fn note_ids(&self, kind: NoteKind) -> BTreeSet<String> {
        self.word
            .document
            .as_ref()
            .map(|d| match kind {
                NoteKind::Footnote => d.footnotes.keys().cloned().collect(),
                NoteKind::Endnote => d.endnotes.keys().cloned().collect(),
            })
            .unwrap_or_default()
    }

    /// Records the number jade gave `item`.
    fn learn(&mut self, item: Numbered, file_id: String) {
        self.numbered.insert(item, file_id.clone());
        self.fresh.push((item, file_id));
    }

    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        let bytes = crate::revision_metadata::read_package(bytes)?;
        let mut word = block_on(WordDocument::load(&bytes, &Options::default()))
            .map_err(|error| error.to_string())?;
        crate::revision_metadata::loaded(&mut word)?;
        Ok(Self::new(word))
    }

    pub fn word(&self) -> &WordDocument {
        &self.word
    }

    /// Restore the base file alongside its authoritative model. Older
    /// checkpoints may predate persistence of the tracking setting.
    pub fn restore(bytes: &[u8], model: &DocxModel) -> Result<Self, String> {
        let mut jade = Self::load(bytes)?;
        if crate::settings::tracking(jade.word())? != model.tracking() {
            crate::settings::set_tracking(&mut jade.word, model.tracking())?;
        }
        Ok(jade)
    }

    /// The file with the model's formatting: what users download.
    pub fn export(&self, model: &DocxModel) -> Result<Vec<u8>, String> {
        let bytes = self
            .formatted(model)?
            .export_docx()
            .map_err(|error| error.to_string())?;
        crate::revision_metadata::write_package(&bytes)
    }

    /// The base file, without the model's marks: what a checkpoint stores
    /// next to the model, which holds the marks.
    pub fn export_base(&self) -> Result<Vec<u8>, DocxExportError> {
        let bytes = self.word.export_docx()?;
        crate::revision_metadata::write_package(&bytes).map_err(|error| {
            DocxExportError::Xml(jade_docx_document_core::docx_xml::DocxXmlError::Malformed(
                error,
            ))
        })
    }

    /// A copy of the document showing the model's formatting: each run of
    /// characters a mark sets gets that value explicitly; everything else
    /// keeps jade's own formatting. The persisted tracking switch also
    /// reflects the model, including models from older checkpoints.
    pub fn formatted(&self, model: &DocxModel) -> Result<WordDocument, String> {
        let formatting = model.formatting();
        let mut copy = JadeDocx::new(self.word.clone());
        if crate::settings::tracking(copy.word())? != model.tracking() {
            crate::settings::set_tracking(&mut copy.word, model.tracking())?;
        }
        for format in formatting {
            let surface = copy.surface(&format.paragraph)?;
            let command = copy.format_command(surface, format.start, format.end, format.change)?;
            copy.run(command)?;
        }
        Ok(copy.word)
    }

    /// jade's paragraph surfaces the model lists, in order: every one but
    /// equations and note separators. The model's paragraph ordinals index
    /// into this.
    pub fn paragraph_surfaces(&self) -> Vec<usize> {
        let document = self.word.document.as_ref();
        let real = |notes: Option<&BTreeMap<String, Note>>, id: &str| {
            notes
                .and_then(|notes| notes.get(id))
                .is_some_and(|note| note.note_type.is_none())
        };
        (0..self.word.paragraph_count())
            .filter(|&s| match self.word.paragraph_story(s) {
                None => false,
                Some(ParagraphStory::Footnote { note_id, .. }) => {
                    real(document.map(|d| &d.footnotes), &note_id)
                }
                Some(ParagraphStory::Endnote { note_id, .. }) => {
                    real(document.map(|d| &d.endnotes), &note_id)
                }
                Some(_) => true,
            })
            .collect()
    }

    /// The text of each paragraph the model lists, in its order.
    pub fn paragraph_texts(&self) -> Vec<String> {
        self.paragraph_surfaces()
            .into_iter()
            .map(|i| self.paragraph_text(i))
            .collect()
    }

    fn paragraph_text(&self, surface: usize) -> String {
        self.word
            .paragraph(surface)
            .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect())
            .unwrap_or_default()
    }

    /// jade paragraph-surface indices of the body paragraphs (table cells
    /// included; notes, text boxes and equations excluded), in order.
    pub fn body_surfaces(&self) -> Vec<usize> {
        (0..self.word.paragraph_count())
            .filter(|&i| self.word.is_body_paragraph_surface(i))
            .collect()
    }

    pub fn body_paragraph_texts(&self) -> Vec<String> {
        self.body_surfaces()
            .into_iter()
            .map(|i| self.paragraph_text(i))
            .collect()
    }

    /// (rows, widest row in grid columns) of each top-level body table.
    pub fn table_shapes(&self) -> Vec<(usize, usize)> {
        let Some(body) = self.word.document.as_ref().and_then(|d| d.body.as_ref()) else {
            return Vec::new();
        };
        body.elements
            .iter()
            .filter_map(|e| match e {
                // Columns of the grid: a merged cell spans several.
                BodyElement::Table(t) => Some((
                    t.rows.len(),
                    t.rows
                        .iter()
                        .map(|r| {
                            r.cells
                                .iter()
                                .map(|c| c.span.unwrap_or(1).max(1) as usize)
                                .sum::<usize>()
                        })
                        .max()
                        .unwrap_or(0),
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
        let mut trial = self.clone();
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

    /// The paragraph surface `at` names now.
    fn surface(&self, at: &ParagraphStory) -> Result<usize, String> {
        self.word
            .paragraph_story_surface(at)
            .ok_or_else(|| format!("jade has no paragraph {at:?}"))
    }

    /// The rendered offset `offset` bytes into the paragraph at `surface`,
    /// after `anchors` of its text boxes and note references (and their
    /// marks). jade must have exactly that many at or before the offset.
    fn rendered_after(
        &self,
        surface: usize,
        offset: usize,
        anchors: usize,
    ) -> Result<usize, String> {
        let listed = self.word.paragraph_anchors(surface);
        let before = listed.iter().filter(|a| a.offset < offset).count();
        let at = listed.iter().filter(|a| a.offset == offset).count();
        if anchors < before || anchors > before + at {
            return Err(format!(
                "jade has {before} text boxes or note references before offset {offset} of \
                 paragraph {surface} and {at} at it; the model has {anchors} before it"
            ));
        }
        let marks: usize = listed[before..anchors].iter().map(|a| a.rendered_len).sum();
        Ok(self.rendered(surface, offset) + marks)
    }

    /// How many paragraph surfaces jade lists without a paragraph behind
    /// them (equations, and text-box paragraphs it lost track of).
    fn unlisted(&self) -> usize {
        (0..self.word.paragraph_count())
            .filter(|&s| self.word.paragraph(s).is_none())
            .count()
    }

    fn run(&mut self, command: HeadlessCommand) -> Result<(), String> {
        crate::bindings::refresh(&mut self.word);
        let description = format!("{command:?}");
        apply_headless_commands_to_document(
            &mut self.word,
            &HeadlessEditRequest {
                commands: vec![command],
            },
        )
        .map_err(|e| format!("{description}: {e}"))
    }

    /// Runs `command(offset)` — inserting a text box or note reference, or
    /// splitting, at a rendered offset — so that it takes effect `offset`
    /// bytes into the paragraph at surface `s` after `anchors` of its text
    /// boxes and note references.
    ///
    /// jade inserts before the anchors already at an offset, and its split
    /// takes them to the second half. To act after some of them, the
    /// materializer types a placeholder after those, acts right after the
    /// placeholder — before the anchors that moved on with it — and deletes
    /// the placeholder again.
    fn at_anchors(
        &mut self,
        s: usize,
        offset: usize,
        anchors: usize,
        command: impl FnOnce(usize) -> HeadlessCommand,
        session: Option<&Session>,
    ) -> Result<(), String> {
        self.among_anchors(s, offset, anchors, command, session, false)
    }

    /// [`Self::at_anchors`]; with `always`, through the placeholder even
    /// when jade would act in the right place by itself.
    fn among_anchors(
        &mut self,
        s: usize,
        offset: usize,
        anchors: usize,
        command: impl FnOnce(usize) -> HeadlessCommand,
        session: Option<&Session>,
        always: bool,
    ) -> Result<(), String> {
        let before = self
            .word
            .paragraph_anchors(s)
            .iter()
            .filter(|a| a.offset < offset)
            .count();
        if anchors == before && !always {
            let at = self.rendered(s, offset);
            return self.run_as(command(at), session);
        }
        let Some(after) = anchors.checked_sub(before) else {
            return Err(format!(
                "jade has {before} text boxes or note references before offset {offset} of \
                 paragraph {s}; the model has {anchors}"
            ));
        };
        let saved = self.word.clone();
        let result = (|| {
            self.run(HeadlessCommand::InsertTextAmongAnchors {
                paragraph: s,
                offset: self.rendered(s, offset),
                after_anchors: after,
                text: PLACEHOLDER.to_string(),
            })?;
            self.run_as(
                command(self.rendered(s, offset + PLACEHOLDER.len())),
                session,
            )?;
            let start = self.rendered_after(s, offset, anchors)?;
            self.run(HeadlessCommand::ReplaceRange {
                start_paragraph: s,
                start_offset: start,
                end_paragraph: s,
                end_offset: start + PLACEHOLDER.len(),
                text: String::new(),
            })
        })();
        if result.is_err() {
            self.word = saved;
        }
        result
    }

    /// Runs `command` with jade's revision session on for `session` (and
    /// off again afterwards, whatever happens), or plainly. The session is
    /// off between commands: jade's is document-wide, the model's per
    /// operation.
    fn run_as(
        &mut self,
        command: HeadlessCommand,
        session: Option<&Session>,
    ) -> Result<(), String> {
        let Some(session) = session else {
            return self.run(command);
        };
        self.run(HeadlessCommand::StartChangeTracking {
            author: session.author.clone(),
            initials: None,
            date: session.date.clone(),
            date_utc: None,
        })?;
        let result = self.run(command);
        let stopped = self.run(HeadlessCommand::StopChangeTracking);
        result.and(stopped)
    }

    /// Accepts (`accept`) or rejects the deletion (`deletion`) or the
    /// insertion jade shows on `start..end` of the paragraph at `at`
    /// (offsets in the runs' text), by targeting the part of each run there
    /// with its own `w:id`.
    fn decide_range(
        &mut self,
        at: &ParagraphStory,
        start: usize,
        end: usize,
        deletion: bool,
        accept: bool,
    ) -> Result<(), String> {
        let s = self.surface(at)?;
        let runs = &self
            .word
            .paragraph(s)
            .ok_or_else(|| format!("jade has no paragraph {s}"))?
            .runs;
        let mut targets: Vec<(Option<HeadlessRevisionKindFilter>, HeadlessRevisionTarget)> =
            Vec::new();
        let mut offset = 0;
        for run in runs {
            let (from, to) = (offset, offset + run.text.len());
            offset = to;
            let (from, to) = (from.max(start), to.min(end));
            if from >= to {
                continue;
            }
            let Some(mark) = run.revision.as_ref() else {
                return Err(format!(
                    "no tracked change at {from}..{to} of paragraph {s}"
                ));
            };
            let layer = if deletion {
                Some(mark).filter(|m| m.is_deletion())
            } else {
                mark.insertion_layer()
            }
            .ok_or_else(|| {
                format!("no tracked change to decide at {from}..{to} of paragraph {s}")
            })?;
            // The kind keeps a target on its layer when a file gives both
            // layers the same (or no) w:id; moves have no kind filter.
            let kind = match layer.kind {
                JadeKind::Inserted => Some(HeadlessRevisionKindFilter::Inserted),
                JadeKind::Deleted => Some(HeadlessRevisionKindFilter::Deleted),
                JadeKind::MovedFrom | JadeKind::MovedTo => None,
            };
            targets.push((
                kind,
                HeadlessRevisionTarget {
                    paragraph: s,
                    start_offset: from,
                    end_offset: to,
                    paragraph_mark: false,
                    id: layer.id.clone(),
                },
            ));
        }
        // From the end: a decision that removes text leaves the offsets
        // before it as they were.
        for (kind, target) in targets.into_iter().rev() {
            let targets = Some(vec![target]);
            self.run(if !accept {
                HeadlessCommand::RejectRevisions {
                    kind,
                    start_paragraph: None,
                    end_paragraph: None,
                    revision_id: None,
                    revision_ids: None,
                    revision_targets: targets,
                }
            } else {
                HeadlessCommand::AcceptRevisions {
                    kind,
                    start_paragraph: None,
                    end_paragraph: None,
                    revision_id: None,
                    revision_ids: None,
                    revision_targets: targets,
                }
            })?;
        }
        Ok(())
    }

    /// Joins the paragraph at `at` (its text `len` bytes long) with the next
    /// paragraph of its story, which jade must list right after it.
    fn join(&mut self, at: &ParagraphStory, len: usize) -> Result<(), String> {
        if matches!(
            at,
            ParagraphStory::Footnote { .. } | ParagraphStory::Endnote { .. }
        ) {
            // Editing the host can leave jade's note XML bindings stale.
            // Rebind from the persisted parts before moving note runs so
            // the merge keeps the second paragraph's text and revisions.
            let bytes = self.export_base().map_err(|e| e.to_string())?;
            self.word = Self::load(&bytes)?.word;
        }
        let (first, second) = (self.surface(at)?, self.surface(&next_paragraph(at))?);
        if first + 1 != second {
            return Err(format!(
                "jade lists a text box or note between {at:?} and the next paragraph"
            ));
        }
        // Keep trailing references on the first side of the range using the
        // same temporary placeholder as anchor-aware splits. A collapsed
        // backspace can lose the second paragraph's text box surfaces while
        // removing its host, before moving those runs into the first.
        let trailing = self
            .word
            .paragraph_anchors(first)
            .iter()
            .any(|a| a.offset == len);
        if trailing {
            let anchors = self.word.paragraph_anchors(first).len();
            let end_offset = self.rendered(second, 0);
            return self.at_anchors(
                first,
                len,
                anchors,
                |offset| HeadlessCommand::ReplaceRange {
                    start_paragraph: first,
                    start_offset: offset,
                    end_paragraph: second,
                    end_offset,
                    text: String::new(),
                },
                None,
            );
        }
        let command = HeadlessCommand::ReplaceRange {
            start_paragraph: first,
            start_offset: self.rendered(first, len),
            end_paragraph: second,
            end_offset: self.rendered(second, 0),
            text: String::new(),
        };
        self.run(command)
    }

    /// Inserts `text` `offset` bytes into the paragraph at `at`, after
    /// `anchors` of its text boxes and note references.
    ///
    /// jade's own typing goes right after the character before the offset
    /// (before the anchors there), or at a paragraph's start after the
    /// anchors there. Concurrent edits can order text otherwise around the
    /// anchors at an offset — after them, before them, or between two of
    /// them; jade's `insert_text_among_anchors` names how many come first.
    fn insert_text(
        &mut self,
        at: &ParagraphStory,
        offset: usize,
        anchors: usize,
        text: String,
        session: Option<&Session>,
    ) -> Result<(), String> {
        let s = self.surface(at)?;
        let listed = self.word.paragraph_anchors(s);
        let before = listed.iter().filter(|a| a.offset < offset).count();
        let here = listed.iter().filter(|a| a.offset == offset).count();
        let natural = if offset == 0 { here } else { before };
        if anchors == natural {
            let command = HeadlessCommand::InsertText {
                paragraph: s,
                offset: self.rendered(s, offset),
                text,
            };
            return self.run_as(command, session);
        }
        if anchors < before || anchors > before + here {
            return Err(format!(
                "jade has {before} text boxes or note references before offset {offset} of \
                 {at:?} and {here} at it; the model has {anchors} before the text"
            ));
        }
        if session.is_some() {
            // jade's insert_text_among_anchors writes plain text; a tracked
            // insertion is typed right after a placeholder there instead.
            return self.among_anchors(
                s,
                offset,
                anchors,
                |offset| HeadlessCommand::InsertText {
                    paragraph: s,
                    offset,
                    text,
                },
                session,
                true,
            );
        }
        self.run(HeadlessCommand::InsertTextAmongAnchors {
            paragraph: s,
            offset: self.rendered(s, offset),
            after_anchors: anchors - before,
            text,
        })
    }

    /// Applies one effect to jade.
    fn realize(&mut self, model: &DocxModel, effect: DocxEffect) -> Result<(), String> {
        let command = match effect {
            DocxEffect::InsertText {
                paragraph,
                offset,
                text,
                anchors,
            } => return self.insert_text(&paragraph, offset, anchors, text, None),
            DocxEffect::TrackInsertText {
                paragraph,
                offset,
                text,
                anchors,
                author,
                date,
            } => {
                let session = Session { author, date };
                return self.insert_text(&paragraph, offset, anchors, text, Some(&session));
            }
            DocxEffect::DeleteRange {
                paragraph,
                start,
                end,
                anchors,
            } => {
                let s = self.surface(&paragraph)?;
                HeadlessCommand::ReplaceRange {
                    start_paragraph: s,
                    start_offset: self.rendered_after(s, start, anchors)?,
                    end_paragraph: s,
                    end_offset: self.rendered_after(s, end, anchors)?,
                    text: String::new(),
                }
            }
            DocxEffect::TrackDeleteRange {
                paragraph,
                start,
                end,
                anchors,
                author,
                date,
            } => {
                let s = self.surface(&paragraph)?;
                let command = HeadlessCommand::ReplaceRange {
                    start_paragraph: s,
                    start_offset: self.rendered_after(s, start, anchors)?,
                    end_paragraph: s,
                    end_offset: self.rendered_after(s, end, anchors)?,
                    text: String::new(),
                };
                return self.run_as(command, Some(&Session { author, date }));
            }
            DocxEffect::RestoreRange {
                paragraph,
                start,
                end,
            } => return self.decide_range(&paragraph, start, end, true, false),
            DocxEffect::RemoveDeletedRange {
                paragraph,
                start,
                end,
            } => return self.decide_range(&paragraph, start, end, true, true),
            DocxEffect::AcceptInsertionRange {
                paragraph,
                start,
                end,
            } => return self.decide_range(&paragraph, start, end, false, true),
            DocxEffect::SetTracking { on } => {
                return crate::settings::set_tracking(&mut self.word, on)
            }
            DocxEffect::SplitParagraph {
                paragraph,
                offset,
                anchors,
            } => {
                // jade's split takes the anchors at the offset along to the
                // second half; a placeholder keeps the ones that stay.
                let s = self.surface(&paragraph)?;
                return self.at_anchors(
                    s,
                    offset,
                    anchors,
                    |offset| HeadlessCommand::InsertParagraphBreak {
                        paragraph: s,
                        offset,
                        count: 1,
                    },
                    None,
                );
            }
            DocxEffect::JoinParagraph { paragraph, offset } => {
                return self.join(&paragraph, offset)
            }
            DocxEffect::InsertTable {
                paragraph,
                above,
                rows,
                columns,
            } => HeadlessCommand::InsertTable {
                after_paragraph: self.surface(&paragraph)?,
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
            DocxEffect::MergeTableCells { table, cells } => HeadlessCommand::MergeTableCells {
                table_order_index: self.table_order_index(table)?,
                selected_cells: cells
                    .into_iter()
                    .map(|(row, column)| HeadlessTableCellSelection {
                        row,
                        column,
                        column_span: 1,
                    })
                    .collect(),
            },
            DocxEffect::UnmergeTableCells {
                table,
                row,
                column,
                column_span,
            } => HeadlessCommand::UnmergeTableCells {
                table_order_index: self.table_order_index(table)?,
                target_cells: vec![HeadlessTableCellSelection {
                    row,
                    column,
                    column_span,
                }],
            },
            DocxEffect::DeleteTable { table } => HeadlessCommand::DeleteTable {
                table_order_index: self.table_order_index(table)?,
            },
            DocxEffect::FormatText {
                paragraph,
                start,
                end,
                change,
            } => self.format_command(self.surface(&paragraph)?, start, end, change)?,
            DocxEffect::FormatParagraph { paragraph, change } => {
                let s = self.surface(&paragraph)?;
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
            } => HeadlessCommand::AddComment {
                start_paragraph: self.surface(&start_paragraph)?,
                // Like formatting, comments count offsets in the runs' text.
                start_offset: Some(start),
                end_paragraph: self.surface(&end_paragraph)?,
                end_offset: Some(end),
                text: content.text,
                author: content.author,
                initials: content.initials,
                date: content.date,
            },
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
            DocxEffect::UpdateComment { comment, text } => HeadlessCommand::UpdateComment {
                comment_id: self.jade_comment(model, comment)?,
                text,
            },
            DocxEffect::InsertNote {
                kind,
                note,
                paragraph,
                offset,
                anchors,
                text,
            } => {
                let s = self.surface(&paragraph)?;
                let before = self.note_ids(kind);
                self.at_anchors(
                    s,
                    offset,
                    anchors,
                    |offset| match kind {
                        NoteKind::Footnote => HeadlessCommand::InsertFootnote {
                            paragraph: s,
                            offset,
                            text,
                        },
                        NoteKind::Endnote => HeadlessCommand::InsertEndnote {
                            paragraph: s,
                            offset,
                            text,
                        },
                    },
                    None,
                )?;
                let mut new = self
                    .note_ids(kind)
                    .into_iter()
                    .filter(|k| !before.contains(k));
                return match (new.next(), new.next()) {
                    (Some(key), None) => {
                        self.learn(kind.numbered(note), key);
                        Ok(())
                    }
                    _ => Err("jade made no single new note".to_string()),
                };
            }
            DocxEffect::DeleteNote { kind, note } => {
                let story = match kind {
                    NoteKind::Footnote => StoryId::Footnote { id: note },
                    NoteKind::Endnote => StoryId::Endnote { id: note },
                };
                let key = model
                    .note_id(story)
                    .filter(|k| !k.is_empty())
                    .ok_or_else(|| format!("note {note} has no jade number"))?
                    .to_string();
                self.run(match kind {
                    NoteKind::Footnote => HeadlessCommand::DeleteFootnote {
                        footnote_id: key.clone(),
                    },
                    NoteKind::Endnote => HeadlessCommand::DeleteEndnote {
                        endnote_id: key.clone(),
                    },
                })?;
                // jade removes the references it knows of; one it left
                // (in a text box, say) would leave the model behind.
                let left = (0..self.word.paragraph_count()).any(|s| {
                    self.word
                        .paragraph_anchors(s)
                        .iter()
                        .any(|a| match &a.kind {
                            ParagraphAnchorKind::Footnote { note_id } => {
                                kind == NoteKind::Footnote && *note_id == key
                            }
                            ParagraphAnchorKind::Endnote { note_id } => {
                                kind == NoteKind::Endnote && *note_id == key
                            }
                            ParagraphAnchorKind::TextBox { .. } => false,
                        })
                });
                return if left {
                    Err(format!("jade kept a reference to {kind:?} {key}"))
                } else {
                    Ok(())
                };
            }
            DocxEffect::InsertTextBox {
                paragraph,
                offset,
                anchors,
                text,
                width_px,
                height_px,
            } => {
                let s = self.surface(&paragraph)?;
                return self.at_anchors(
                    s,
                    offset,
                    anchors,
                    |offset| HeadlessCommand::InsertShape {
                        paragraph: s,
                        offset,
                        shape: HeadlessShapeKind::Rectangle,
                        width_px: f64::from(width_px),
                        height_px: f64::from(height_px),
                        fill_color: None,
                        border_color: Some("000000".to_string()),
                        border_width_px: 1.0,
                        text,
                    },
                    None,
                );
            }
            DocxEffect::DeleteTextBox {
                paragraph,
                text_box,
            } => HeadlessCommand::DeleteTextBox {
                paragraph: self.surface(&paragraph)?,
                text_box,
            },
            DocxEffect::Numbered { item, file_id } => {
                // jade numbered it when it was made; on a replayed log it
                // must have given the same number again.
                self.fresh.retain(|(fresh, _)| *fresh != item);
                return match self.numbered.insert(item, file_id.clone()) {
                    Some(known) if known != file_id => Err(format!(
                        "jade numbered {item:?} {known}, the log says {file_id}"
                    )),
                    _ => Ok(()),
                };
            }
        };
        self.run(command)
    }
}

/// Typed and deleted again to put a text box or note reference after some
/// of the anchors at an offset.
const PLACEHOLDER: &str = "X";
/// Who a tracked edit is by, for jade's revision session.
struct Session {
    author: Option<String>,
    date: Option<String>,
}

/// The paragraph after `at` in its story.
fn next_paragraph(at: &ParagraphStory) -> ParagraphStory {
    let mut next = at.clone();
    match &mut next {
        ParagraphStory::Body { index } => *index += 1,
        ParagraphStory::TextBox { paragraph, .. }
        | ParagraphStory::Footnote { paragraph, .. }
        | ParagraphStory::Endnote { paragraph, .. } => *paragraph += 1,
    }
    next
}

impl Materializer<DocxModel> for JadeDocx {
    fn may_derive(&self, _: &DocxModel, cause: &DocxOp) -> bool {
        !self.fresh.is_empty()
            || matches!(
                cause,
                DocxOp::AddComment { .. }
                    | DocxOp::ReplyToComment { .. }
                    | DocxOp::InsertNote { .. }
            )
    }
    fn fork(&self, _: &DocxModel) -> Result<Self, String> {
        Ok(self.clone())
    }

    /// The numbers jade gave what `cause` made, for the model to record.
    fn derived(&mut self, _: &DocxModel, _cause: &DocxOp) -> Result<Vec<DocxOp>, Rejection> {
        Ok(std::mem::take(&mut self.fresh)
            .into_iter()
            .map(|(item, file_id)| DocxOp::Numbered { item, file_id })
            .collect())
    }

    fn admit(&self, model: &DocxModel, op: &DocxOp) -> Result<(), Rejection> {
        model.check_table_budget(op)?;
        if matches!(
            op,
            DocxOp::Format { .. }
                | DocxOp::SetTracking { .. }
                | DocxOp::AddComment { .. }
                | DocxOp::ReplyToComment { .. }
                | DocxOp::ResolveComment { .. }
                | DocxOp::DeleteComment { .. }
                | DocxOp::UpdateComment { .. }
                | DocxOp::InsertNote { .. }
                | DocxOp::DeleteNote { .. }
                | DocxOp::InsertTextBox { .. }
                | DocxOp::DeleteTextBox { .. }
                | DocxOp::AcceptRevisions { .. }
                | DocxOp::RejectRevisions { .. }
        ) {
            return self.preflight(model, op);
        }
        if let DocxOp::JoinParagraph { story, mark } = op {
            let Some(paragraph) = model.paragraph_ending_with(*story, *mark) else {
                return Ok(()); // the model rejects it
            };
            // Between two paragraphs jade may list a text box's or a note's
            // paragraphs; it cannot join across them.
            let at = model.paragraphs()[paragraph].at.clone();
            if let (Ok(a), Ok(b)) = (self.surface(&at), self.surface(&next_paragraph(&at))) {
                if a + 1 != b {
                    return Err(Rejection::new(
                        "joining a paragraph across a text box or note is not supported",
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
            let unlisted = matches!(
                effect,
                DocxEffect::AddComment { .. } | DocxEffect::ReplyToComment { .. }
            )
            .then(|| self.unlisted());
            self.realize(model, effect)
                .map_err(|e| format!("{description}: {e}"))?;
            // jade's add_comment (and a reply, anchored the same way) splits
            // the runs it anchors to without relisting the paragraph's text
            // boxes, whose paragraphs then point at the wrong run and vanish
            // until the document is read again; putting jade's own state
            // back relists them.
            if unlisted.is_some_and(|before| self.unlisted() > before) {
                if let Some(snapshot) = self.word.snapshot_including_package() {
                    self.word.restore_snapshot(snapshot);
                }
            }
            if let Some(comment) = created {
                // The one comment jade did not have before is this one.
                let mut new = self
                    .word
                    .comments
                    .keys()
                    .filter(|key| !before.contains(*key));
                match (new.next(), new.next()) {
                    (Some(key), None) => {
                        let key = key.clone();
                        self.learn(Numbered::Comment { id: comment }, key);
                    }
                    _ => return Err(format!("{description}: jade made no single new comment")),
                }
            }
        }
        Ok(())
    }
}
