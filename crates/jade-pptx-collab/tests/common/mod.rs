#![allow(dead_code)]

use std::path::PathBuf;

use jade_collab_core::id::Id;
use jade_pptx_collab::intent::{resolve, PptxIntent, Target};
use jade_pptx_collab::{
    Alignment, CharFormat, Geometry, GeometryEdit, Image, JadePptx, PptxModel, PptxOp, ShapeRef,
    SlideView, StoryId,
};

/// A 1×1 PNG.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

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
        assert_eq!(
            slide.hidden,
            file.hidden(index),
            "visibility of slide {index} {context}"
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
    match choice % 24 {
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
        17 => Some(PptxOp::SetSlideHidden {
            slide: slide.id,
            stamp: ids(1),
            hidden: pick(2, 14) == 0,
        }),
        16 => {
            let deletable: Vec<_> = slide.shapes.iter().filter(|s| s.deletable).collect();
            if deletable.is_empty() {
                return None;
            }
            Some(PptxOp::DeleteShape {
                slide: slide.id,
                shape: deletable[pick(deletable.len(), 14)].id,
            })
        }
        // Character formatting over a random range of a shape's text.
        18..=19 => {
            let shapes: Vec<_> = stories
                .into_iter()
                .filter_map(|(number, target, story)| match target {
                    Target::Shape(shape) => Some((number, shape, story)),
                    Target::Notes => None,
                })
                .collect();
            if shapes.is_empty() {
                return None;
            }
            let (number, shape, story) = shapes[pick(shapes.len(), 8)];
            let paragraphs = view.paragraphs(story)?;
            let (a, b) = (pick(paragraphs.len(), 14), pick(paragraphs.len(), 18));
            let (first, last) = (a.min(b), a.max(b));
            let start = boundaries(&paragraphs[first].text);
            let end = boundaries(&paragraphs[last].text);
            let (mut start, mut end) = (start[pick(start.len(), 22)], end[pick(end.len(), 26)]);
            if first == last && start > end {
                std::mem::swap(&mut start, &mut end);
            }
            let formats = [
                CharFormat::Bold { value: true },
                CharFormat::Bold { value: false },
                CharFormat::Italic { value: true },
                CharFormat::Underline { value: true },
                CharFormat::Size {
                    hundredths: 800 + 100 * pick(40, 30) as u32,
                },
                CharFormat::Font {
                    typeface: ["Arial", "Georgia"][pick(2, 30)].into(),
                },
                CharFormat::EastAsianFont {
                    typeface: "Yu Gothic".into(),
                },
                CharFormat::Color {
                    color: ["#FF0000", "#0070c0"][pick(2, 30)].into(),
                },
            ];
            let intent = PptxIntent::FormatText {
                slide: number,
                shape,
                start_paragraph: first,
                start_offset: start,
                end_paragraph: last,
                end_offset: end,
                format: formats[pick(formats.len(), 34)].clone(),
            };
            resolve(view, &intent, ids).ok()?.into_iter().next()
        }
        // A picture, sized from its pixels or given a frame.
        21 => Some(PptxOp::InsertPicture {
            slide: slide.id,
            id: ids(1),
            geometry: Geometry {
                x: (pick(100, 14) as i64) * 50_000,
                y: (pick(100, 22) as i64) * 30_000,
                width: 1 + (pick(100, 30) as i64) * 20_000,
                height: 1 + (pick(100, 38) as i64) * 10_000,
                rotation: 0,
            },
            image: Image(PNG.to_vec()),
        }),
        // Resizing by width, as a person drags a corner: a shape whose
        // ratio is locked keeps it (a sequenced resize), others stretch.
        22 => {
            let placed: Vec<_> = slide
                .shapes
                .iter()
                .filter(|s| s.geometry.is_some())
                .collect();
            if placed.is_empty() {
                return None;
            }
            let shape = placed[pick(placed.len(), 14)];
            let width = shape.geometry?.width;
            let intent = PptxIntent::SetGeometry {
                slide: slide_index + 1,
                shape: shape.number,
                x: None,
                y: None,
                width: Some((width * (1 + pick(8, 20) as i64) / 4).max(1)),
                height: None,
                rotation: None,
            };
            resolve(view, &intent, ids).ok()?.into_iter().next_back()
        }
        // Aligning a few paragraphs of a shape.
        _ => {
            let shapes: Vec<_> = stories
                .into_iter()
                .filter_map(|(number, target, story)| match target {
                    Target::Shape(shape) => Some((number, shape, story)),
                    Target::Notes => None,
                })
                .collect();
            if shapes.is_empty() {
                return None;
            }
            let (number, shape, story) = shapes[pick(shapes.len(), 8)];
            let count = view.paragraphs(story)?.len();
            let (a, b) = (pick(count, 14), pick(count, 18));
            let alignments = [
                Alignment::Left,
                Alignment::Center,
                Alignment::Right,
                Alignment::Justify,
                Alignment::Distributed,
            ];
            let intent = PptxIntent::AlignParagraphs {
                slide: number,
                shape,
                start_paragraph: a.min(b),
                end_paragraph: a.max(b),
                alignment: alignments[pick(alignments.len(), 22)],
            };
            resolve(view, &intent, ids).ok()?.into_iter().next()
        }
    }
}

/// The character attributes a mark can set, as jade's reader reports a
/// run's (stated values: an attribute the run does not state is `None`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunLook {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub size: Option<f32>,
    pub color: Option<String>,
    pub font: Option<String>,
    pub east_asian_font: Option<String>,
}

impl RunLook {
    fn of(run: &jade_pptx_reader::TextRun) -> Self {
        Self {
            bold: run.bold_stated,
            italic: run.italic_stated,
            underline: run.underline_stated,
            size: run.size_pt,
            color: run.color.as_ref().map(|c| format!("{c:?}")),
            font: run.font.clone(),
            east_asian_font: run.east_asian_font.clone(),
        }
    }

