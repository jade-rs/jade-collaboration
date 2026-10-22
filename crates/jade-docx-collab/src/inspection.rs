//! Read-only inspection of the materialized Word document. Keep native style
//! resolution on the server; replicas do not predict OOXML inheritance.
use crate::{DocxModel, JadeDocx};
use jade_docx_document_core::document::RunProperties;
use jade_docx_headless_api::{HeadlessParagraphIndent, HeadlessParagraphSpacing};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CharacterStyle {
    pub bold: bool,
    pub italic: bool,
    pub bold_complex: bool,
    pub italic_complex: bool,
    pub underline: bool,
    pub underline_style: Option<String>,
    pub underline_color: Option<String>,
    pub strike: bool,
    pub double_strike: bool,
    /// Points, rather than the half points accepted by a format edit.
    pub font_size: Option<f32>,
    pub font_family: Option<String>,
    pub font_color: Option<String>,
    pub highlight: Option<String>,
    pub character_style: Option<String>,
}

impl CharacterStyle {
    fn read(props: &RunProperties) -> Self {
        Self {
            bold: props.bold.unwrap_or(false),
            italic: props.italic.unwrap_or(false),
            bold_complex: props.bold_complex.unwrap_or(false),
            italic_complex: props.italic_complex.unwrap_or(false),
            underline: props.underline.unwrap_or(false),
            underline_style: props.underline_kind.as_deref().map(str::to_owned),
            underline_color: props.underline_color.as_deref().map(str::to_owned),
            strike: props.strike.unwrap_or(false),
            double_strike: props.double_strike.unwrap_or(false),
            font_size: props.font_size.filter(|n| n.is_finite()),
            font_family: props.font_family.as_deref().map(str::to_owned),
            font_color: props.color.as_deref().map(str::to_owned),
            highlight: props.highlight.as_deref().map(str::to_owned),
            character_style: props.style.as_deref().map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CharacterStyleRun {
    pub start: usize,
    pub end: usize,
    pub style: CharacterStyle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParagraphFormat {
    pub style_id: Option<String>,
    pub alignment: Option<String>,
    pub indent: Option<HeadlessParagraphIndent>,
    pub spacing: Option<HeadlessParagraphSpacing>,
    pub runs: Vec<CharacterStyleRun>,
}

/// One entry for every paragraph in the accompanying model, in its read order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocxFormatting {
    pub paragraphs: Vec<ParagraphFormat>,
}

impl DocxFormatting {
    /// Refuses a response whose run boundaries do not cover its model's text.
    pub fn validate(&self, model: &DocxModel) -> Result<(), String> {
        let paragraphs = model.paragraphs();
        if paragraphs.len() != self.paragraphs.len() {
            return Err("formatting paragraph count differs from its model".into());
        }
        for (p, format) in paragraphs.iter().zip(&self.paragraphs) {
            let mut end = 0;
            for run in &format.runs {
                if run.start != end
                    || run.end <= run.start
                    || run.end > p.text.len()
                    || !p.text.is_char_boundary(run.start)
                    || !p.text.is_char_boundary(run.end)
                {
                    return Err("formatting runs do not partition UTF-8 text".into());
                }
                end = run.end;
            }
            if end != p.text.len() {
                return Err("formatting runs do not cover all text".into());
            }
        }
        Ok(())
    }
}

impl JadeDocx {
    /// Resolves document defaults, paragraph styles, character styles and
    /// direct properties after applying collaborative marks to a private copy.
    /// Every range uses UTF-8 bytes of the accompanying model's text.
    pub fn inspect_formatting(&self, model: &DocxModel) -> Result<DocxFormatting, String> {
        let word = self.formatted(model)?;
        let mut paragraphs = Vec::new();
        for paragraph in model.paragraphs() {
            let surface = word
                .paragraph_story_surface(&paragraph.at)
                .ok_or_else(|| format!("no native paragraph for {:?}", paragraph.at))?;
            let native = word
                .paragraph(surface)
                .ok_or_else(|| "native paragraph disappeared".to_owned())?;
            let props = native.properties.as_ref();
            let style_id = props.and_then(|p| p.style.as_deref());
            let mut out = ParagraphFormat {
                style_id: style_id.map(str::to_owned),
                alignment: props
                    .and_then(|p| p.alignment.as_deref())
                    .map(str::to_owned),
                indent: props
                    .and_then(|p| p.indent.as_ref())
                    .map(|p| HeadlessParagraphIndent {
                        left: p.left,
                        right: p.right,
                        first_line: p.first_line,
                        hanging: p.hanging,
                    }),
                spacing: props
                    .and_then(|p| p.spacing.as_ref())
                    .map(|p| HeadlessParagraphSpacing {
                        before: p.before,
                        after: p.after,
                        line: p.line,
                        line_rule: p.line_rule.clone(),
                    }),
                runs: Vec::new(),
            };
            let mut offset = 0;
            let mut text = String::new();
            for run in &native.runs {
                if run.text.is_empty() {
                    continue;
                }
                let effective = word
                    .styles_part
                    .as_ref()
                    .and_then(|styles| {
                        styles.resolve_effective_run_properties(style_id, run.properties.as_ref())
                    })
                    .or_else(|| run.properties.clone())
                    .unwrap_or_default();
                let style = CharacterStyle::read(&effective);
                let end = offset + run.text.len();
                if let Some(prior) = out.runs.last_mut().filter(|p| p.style == style) {
                    prior.end = end;
                } else {
                    out.runs.push(CharacterStyleRun {
                        start: offset,
                        end,
                        style,
                    });
                }
                offset = end;
                text.push_str(&run.text);
            }
            if text != paragraph.text {
                return Err(format!(
                    "native text differs from model at {:?}",
                    paragraph.at
                ));
            }
            paragraphs.push(out);
        }
        Ok(DocxFormatting { paragraphs })
    }
}
