//! Realising model effects in a deck through jade's pptx writer.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Materializer, Rejection};
use jade_pptx_writer::{
    EditSession, NotesInputEdit, ParagraphFormat, ParagraphFormatEdit, ShapeTransformInfo,
    TextFormat, TextFormatEdit, TextRangeEdit,
};

use crate::load::{geometry_of, shape_tree};
use crate::model::{transform, Geometry, PptxEffect, PptxModel, PptxOp, ShapeRef, TextTarget};

/// The server's deck. Every write patches the original package, so parts
/// the model does not describe (layouts, media, animations, tables, text
/// formatting) survive untouched.
pub struct JadePptx {
    session: EditSession,
    /// File ids jade gave the shapes created in this session, where they
    /// differ from the number the model knows them by.
    ids: BTreeMap<(Id, Id), u32>,
    /// What jade reads back from the current bytes, dropped on every write.
    cache: RefCell<Cache>,
}

#[derive(Default)]
struct Cache {
    deck: Option<Rc<jade_pptx_reader::Deck>>,
    shapes: BTreeMap<usize, Rc<Vec<ShapeTransformInfo>>>,
}

fn utf16(text: &str) -> usize {
    text.encode_utf16().count()
}

fn refused(error: impl std::fmt::Display) -> Rejection {
    Rejection::new(format!("jade refuses this: {error}"))
}

impl JadePptx {
    pub fn new(session: EditSession) -> Self {
        Self {
            session,
            ids: BTreeMap::new(),
            cache: RefCell::default(),
        }
    }

    pub fn open(bytes: &[u8]) -> Result<Self, String> {
        EditSession::open(bytes)
            .map(Self::new)
            .map_err(|e| e.to_string())
    }

    /// The materializer for `bytes`, a file this crate exported, described by
    /// `model` (restored as it was, not derived from the file). jade may have
    /// given a text box made in the session another file id than the number
    /// the model knows it by; the two list each slide's shapes in the same
    /// order, so the ids are matched up by position.
    pub fn restore(bytes: &[u8], model: &PptxModel) -> Result<Self, String> {
        let mut jade = Self::open(bytes)?;
        let slides = model.slides();
        if slides.len() != jade.slide_count() {
            return Err(format!(
                "the file has {} slides, the model {}",
                jade.slide_count(),
                slides.len()
            ));
        }
        for (index, slide) in slides.iter().enumerate() {
            let ids = jade.shape_ids(index);
            if ids.len() != slide.shapes.len() {
                return Err(format!(
                    "slide {} has {} shapes in the file but {} in the model",
                    index + 1,
                    ids.len(),
                    slide.shapes.len()
                ));
            }
            for (shape, id) in slide.shapes.iter().zip(ids) {
                if shape.number != id {
                    jade.ids.insert((slide.id, shape.id), id);
                }
            }
        }
        Ok(jade)
    }

    /// The current native writer cache. Its Undo history may be rebased by
    /// equivalent notes compaction; collaboration history lives in the log.
    pub fn session(&self) -> &EditSession {
        &self.session
    }

    /// Stage a lossless run compaction. The collaboration log is authoritative;
    /// native writer history is a cache and can be rebased at this boundary.
    fn compact_notes_session(&self, slide: usize) -> Option<EditSession> {
        use jade_opc::{parse_relationships, rels_part_name, resolve_target, OpcPackage};
        let deck = self.deck();
        let slide_part = &deck.slides.get(slide)?.part_name;
        let mut package = OpcPackage::open_preserving(self.bytes()).ok()?;
        let relationships = parse_relationships(
            &package
                .required_part_str(&rels_part_name(slide_part))
                .ok()?,
        )
        .ok()?;
        let notes = relationships.by_type_suffix("/notesSlide");
        if notes.len() != 1 || notes[0].external {
            return None;
        }
        let part = resolve_target(slide_part, &notes[0].target);
        let xml = package.required_part_str(&part).ok()?;
        let compact = crate::notes_runs::compact(&xml)?;
        package.replace(&part, compact.into_bytes());
        EditSession::open(&package.write().ok()?).ok()
    }

