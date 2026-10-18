#![allow(dead_code)]

use std::path::PathBuf;

use jade_collab_core::id::Id;
use jade_pptx_collab::intent::{resolve, PptxIntent, Target};
use jade_pptx_collab::{
    GeometryEdit, Geometry, JadePptx, PptxModel, PptxOp, ShapeRef, SlideView, StoryId,
};

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/public/fixtures")
}

pub fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Opens a fixture in jade and builds the model from the same bytes.
pub fn open(name: &str) -> (PptxModel, JadePptx) {
    let jade = JadePptx::open(&fixture(name)).expect("jade opens the fixture");
    let model = PptxModel::from_pptx(jade.bytes()).expect("model builds");
    (model, jade)
}

pub fn shape_ref(slide: &SlideView, index: usize) -> ShapeRef {
    let shape = &slide.shapes[index];
    ShapeRef {
        slide: slide.id,
        shape: shape.id,
        number: shape.number,
    }
}

/// The model and the deck in `file` describe the same presentation: the
/// same slides in the same order, the same shapes (by file id, through
/// `ids`' mapping of shapes made in the session) in the same order, the same
/// text in every text body jade can edit, the same speaker notes and the
/// same geometry. `ids` is the materializer that made the file (it knows
/// which file id each new text box got); `file` is the deck to inspect —
/// the same materializer, or the deck reopened from its export.
pub fn assert_agree_with(model: &PptxModel, ids: &JadePptx, file: &JadePptx, context: &str) {
    let slides = model.slides();
    assert_eq!(slides.len(), file.slide_count(), "slide count {context}");
    for (index, slide) in slides.iter().enumerate() {
        let expected: Vec<u32> = (0..slide.shapes.len())
            .map(|k| ids.file_id(shape_ref(slide, k)))
            .collect();
        assert_eq!(
            expected,
            file.shape_ids(index),
            "shapes of slide {index} {context}"
        );
        for (k, shape) in slide.shapes.iter().enumerate() {
            let id = expected[k];
            assert_eq!(
                shape.text,
                file.shape_text(index, id),
                "text of shape {id} on slide {index} {context}"
            );
            if let Some(geometry) = shape.geometry {
                assert_eq!(
                    Some(geometry),
                    file.geometry(index, id),
                    "geometry of shape {id} on slide {index} {context}"
                );
            }
        }
        assert_eq!(
            slide.notes,
            file.notes_text(index),
            "notes of slide {index} {context}"
        );
    }
}

pub fn assert_agree(model: &PptxModel, jade: &JadePptx, context: &str) {
    assert_agree_with(model, jade, jade, context);
}

/// Every text story of a view: shape text bodies and notes, with the intent
/// target that names it.
pub fn stories(view: &PptxModel) -> Vec<(usize, Target, StoryId)> {
    let mut out = Vec::new();
    for (index, slide) in view.slides().iter().enumerate() {
        for shape in &slide.shapes {
            if shape.text.is_some() {
                out.push((
                    index + 1,
                    Target::Shape(shape.number),
                    StoryId::Shape {
                        slide: slide.id,
                        shape: shape.id,
                    },
                ));
            }
        }
        out.push((index + 1, Target::Notes, StoryId::Notes { slide: slide.id }));
    }
    out
}

fn boundaries(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect()
}

