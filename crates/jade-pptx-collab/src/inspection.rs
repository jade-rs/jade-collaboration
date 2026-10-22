//! Authoring properties resolved by jade's drawing style chain, before autofit.
//! Inspection formats a private export; the native base and replicated model
//! remain unchanged. Ranges address the model's UTF-8 text, including soft breaks.
use crate::{JadePptx, PptxModel, ShapeRef};
use jade_pptx_layout::{
    paragraph_format_for_deck_shape, text_format_for_deck_shape, ParagraphFormatState,
    ParagraphSpacingState, TextFormatState,
};
use jade_pptx_reader::{Deck, ShapeItem, TextBody};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CharacterStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub size_pt: f32,
    pub font: Option<String>,
    pub east_asian_font: Option<String>,
    pub color: String,
}

impl CharacterStyle {
    fn read(value: TextFormatState) -> Result<Self, String> {
        let missing = || "native run has mixed or unavailable character properties".to_owned();
        Ok(Self {
            bold: value.bold.ok_or_else(missing)?,
            italic: value.italic.ok_or_else(missing)?,
            underline: value.underline.ok_or_else(missing)?,
            size_pt: value.size_pt.ok_or_else(missing)?,
            font: value.font,
            east_asian_font: value.east_asian_font,
            color: value.color.ok_or_else(missing)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CharacterStyleRun {
    pub start: usize,
    pub end: usize,
    pub style: CharacterStyle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "lowercase")]
pub enum Spacing {
    Percent(f32),
    Points(f32),
}

impl From<ParagraphSpacingState> for Spacing {
    fn from(value: ParagraphSpacingState) -> Self {
        match value {
            ParagraphSpacingState::Percent(n) => Self::Percent(n),
            ParagraphSpacingState::Points(n) => Self::Points(n),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParagraphStyle {
    pub alignment: Option<String>,
    pub bullet: Option<String>,
    pub character: Option<String>,
    pub scheme: Option<String>,
    pub start_at: Option<u32>,
    pub bullet_font: Option<String>,
    pub level: Option<u8>,
    pub margin_left_pt: Option<f64>,
    pub margin_right_pt: Option<f64>,
    pub indent_pt: Option<f64>,
    pub line_spacing: Option<Spacing>,
    pub space_before: Option<Spacing>,
    pub space_after: Option<Spacing>,
}

impl From<ParagraphFormatState> for ParagraphStyle {
    fn from(value: ParagraphFormatState) -> Self {
        Self {
            alignment: value.alignment,
            bullet: value.bullet,
            character: value.character,
            scheme: value.scheme,
            start_at: value.start_at,
            bullet_font: value.bullet_font,
            level: value.level,
            margin_left_pt: value.margin_left_pt,
            margin_right_pt: value.margin_right_pt,
            indent_pt: value.indent_pt,
            line_spacing: value.line_spacing.map(Into::into),
            space_before: value.space_before.map(Into::into),
            space_after: value.space_after.map(Into::into),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParagraphFormat {
    pub style: ParagraphStyle,
    pub runs: Vec<CharacterStyleRun>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeFormatting {
    /// The public model number, including for session-created shapes.
    pub shape: u32,
    /// None for a group, picture, table, chart or shape without a text body.
    pub paragraphs: Option<Vec<ParagraphFormat>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlideFormatting {
    pub shapes: Vec<ShapeFormatting>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PptxFormatting {
    /// Model slide order, including hidden slides. Notes are not styled here.
    pub slides: Vec<SlideFormatting>,
}

impl PptxFormatting {
    /// Checks identity, paragraph counts and exact UTF-8 coverage before a
    /// client pairs untrusted inspection data with its captured model.
    pub fn validate(&self, model: &PptxModel) -> Result<(), String> {
        let bad = || "native formatting does not match its PPTX model".to_owned();
        let slides = model.slides();
        if self.slides.len() != slides.len() {
            return Err(bad());
        }
        for (slide, format) in slides.iter().zip(&self.slides) {
            if slide.shapes.len() != format.shapes.len() {
                return Err(bad());
            }
            for (shape, format) in slide.shapes.iter().zip(&format.shapes) {
                if shape.number != format.shape {
                    return Err(bad());
                }
                match (&shape.text, &format.paragraphs) {
                    (None, None) => {}
                    (Some(text), Some(paragraphs)) => {
                        if text.split('\n').count() != paragraphs.len() {
                            return Err(bad());
                        }
                        for (text, p) in text.split('\n').zip(paragraphs) {
                            if [
                                p.style.margin_left_pt,
                                p.style.margin_right_pt,
                                p.style.indent_pt,
                            ]
                            .into_iter()
                            .flatten()
                            .any(|v| !v.is_finite())
                            {
                                return Err(bad());
                            }
                            for spacing in [
                                &p.style.line_spacing,
                                &p.style.space_before,
                                &p.style.space_after,
                            ]
                            .into_iter()
                            .flatten()
                            {
                                let (Spacing::Percent(n) | Spacing::Points(n)) = spacing;
                                if !n.is_finite() {
                                    return Err(bad());
                                }
                            }
                            let mut end = 0;
                            for run in &p.runs {
                                if run.start != end
                                    || run.end <= run.start
                                    || run.end > text.len()
                                    || !text.is_char_boundary(run.start)
                                    || !text.is_char_boundary(run.end)
                                    || !run.style.size_pt.is_finite()
                                    || run.style.size_pt <= 0.0
                                {
                                    return Err(bad());
                                }
                                end = run.end;
                            }
                            if end != text.len() {
                                return Err(bad());
                            }
                        }
                    }
                    _ => return Err(bad()),
                }
            }
        }
        Ok(())
    }
}

fn text_body(items: &[ShapeItem], id: u32) -> Result<&TextBody, String> {
    fn find<'a>(items: &'a [ShapeItem], id: u32, matches: &mut Vec<&'a TextBody>) {
        for item in items {
            match item {
                ShapeItem::Shape(s) if s.id == Some(id) => {
                    if let Some(text) = &s.text {
                        matches.push(text);
                    }
                }
                ShapeItem::Group(g) => find(&g.children, id, matches),
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    find(items, id, &mut found);
    if found.len() != 1 {
        return Err(format!("no unique native text body for shape {id}"));
    }
    Ok(found[0])
}

fn paragraphs(
    deck: &Deck,
    slide: usize,
    id: u32,
    text: &str,
) -> Result<Vec<ParagraphFormat>, String> {
    let body = text_body(&deck.slides[slide].content.shapes, id)?;
    let native = body
        .paragraphs
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    if native != text {
        return Err(format!(
            "native text differs from model at slide {} shape {id}",
            slide + 1
        ));
    }
    // Resolve each native run once. Split its range into the model's paragraphs
    // afterwards: a native soft break is also a newline in the replicated text.
    let mut spans = Vec::new();
    let (mut byte, mut utf16) = (0, 0);
    for (index, p) in body.paragraphs.iter().enumerate() {
        if index > 0 {
            byte += 1;
            utf16 += 1;
        }
        for run in &p.runs {
            let end = utf16 + run.text.encode_utf16().count();
            if !run.text.is_empty() {
                let style = CharacterStyle::read(
                    text_format_for_deck_shape(deck, slide, id, utf16, end)
                        .ok_or_else(|| "native character style unavailable".to_owned())?,
                )?;
                spans.push(CharacterStyleRun {
                    start: byte,
                    end: byte + run.text.len(),
                    style,
                });
            }
            byte += run.text.len();
            utf16 = end;
        }
    }
    let mut out = Vec::new();
    let (mut byte, mut utf16) = (0, 0);
    for text in text.split('\n') {
        let style = paragraph_format_for_deck_shape(deck, slide, id, utf16, utf16)
            .ok_or_else(|| "native paragraph style unavailable".to_owned())?
            .into();
        let mut runs: Vec<CharacterStyleRun> = Vec::new();
        for span in &spans {
            let start = span.start.max(byte);
            let end = span.end.min(byte + text.len());
            if start >= end {
                continue;
            }
            if let Some(prior) = runs.last_mut().filter(|p| p.style == span.style) {
                prior.end = end - byte;
            } else {
                runs.push(CharacterStyleRun {
                    start: start - byte,
                    end: end - byte,
                    style: span.style.clone(),
                });
            }
        }
        out.push(ParagraphFormat { style, runs });
        byte += text.len() + 1;
        utf16 += text.encode_utf16().count() + 1;
    }
    Ok(out)
}

impl JadePptx {
    /// Resolves presentation defaults, master/layout placeholder styles, theme
    /// fonts/colours, direct properties and collaborative character marks.
    pub fn inspect_formatting(&self, model: &PptxModel) -> Result<PptxFormatting, String> {
        let bytes = self.export(model)?;
        let mut deck = jade_pptx_reader::read_deck(&bytes).map_err(|e| e.to_string())?;
        let package = jade_opc::OpcPackage::open(&bytes).map_err(|e| e.to_string())?;
        // The renderer substitutes slide-number fields. Authoring positions
        // use cached field text, as the writer and replicated model do.
        for slide in &mut deck.slides {
            let xml = package
                .part_str(&slide.part_name)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "native slide part missing".to_owned())?;
            slide.content = jade_pptx_reader::parse_slide_xml(&xml).map_err(|e| e.to_string())?;
        }
        let slides = model.slides();
        if slides.len() != deck.slides.len() {
            return Err("native slide count differs from model".into());
        }
        let mut result = PptxFormatting { slides: Vec::new() };
        for (index, slide) in slides.iter().enumerate() {
            let mut shapes = Vec::new();
            for shape in &slide.shapes {
                let id = self.file_id(ShapeRef {
                    slide: slide.id,
                    shape: shape.id,
                    number: shape.number,
                });
                shapes.push(ShapeFormatting {
                    shape: shape.number,
                    paragraphs: shape
                        .text
                        .as_deref()
                        .map(|text| paragraphs(&deck, index, id, text))
                        .transpose()?,
                });
            }
            result.slides.push(SlideFormatting { shapes });
        }
        result.validate(model)?;
        Ok(result)
    }
}