    /// Whether this attribute shows `format`'s value.
    fn shows(&self, format: &CharFormat) -> bool {
        match format {
            CharFormat::Bold { value } => self.bold == Some(*value),
            CharFormat::Italic { value } => self.italic == Some(*value),
            CharFormat::Underline { value } => self.underline == Some(*value),
            CharFormat::Size { hundredths } => self.size == Some(*hundredths as f32 / 100.0),
            CharFormat::Font { typeface } => self.font.as_deref() == Some(typeface),
            CharFormat::EastAsianFont { typeface } => {
                self.east_asian_font.as_deref() == Some(typeface)
            }
            CharFormat::Color { color } => {
                self.color
                    == Some(format!(
                        "{:?}",
                        jade_pptx_reader::Color::Srgb(color[1..].to_ascii_uppercase())
                    ))
            }
        }
    }

    /// The same attribute as `format` sets, read from `self`.
    fn attribute(&self, format: &CharFormat) -> String {
        match format {
            CharFormat::Bold { .. } => format!("{:?}", self.bold),
            CharFormat::Italic { .. } => format!("{:?}", self.italic),
            CharFormat::Underline { .. } => format!("{:?}", self.underline),
            CharFormat::Size { .. } => format!("{:?}", self.size),
            CharFormat::Font { .. } => format!("{:?}", self.font),
            CharFormat::EastAsianFont { .. } => format!("{:?}", self.east_asian_font),
            CharFormat::Color { .. } => format!("{:?}", self.color),
        }
    }
}

/// Every attribute a mark can set, as one value each.
pub fn every_attribute() -> [CharFormat; 7] {
    [
        CharFormat::Bold { value: true },
        CharFormat::Italic { value: true },
        CharFormat::Underline { value: true },
        CharFormat::Size { hundredths: 100 },
        CharFormat::Font {
            typeface: String::new(),
        },
        CharFormat::EastAsianFont {
            typeface: String::new(),
        },
        CharFormat::Color {
            color: "#000000".into(),
        },
    ]
}

fn find_text<'a>(
    items: &'a [jade_pptx_reader::ShapeItem],
    id: u32,
) -> Option<&'a jade_pptx_reader::TextBody> {
    items.iter().find_map(|item| match item {
        jade_pptx_reader::ShapeItem::Shape(s) if s.id == Some(id) => s.text.as_ref(),
        jade_pptx_reader::ShapeItem::Group(g) => find_text(&g.children, id),
        _ => None,
    })
}

/// Each character of shape `id`'s text on slide `index`, paragraphs
/// separated by `\n`, with how its run looks.
pub fn looks(deck: &jade_pptx_reader::Deck, index: usize, id: u32) -> Vec<(char, RunLook)> {
    let Some(body) = find_text(&deck.slides[index].content.shapes, id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (k, paragraph) in body.paragraphs.iter().enumerate() {
        if k > 0 {
            out.push(('\n', RunLook::default()));
        }
        for run in &paragraph.runs {
            let look = RunLook::of(run);
            out.extend(run.text.chars().map(|c| (c, look.clone())));
        }
    }
    out
}

/// The file jade writes for the model shows the marks: every character a
/// mark of an attribute covers has that mark's value, and every attribute
/// no mark sets is as in jade's base file (the server's document, which
/// never sees a mark).
pub fn assert_formatted(model: &PptxModel, jade: &JadePptx, context: &str) {
    let base = jade_pptx_reader::read_deck(jade.bytes()).unwrap();
    let written = jade
        .export(model)
        .unwrap_or_else(|e| panic!("{context}: {e}"));
    let formatted = jade_pptx_reader::read_deck(&written).unwrap();
    for (index, slide) in model.slides().iter().enumerate() {
        for shape in &slide.shapes {
            let Some(text) = &shape.text else { continue };
            if !shape.editable {
                continue;
            }
            let story = StoryId::Shape {
                slide: slide.id,
                shape: shape.id,
            };
            let id = jade.file_id(ShapeRef {
                slide: slide.id,
                shape: shape.id,
                number: shape.number,
            });
            let before = looks(&base, index, id);
            let after = looks(&formatted, index, id);
            let shown: String = after.iter().map(|(c, _)| *c).collect();
            assert_eq!(
                &shown, text,
                "text of shape {id} on slide {index} {context}"
            );
            assert_eq!(before.len(), after.len(), "{context}");
            let marked = model.char_formats(story).unwrap();
            let chars = model
                .paragraphs(story)
                .unwrap()
                .into_iter()
                .enumerate()
                .flat_map(|(k, p)| {
                    (k > 0)
                        .then_some(None)
                        .into_iter()
                        .chain(p.chars.into_iter().map(Some))
                })
                .collect::<Vec<_>>();
            for (k, c) in chars.into_iter().enumerate() {
                let Some(c) = c else { continue };
                let formats = marked.get(&c).cloned().unwrap_or_default();
                for attribute in every_attribute() {
                    let mark = formats
                        .iter()
                        .find(|f| std::mem::discriminant(*f) == std::mem::discriminant(&attribute));
                    match mark {
                        Some(format) => assert!(
                            after[k].1.shows(format),
                            "character {k} of shape {id} on slide {index} should show \
                             {format:?} but looks {:?} {context}",
                            after[k].1
                        ),
                        None => assert_eq!(
                            after[k].1.attribute(&attribute),
                            before[k].1.attribute(&attribute),
                            "character {k} of shape {id} on slide {index}: {attribute:?} \
                             is not jade's own {context}"
                        ),
                    }
                }
            }
        }
    }
}