/// Turns a random `choice` into an operation on `view`. `ids(n)` reserves
/// `n` consecutive identifiers. Returns `None` when the choice does not fit
/// the document (e.g. no shape to delete).
pub fn generate(view: &PptxModel, choice: u64, ids: &mut dyn FnMut(u64) -> Id) -> Option<PptxOp> {
    let pick = |n: usize, salt: u32| ((choice >> salt) as usize) % n.max(1);
    let slides = view.slides();
    let slide_index = pick(slides.len(), 8);
    let slide = &slides[slide_index];
    let stories = stories(view);
    let words = ["x", "日本", "é", "Lorem ", "😀", "a\tb", "ab"];
    match choice % 17 {
        // Typing at a random caret, through the index-addressed intent API.
        0..=4 => {
            let (number, target, story) = stories[pick(stories.len(), 8)];
            let paragraphs = view.paragraphs(story)?;
            let p = pick(paragraphs.len(), 14);
            let bounds = boundaries(&paragraphs[p].text);
            let intent = PptxIntent::InsertText {
                slide: number,
                target,
                paragraph: p,
                offset: bounds[pick(bounds.len(), 20)],
                text: words[pick(words.len(), 28)].to_string(),
            };
            resolve(view, &intent, ids).ok()?.into_iter().next()
        }
        // Deleting a few characters, possibly in separate ranges.
        5..=6 => {
            let (_, _, story) = stories[pick(stories.len(), 8)];
            let paragraphs = view.paragraphs(story)?;
            let chars = &paragraphs[pick(paragraphs.len(), 14)].chars;
            if chars.is_empty() {
                return None;
            }
            let start = pick(chars.len(), 20);
            let stride = 1 + pick(3, 26);
            let ids = chars[start..]
                .iter()
                .step_by(stride)
                .take(1 + pick(4, 30))
                .copied()
                .collect();
            Some(PptxOp::DeleteText { story, ids })
        }
        7 => {
            let (number, target, story) = stories[pick(stories.len(), 8)];
            let paragraphs = view.paragraphs(story)?;
            let p = pick(paragraphs.len(), 14);
            let bounds = boundaries(&paragraphs[p].text);
            let intent = PptxIntent::InsertParagraphBreak {
                slide: number,
                target,
                paragraph: p,
                offset: bounds[pick(bounds.len(), 20)],
                count: 1,
            };
            resolve(view, &intent, ids).ok()?.into_iter().next()
        }
        8 => {
            let (_, _, story) = stories[pick(stories.len(), 8)];
            let marks: Vec<Id> = view
                .paragraphs(story)?
                .iter()
                .filter_map(|p| p.mark)
                .collect();
            if marks.is_empty() {
                return None;
            }
            Some(PptxOp::JoinParagraph {
                story,
                mark: marks[pick(marks.len(), 14)],
            })
        }
        9..=10 => {
            let movable: Vec<_> = slide
                .shapes
                .iter()
                .filter(|s| s.geometry.is_some())
                .collect();
            if movable.is_empty() {
                return None;
            }
            let shape = movable[pick(movable.len(), 14)];
            let value = |salt: u32, limit: i64| Some(((choice >> salt) as i64).rem_euclid(limit));
            let mask = choice >> 50;
            let edit = GeometryEdit {
                x: (mask & 1 != 0).then(|| value(20, 12_000_000)).flatten(),
                y: (mask & 2 != 0).then(|| value(24, 6_000_000)).flatten(),
                width: (mask & 4 != 0)
                    .then(|| value(28, 6_000_000).map(|w| w + 1))
                    .flatten(),
                height: (mask & 8 != 0)
                    .then(|| value(32, 4_000_000).map(|h| h + 1))
                    .flatten(),
                rotation: (mask & 16 != 0).then(|| value(36, 21_600_000)).flatten(),
            };
            Some(PptxOp::SetGeometry {
                slide: slide.id,
                shape: shape.id,
                stamp: ids(1),
                edit,
            })
        }
        11 => Some(PptxOp::InsertSlide {
            id: ids(1),
            after: (pick(4, 14) != 0).then_some(slide.id),
        }),
        12 => Some(PptxOp::DeleteSlide { slide: slide.id }),
        13 => {
            let target = &slides[pick(slides.len(), 14)];
            Some(PptxOp::MoveSlide {
                slide: slide.id,
                after: (pick(4, 20) != 0 && target.id != slide.id).then_some(target.id),
            })
        }
        14 => Some(PptxOp::DuplicateSlide {
            slide: slide.id,
            id: ids(1),
        }),
        15 => Some(PptxOp::InsertTextBox {
            slide: slide.id,
            id: ids(1),
            geometry: Geometry {
                x: (pick(100, 14) as i64) * 50_000,
                y: (pick(100, 22) as i64) * 30_000,
                width: 1 + (pick(100, 30) as i64) * 20_000,
                height: 1 + (pick(100, 38) as i64) * 10_000,
                rotation: 0,
            },
        }),
        _ => {
            let deletable: Vec<_> = slide.shapes.iter().filter(|s| s.deletable).collect();
            if deletable.is_empty() {
                return None;
            }
            Some(PptxOp::DeleteShape {
                slide: slide.id,
                shape: deletable[pick(deletable.len(), 14)].id,
            })
        }
    }
}
