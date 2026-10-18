//! Index-addressed edits, resolved against a view into identifier-addressed
//! [`PptxOp`]s.
//!
//! Addressing follows jade's headless pptx tools: slides by their number
//! from 1 (as `jade-pptx-headless-cli` prints them), shapes by their
//! `cNvPr id` (the model's [`crate::ShapeView::number`]), speaker notes as
//! their own target. Within a text body, paragraphs are indices and offsets
//! UTF-8 bytes on character boundaries, as in the docx intent API; `\n` in
//! inserted text splits paragraphs. Resolving pins the caret to the
//! character before it, so the edit keeps its meaning however long it takes
//! to reach the server.

use jade_collab_core::id::Id;
use serde::{Deserialize, Serialize};

use crate::model::{
    Alignment, CharFormat, Geometry, GeometryEdit, Image, Paragraph, PptxModel, PptxOp, StoryId,
};

fn one() -> usize {
    1
}

/// Which text body of a slide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// The shape with this id.
    Shape(u32),
    /// The slide's speaker notes.
    Notes,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Forward,
    Backward,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PptxIntent {
    InsertText {
        slide: usize,
        target: Target,
        paragraph: usize,
        offset: usize,
        text: String,
    },
    ReplaceRange {
        slide: usize,
        target: Target,
        start_paragraph: usize,
        start_offset: usize,
        end_paragraph: usize,
        end_offset: usize,
        text: String,
    },
    /// Deletes `count` characters (Unicode scalar values); crossing a
    /// paragraph end joins the paragraphs.
    DeleteText {
        slide: usize,
        target: Target,
        paragraph: usize,
        offset: usize,
        #[serde(default)]
        direction: Direction,
        #[serde(default = "one")]
        count: usize,
    },
    InsertParagraphBreak {
        slide: usize,
        target: Target,
        paragraph: usize,
        offset: usize,
        #[serde(default = "one")]
        count: usize,
    },
    /// EMU; rotation in 60000ths of a degree. Omitted attributes stay. A
    /// shape whose aspect ratio is locked (a picture) keeps it: a width or
    /// height alone scales the other side, and the size travels as a
    /// sequenced resize.
    SetGeometry {
        slide: usize,
        shape: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation: Option<i64>,
    },
    /// A new slide that becomes number `at` (1 to the slide count + 1).
    InsertSlide {
        at: usize,
    },
    DeleteSlide {
        slide: usize,
    },
    /// Moves a slide so that it becomes number `to`.
    MoveSlide {
        slide: usize,
        to: usize,
    },
    /// The copy follows the slide.
    DuplicateSlide {
        slide: usize,
    },
    InsertTextBox {
        slide: usize,
        x: i64,
        y: i64,
        width: i64,
        height: i64,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
    },
    DeleteShape {
        slide: usize,
        shape: u32,
    },
    /// Gives the characters between two carets of a shape's text one
    /// attribute (a convergent mark: text typed at its end takes it too).
    FormatText {
        slide: usize,
        shape: u32,
        start_paragraph: usize,
        start_offset: usize,
        end_paragraph: usize,
        end_offset: usize,
        format: CharFormat,
    },
    /// Aligns paragraphs `start_paragraph..=end_paragraph` of a shape.
    AlignParagraphs {
        slide: usize,
        shape: u32,
        start_paragraph: usize,
        end_paragraph: usize,
        alignment: Alignment,
    },
    /// A PNG or JPEG picture (base64 on the wire), on top of the slide.
    /// Without a width and height it takes its pixel size at 96 dpi; with
    /// one of them, the other follows the image's ratio.
    InsertPicture {
        slide: usize,
        x: i64,
        y: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<i64>,
        image: Image,
    },
    /// Hides a slide from the slide show, or shows it again.
    SetSlideHidden {
        slide: usize,
        hidden: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IntentError {
    #[error("slide {0} does not exist (slides are numbered from 1)")]
    NoSuchSlide(usize),
    #[error("slide {slide} has no shape {shape}")]
    NoSuchShape { slide: usize, shape: u32 },
    #[error("shape {shape} on slide {slide} has no text body jade can edit")]
    NoText { slide: usize, shape: u32 },
    #[error("paragraph {0} does not exist")]
    NoSuchParagraph(usize),
    #[error("offset {offset} is not a character boundary of paragraph {paragraph}")]
    BadOffset { paragraph: usize, offset: usize },
    #[error("the range starts after it ends")]
    InvertedRange,
    #[error("there is no slide position {0}")]
    BadPosition(usize),
    #[error("the range holds no characters to format")]
    NothingToFormat,
    #[error("invalid format: {0}")]
    InvalidFormat(String),
    #[error("invalid picture: {0}")]
    InvalidPicture(String),
}

/// `known_other * given / known`, rounded: the side that keeps a ratio.
fn scaled(given: i64, known: i64, known_other: i64) -> i64 {
    if known == 0 {
        return known_other;
    }
    let (n, d) = (
        i128::from(given) * i128::from(known_other),
        i128::from(known),
    );
    let rounded = (2 * n + d.signum() * d) / (2 * d);
    i64::try_from(rounded).unwrap_or(i64::MAX)
}

/// EMU per pixel at 96 dpi.
const EMU_PER_PIXEL: i64 = 9525;

fn slide_id(view: &PptxModel, number: usize) -> Result<Id, IntentError> {
    number
        .checked_sub(1)
        .and_then(|i| view.slides().get(i).map(|s| s.id))
        .ok_or(IntentError::NoSuchSlide(number))
}

fn shape_id(view: &PptxModel, slide: usize, shape: u32) -> Result<(Id, Id), IntentError> {
    let id = slide_id(view, slide)?;
    let key = view
        .shape_by_number(slide - 1, shape)
        .ok_or(IntentError::NoSuchShape { slide, shape })?;
    Ok((id, key))
}

fn story(view: &PptxModel, slide: usize, target: Target) -> Result<StoryId, IntentError> {
    match target {
        Target::Notes => Ok(StoryId::Notes {
            slide: slide_id(view, slide)?,
        }),
        Target::Shape(shape) => {
            let (slide_id, key) = shape_id(view, slide, shape)?;
            let story = StoryId::Shape {
                slide: slide_id,
                shape: key,
            };
            view.paragraphs(story)
                .map(|_| story)
                .ok_or(IntentError::NoText { slide, shape })
        }
    }
}

/// A caret: the atom before it (`None`: story start) and its index among
/// the story's visible atoms.
struct Caret {
    parent: Option<Id>,
    index: usize,
}

/// Visible atoms in order: each paragraph's characters, then its mark.
/// `Ok(id)` is a character, `Err(id)` a mark.
fn atoms(paragraphs: &[Paragraph]) -> Vec<Result<Id, Id>> {
    paragraphs
        .iter()
        .flat_map(|p| p.chars.iter().map(|c| Ok(*c)).chain(p.mark.map(Err)))
        .collect()
}

fn caret(paragraphs: &[Paragraph], paragraph: usize, offset: usize) -> Result<Caret, IntentError> {
    let p = paragraphs
        .get(paragraph)
        .ok_or(IntentError::NoSuchParagraph(paragraph))?;
    if offset > p.text.len() || !p.text.is_char_boundary(offset) {
        return Err(IntentError::BadOffset { paragraph, offset });
    }
    let chars_before = p.text[..offset].chars().count();
    let preceding = &paragraphs[..paragraph];
    let index = preceding
        .iter()
        .map(|q| q.chars.len() + usize::from(q.mark.is_some()))
        .sum::<usize>()
        + chars_before;
    let parent = if chars_before > 0 {
        Some(p.chars[chars_before - 1])
    } else {
        preceding.last().and_then(|q| q.mark)
    };
    Ok(Caret { parent, index })
}

/// Deleting `atoms`: one text deletion, then a join per mark.
fn deletion(story: StoryId, atoms: &[Result<Id, Id>]) -> Vec<PptxOp> {
    let ids: Vec<Id> = atoms.iter().filter_map(|a| a.ok()).collect();
    let mut ops = Vec::new();
    if !ids.is_empty() {
        ops.push(PptxOp::DeleteText { story, ids });
    }
    ops.extend(
        atoms
            .iter()
            .filter_map(|a| a.err())
            .map(|mark| PptxOp::JoinParagraph { story, mark }),
    );
    ops
}

/// Typing `text` after `parent`: runs of characters, a split per `\n`.
fn typing(
    story: StoryId,
    parent: Option<Id>,
    text: &str,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Vec<PptxOp> {
    let mut ops = Vec::new();
    let mut parent = parent;
    for (k, run) in text.split('\n').enumerate() {
        if k > 0 {
            let id = ids(1);
            ops.push(PptxOp::SplitParagraph { story, parent, id });
            parent = Some(id);
        }
        if !run.is_empty() {
            let count = run.chars().count() as u64;
            let first = ids(count);
            ops.push(PptxOp::InsertText {
                story,
                parent,
                first,
                text: run.to_string(),
            });
            parent = Some(first.offset(count - 1));
        }
    }
    ops
}

/// Resolves `intent` against `view`. `ids(n)` must reserve `n` consecutive
/// fresh identifiers (see `Replica::reserve`). The operations are meant to
/// be submitted in order.
pub fn resolve(
    view: &PptxModel,
    intent: &PptxIntent,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Result<Vec<PptxOp>, IntentError> {
    match intent {
        PptxIntent::InsertText {
            slide,
            target,
            paragraph,
            offset,
            text,
        } => {
            let story = story(view, *slide, *target)?;
            let paragraphs = view.paragraphs(story).expect("resolved");
            let at = caret(&paragraphs, *paragraph, *offset)?;
            Ok(typing(story, at.parent, text, ids))
        }
        PptxIntent::InsertParagraphBreak {
            slide,
            target,
            paragraph,
            offset,
            count,
        } => {
            let story = story(view, *slide, *target)?;
            let paragraphs = view.paragraphs(story).expect("resolved");
            let at = caret(&paragraphs, *paragraph, *offset)?;
            Ok(typing(story, at.parent, &"\n".repeat(*count), ids))
        }
        PptxIntent::DeleteText {
            slide,
            target,
            paragraph,
            offset,
            direction,
            count,
        } => {
            let story = story(view, *slide, *target)?;
            let paragraphs = view.paragraphs(story).expect("resolved");
            let at = caret(&paragraphs, *paragraph, *offset)?;
            let atoms = atoms(&paragraphs);
            let range = match direction {
                Direction::Forward => at.index..(at.index + count).min(atoms.len()),
                Direction::Backward => at.index.saturating_sub(*count)..at.index,
            };
            Ok(deletion(story, &atoms[range]))
        }
        PptxIntent::ReplaceRange {
            slide,
            target,
            start_paragraph,
            start_offset,
            end_paragraph,
            end_offset,
            text,
        } => {
            let story = story(view, *slide, *target)?;
            let paragraphs = view.paragraphs(story).expect("resolved");
            let start = caret(&paragraphs, *start_paragraph, *start_offset)?;
            let end = caret(&paragraphs, *end_paragraph, *end_offset)?;
            if start.index > end.index {
                return Err(IntentError::InvertedRange);
            }
            let mut ops = deletion(story, &atoms(&paragraphs)[start.index..end.index]);
            ops.extend(typing(story, start.parent, text, ids));
            Ok(ops)
        }
        PptxIntent::SetGeometry {
            slide,
            shape,
            x,
            y,
            width,
            height,
            rotation,
        } => {
            let number = *slide;
            let (slide, shape) = shape_id(view, *slide, *shape)?;
            let view_shape = view.slides()[number - 1]
                .shapes
                .iter()
                .find(|s| s.id == shape)
                .cloned()
                .expect("resolved");
            let locked = view_shape.aspect_locked;
            let mut ops = Vec::new();
            let edit = GeometryEdit {
                x: *x,
                y: *y,
                width: if locked { None } else { *width },
                height: if locked { None } else { *height },
                rotation: *rotation,
            };
            if edit != GeometryEdit::default() || !locked {
                ops.push(PptxOp::SetGeometry {
                    slide,
                    shape,
                    stamp: ids(1),
                    edit,
                });
            }
            if let (true, Some(current)) = (locked, view_shape.geometry) {
                let size = match (*width, *height) {
                    (Some(w), Some(h)) => Some((w, h)),
                    (Some(w), None) => Some((w, scaled(w, current.width, current.height))),
                    (None, Some(h)) => Some((scaled(h, current.height, current.width), h)),
                    (None, None) => None,
                };
                if let Some((width, height)) = size {
                    ops.push(PptxOp::ResizeShape {
                        slide,
                        shape,
                        stamp: ids(1),
                        width,
                        height,
                    });
                }
            }
            Ok(ops)
        }
        PptxIntent::InsertSlide { at } => {
            let slides = view.slides();
            if *at == 0 || *at > slides.len() + 1 {
                return Err(IntentError::BadPosition(*at));
            }
            Ok(vec![PptxOp::InsertSlide {
                id: ids(1),
                after: (*at > 1).then(|| slides[at - 2].id),
            }])
        }
        PptxIntent::DeleteSlide { slide } => Ok(vec![PptxOp::DeleteSlide {
            slide: slide_id(view, *slide)?,
        }]),
        PptxIntent::MoveSlide { slide, to } => {
            let id = slide_id(view, *slide)?;
            let rest: Vec<Id> = view
                .slides()
                .iter()
                .map(|s| s.id)
                .filter(|s| *s != id)
                .collect();
            if *to == 0 || *to > rest.len() + 1 {
                return Err(IntentError::BadPosition(*to));
            }
            Ok(vec![PptxOp::MoveSlide {
                slide: id,
                after: (*to > 1).then(|| rest[to - 2]),
            }])
        }
        PptxIntent::DuplicateSlide { slide } => Ok(vec![PptxOp::DuplicateSlide {
            slide: slide_id(view, *slide)?,
            id: ids(1),
        }]),
        PptxIntent::InsertTextBox {
            slide,
            x,
            y,
            width,
            height,
            text,
        } => {
            let slide = slide_id(view, *slide)?;
            let id = ids(1);
            let mut ops = vec![PptxOp::InsertTextBox {
                slide,
                id,
                geometry: Geometry {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    rotation: 0,
                },
            }];
            ops.extend(typing(StoryId::Shape { slide, shape: id }, None, text, ids));
            Ok(ops)
        }
        PptxIntent::DeleteShape { slide, shape } => {
            let (slide, shape) = shape_id(view, *slide, *shape)?;
            Ok(vec![PptxOp::DeleteShape { slide, shape }])
        }
        PptxIntent::FormatText {
            slide,
            shape,
            start_paragraph,
            start_offset,
            end_paragraph,
            end_offset,
            format,
        } => {
            format.validate().map_err(IntentError::InvalidFormat)?;
            let story = story(view, *slide, Target::Shape(*shape))?;
            let paragraphs = view.paragraphs(story).expect("resolved");
            let start = caret(&paragraphs, *start_paragraph, *start_offset)?;
            let end = caret(&paragraphs, *end_paragraph, *end_offset)?;
            if start.index > end.index {
                return Err(IntentError::InvertedRange);
            }
            let chars: Vec<Id> = atoms(&paragraphs)[start.index..end.index]
                .iter()
                .filter_map(|a| a.ok())
                .collect();
            let (Some(first), Some(last)) = (chars.first(), chars.last()) else {
                return Err(IntentError::NothingToFormat);
            };
            let (start, end) = view
                .mark_anchors(story, *first, *last, format.key().expand())
                .expect("a view's characters are in their story, in order");
            Ok(vec![PptxOp::Mark {
                story,
                id: ids(1),
                start,
                end,
                format: format.clone(),
            }])
        }
        PptxIntent::AlignParagraphs {
            slide,
            shape,
            start_paragraph,
            end_paragraph,
            alignment,
        } => {
            let story = story(view, *slide, Target::Shape(*shape))?;
            let paragraphs = view.paragraphs(story).expect("resolved");
            for n in [*start_paragraph, *end_paragraph] {
                if n >= paragraphs.len() {
                    return Err(IntentError::NoSuchParagraph(n));
                }
            }
            if start_paragraph > end_paragraph {
                return Err(IntentError::InvertedRange);
            }
            // A paragraph is named by the mark that ends the one before it.
            let after = |n: usize| n.checked_sub(1).and_then(|k| paragraphs[k].mark);
            Ok(vec![PptxOp::AlignParagraphs {
                story,
                from: after(*start_paragraph),
                to: after(*end_paragraph),
                alignment: *alignment,
            }])
        }
        PptxIntent::InsertPicture {
            slide,
            x,
            y,
            width,
            height,
            image,
        } => {
            let slide = slide_id(view, *slide)?;
            let info = jade_pptx_writer::picture_info(&image.0)
                .map_err(|e| IntentError::InvalidPicture(e.to_string()))?;
            let (pw, ph) = (
                i64::from(info.width) * EMU_PER_PIXEL,
                i64::from(info.height) * EMU_PER_PIXEL,
            );
            let (width, height) = match (*width, *height) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, scaled(w, pw, ph)),
                (None, Some(h)) => (scaled(h, ph, pw), h),
                (None, None) => (pw, ph),
            };
            Ok(vec![PptxOp::InsertPicture {
                slide,
                id: ids(1),
                geometry: Geometry {
                    x: *x,
                    y: *y,
                    width,
                    height,
                    rotation: 0,
                },
                image: image.clone(),
            }])
        }
        PptxIntent::SetSlideHidden { slide, hidden } => Ok(vec![PptxOp::SetSlideHidden {
            slide: slide_id(view, *slide)?,
            stamp: ids(1),
            hidden: *hidden,
        }]),
    }
}