    fn replace_notes(
        &mut self,
        slide: usize,
        start: usize,
        end: usize,
        text: &str,
    ) -> Result<(), String> {
        let expected_text = self.text_of(slide, &TextTarget::Notes)?;
        let edit = NotesInputEdit {
            start,
            end,
            text: text.into(),
            expected_text,
            soft_break: false,
            history: None,
        };
        // Eligibility depends only on the current archive. A volatile cadence
        // changes run boundaries after checkpoint/replay and breaks exact exports.
        if let Some(mut staged) = self.compact_notes_session(slide) {
            // Native input validates text, locking, capabilities and offsets on
            // the staged file too. Any failed optimization leaves the original
            // session untouched and uses its existing native path instead.
            if let Ok(result) = staged.apply_notes_input(slide, &edit) {
                if result.changed {
                    self.session = staged;
                }
                return Ok(());
            }
        }
        self.session
            .apply_notes_input(slide, &edit)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn bytes(&self) -> &[u8] {
        self.session.bytes()
    }

    /// The file with the model's character formatting: what users
    /// download. A copy of jade's deck gets every run of characters a mark
    /// sets (`PptxModel::formatting`) as an explicit value; everything else
    /// keeps jade's own formatting.
    pub fn export(&self, model: &PptxModel) -> Result<Vec<u8>, String> {
        let mut bytes = self.bytes().to_vec();
        for run in model.formatting() {
            let target = TextTarget::Shape(run.shape);
            // Formatting never changes the text, so positions read on the
            // base hold on every intermediate copy.
            let start = self.position(run.slide, &target, run.paragraph, run.start)?;
            let end = self.position(run.slide, &target, run.paragraph, run.end)?;
            bytes = jade_pptx_writer::format_text_range(
                &bytes,
                run.slide,
                &TextFormatEdit {
                    shape_id: self.file_id(run.shape),
                    start,
                    end,
                    format: run.format.to_jade(),
                },
            )
            .map_err(|e| format!("formatting {run:?}: {e}"))?;
        }
        Ok(bytes)
    }

    /// jade's deck without the model's marks: what a checkpoint stores next
    /// to the model, which holds the marks.
    pub fn export_base(&self) -> Vec<u8> {
        self.session.bytes().to_vec()
    }

    /// The file's id for a shape the model names.
    pub fn file_id(&self, shape: ShapeRef) -> u32 {
        self.ids
            .get(&(shape.slide, shape.shape))
            .copied()
            .unwrap_or(shape.number)
    }

    fn deck(&self) -> Rc<jade_pptx_reader::Deck> {
        let mut cache = self.cache.borrow_mut();
        cache
            .deck
            .get_or_insert_with(|| {
                Rc::new(
                    jade_pptx_reader::read_deck(self.bytes())
                        .expect("jade keeps the deck readable"),
                )
            })
            .clone()
    }

    /// jade's placement and capabilities of slide `index`'s top-level shapes.
    fn editable_shapes(&self, index: usize) -> Result<Rc<Vec<ShapeTransformInfo>>, String> {
        if let Some(shapes) = self.cache.borrow().shapes.get(&index) {
            return Ok(shapes.clone());
        }
        let shapes = Rc::new(
            jade_pptx_writer::editable_shapes(self.bytes(), index).map_err(|e| e.to_string())?,
        );
        self.cache.borrow_mut().shapes.insert(index, shapes.clone());
        Ok(shapes)
    }

    pub fn slide_count(&self) -> usize {
        self.deck().slides.len()
    }

    /// The ids of slide `index`'s shapes (0-based), depth first, in the
    /// order the model lists them.
    pub fn shape_ids(&self, index: usize) -> Vec<u32> {
        self.deck()
            .slides
            .get(index)
            .map(|s| shape_tree(&s.content.shapes).iter().map(|t| t.id).collect())
            .unwrap_or_default()
    }

    /// The text jade would edit in shape `id`, if it edits one.
    pub fn shape_text(&self, index: usize, id: u32) -> Option<String> {
        jade_pptx_writer::shape_text(self.bytes(), index, id)
            .ok()
            .map(|t| t.text)
    }

    pub fn notes_text(&self, index: usize) -> String {
        match jade_pptx_writer::notes_text(self.bytes(), index) {
            Ok(t) => t.text,
            Err(_) => self
                .deck()
                .slides
                .get(index)
                .and_then(|s| s.notes.clone())
                .unwrap_or_default(),
        }
    }

    /// Whether slide `index` is hidden from the slide show.
    pub fn hidden(&self, index: usize) -> bool {
        self.deck()
            .slides
            .get(index)
            .is_some_and(|s| s.content.hidden)
    }

    /// Where jade places top-level shape `id` (inherited placement resolved).
    pub fn geometry(&self, index: usize, id: u32) -> Option<Geometry> {
        self.editable_shapes(index)
            .ok()?
            .iter()
            .find(|i| i.id == id)
            .and_then(geometry_of)
    }

    fn text_of(&self, slide: usize, target: &TextTarget) -> Result<String, String> {
        match target {
            TextTarget::Shape(shape) => {
                jade_pptx_writer::shape_text(self.bytes(), slide, self.file_id(*shape))
                    .map(|t| t.text)
            }
            TextTarget::Notes => jade_pptx_writer::notes_text(self.bytes(), slide).map(|t| t.text),
        }
        .map_err(|e| e.to_string())
    }

    /// jade's UTF-16 offset into a text body for a model position: the
    /// paragraph's start plus the offset's UTF-8 prefix, counted on jade's
    /// own text.
    fn position(
        &self,
        slide: usize,
        target: &TextTarget,
        paragraph: usize,
        offset: usize,
    ) -> Result<usize, String> {
        let text = self.text_of(slide, target)?;
        let paragraphs: Vec<&str> = text.split('\n').collect();
        let p = paragraphs.get(paragraph).ok_or_else(|| {
            format!(
                "paragraph {paragraph} is not among jade's {}",
                paragraphs.len()
            )
        })?;
        if offset > p.len() || !p.is_char_boundary(offset) {
            return Err(format!("offset {offset} is not in jade's paragraph {p:?}"));
        }
        Ok(paragraphs[..paragraph]
            .iter()
            .map(|q| utf16(q) + 1)
            .sum::<usize>()
            + utf16(&p[..offset]))
    }

    fn replace(
        &mut self,
        slide: usize,
        target: &TextTarget,
        start: usize,
        end: usize,
        text: &str,
    ) -> Result<(), String> {
        let result = match target {
            TextTarget::Shape(shape) => self
                .session
                .replace_text_range(
                    slide,
                    &TextRangeEdit {
                        shape_id: self.file_id(*shape),
                        start,
                        end,
                        text: text.to_string(),
                        soft_break: false,
                    },
                )
                .map(drop),
            TextTarget::Notes => return self.replace_notes(slide, start, end, text),
        };
        result.map_err(|e| e.to_string())
    }

    fn realise(&mut self, effect: PptxEffect) -> Result<(), String> {
        let result = self.write(effect);
        *self.cache.get_mut() = Cache::default();
        result
    }

    fn write(&mut self, effect: PptxEffect) -> Result<(), String> {
        match effect {
            PptxEffect::InsertText {
                slide,
                target,
                paragraph,
                offset,
                text,
            } => {
                let at = self.position(slide, &target, paragraph, offset)?;
                self.replace(slide, &target, at, at, &text)
            }
            PptxEffect::DeleteRange {
                slide,
                target,
                paragraph,
                start,
                end,
            } => {
                let start = self.position(slide, &target, paragraph, start)?;
                let end = self.position(slide, &target, paragraph, end)?;
                self.replace(slide, &target, start, end, "")
            }
            PptxEffect::SplitParagraph {
                slide,
                target,
                paragraph,
                offset,
            } => {
                let at = self.position(slide, &target, paragraph, offset)?;
                self.replace(slide, &target, at, at, "\n")
            }
            PptxEffect::JoinParagraph {
                slide,
                target,
                paragraph,
                offset,
            } => {
                let at = self.position(slide, &target, paragraph, offset)?;
                self.replace(slide, &target, at, at + 1, "")
            }
            PptxEffect::SetTransform {
                slide,
                shape,
                geometry,
            } => {
                let id = self.file_id(shape);
                // Flips are not modelled; jade's stay.
                let current = self
                    .editable_shapes(slide)?
                    .iter()
                    .find(|i| i.id == id)
                    .and_then(|i| i.transform);
                let mut t = transform(geometry).ok_or("rotation is out of range")?;
                if let Some(current) = current {
                    t.flip_h = current.flip_h;
                    t.flip_v = current.flip_v;
                }
                self.session
                    .set_shape_transform(slide, id, t)
                    .map(drop)
                    .map_err(|e| e.to_string())
            }
            PptxEffect::AlignParagraph {
                slide,
                shape,
                paragraph,
                alignment,
            } => {
                // A caret in the paragraph formats exactly that paragraph.
                let at = self.position(slide, &TextTarget::Shape(shape), paragraph, 0)?;
                self.session
                    .format_paragraphs(
                        slide,
                        &ParagraphFormatEdit {
                            shape_id: self.file_id(shape),
                            start: at,
                            end: at,
                            format: ParagraphFormat {
                                alignment: Some(alignment.to_jade()),
                                ..ParagraphFormat::default()
                            },
                        },
                    )
                    .map(drop)
                    .map_err(|e| e.to_string())
            }
            PptxEffect::SetSlideHidden { slide, hidden } => self
                .session
                .set_slide_hidden(slide, hidden)
                .map(drop)
                .map_err(|e| e.to_string()),
            PptxEffect::InsertSlide { at } => self
                .session
                .insert_slide(at)
                .map(drop)
                .map_err(|e| e.to_string()),
            PptxEffect::DeleteSlide { at } => self
                .session
                .delete_slide(at)
                .map(drop)
                .map_err(|e| e.to_string()),
            PptxEffect::MoveSlide { from, to } => self
                .session
                .move_slide(from, to)
                .map(drop)
                .map_err(|e| e.to_string()),
            PptxEffect::DuplicateSlide { at, source, copy } => {
                self.session
                    .duplicate_slide(at)
                    .map_err(|e| e.to_string())?;
                // The copy's shapes keep the source's file ids.
                let learnt: Vec<_> = self
                    .ids
                    .range((source, Id::new(0, ReplicaId::ORIGIN))..)
                    .take_while(|((slide, _), _)| *slide == source)
                    .map(|((_, shape), id)| ((copy, *shape), *id))
                    .collect();
                self.ids.extend(learnt);
                Ok(())
            }
            PptxEffect::InsertTextBox {
                slide,
                shape,
                geometry,
            } => {
                let t = transform(geometry).ok_or("rotation is out of range")?;
                let id = self
                    .session
                    .insert_text_box(slide, t, "", &TextFormat::default())
                    .map_err(|e| e.to_string())?;
                if id != shape.number {
                    self.ids.insert((shape.slide, shape.shape), id);
                }
                Ok(())
            }
            PptxEffect::InsertPicture {
                slide,
                shape,
                geometry,
                image,
            } => {
                let t = transform(geometry).ok_or("rotation is out of range")?;
                let inserted = self
                    .session
                    .insert_picture(slide, t, &image.0)
                    .map_err(|e| e.to_string())?;
                if inserted.id != shape.number {
                    self.ids.insert((shape.slide, shape.shape), inserted.id);
                }
                Ok(())
            }
            PptxEffect::DeleteShape { slide, shape } => self
                .session
                .delete_shape(slide, self.file_id(shape))
                .map(drop)
                .map_err(|e| e.to_string()),
        }
    }
}

impl Materializer<PptxModel> for JadePptx {
    fn may_derive(&self, _: &PptxModel, _: &PptxOp) -> bool {
        false
    }
    fn fork(&self, model: &PptxModel) -> Result<Self, String> {
        Self::restore(self.bytes(), model)
    }

    /// Asks jade, on the current file, whether it would take a structural
    /// change whose validity depends on what the model does not track:
    /// other parts of a slide that refer to a shape, the parts a slide owns,
    /// a shape tree jade cannot extend.
    fn admit(&self, model: &PptxModel, op: &PptxOp) -> Result<(), Rejection> {
        let bytes = self.bytes();
        match op {
            PptxOp::DeleteShape { slide, shape } => {
                if let Some((index, shape)) = model.shape_ref(*slide, *shape) {
                    jade_pptx_writer::delete_shape(bytes, index, self.file_id(shape))
                        .map_err(refused)?;
                }
            }
            PptxOp::DeleteSlide { slide } => {
                if let Some(index) = model.slide_index(*slide) {
                    jade_pptx_writer::validate_slide_deletions(bytes, &[index]).map_err(refused)?;
                }
            }
            PptxOp::DuplicateSlide { slide, .. } => {
                if let Some(index) = model.slide_index(*slide) {
                    jade_pptx_writer::validate_slide_duplications(bytes, &[index])
                        .map_err(refused)?;
                }
            }
            PptxOp::InsertTextBox {
                slide, geometry, ..
            } => {
                let format = TextFormat::default();
                if let (Some(index), Some(t)) = (model.slide_index(*slide), transform(*geometry)) {
                    // Invalid geometry is the model's to refuse.
                    if jade_pptx_writer::validate_text_box(t, "", &format).is_ok() {
                        jade_pptx_writer::insert_text_box(bytes, index, t, "", &format)
                            .map_err(refused)?;
                    }
                }
            }
            PptxOp::InsertPicture {
                slide,
                geometry,
                image,
                ..
            } => {
                if let (Some(index), Some(t)) = (model.slide_index(*slide), transform(*geometry)) {
                    // Invalid geometry is the model's to refuse; decoding
                    // the image is jade's.
                    if jade_pptx_writer::validate_transform(t).is_ok()
                        && geometry.width > 0
                        && geometry.height > 0
                    {
                        jade_pptx_writer::insert_picture(bytes, index, t, &image.0)
                            .map_err(refused)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn materialize(
        &mut self,
        _: &PptxModel,
        effects: Vec<PptxEffect>,
        _: &ApplyContext,
    ) -> Result<(), String> {
        for effect in effects {
            let description = format!("{effect:?}");
            self.realise(effect)
                .map_err(|e| format!("{description}: {e}"))?;
        }
        Ok(())
    }
}
