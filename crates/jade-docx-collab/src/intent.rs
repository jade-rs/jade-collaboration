//! Index-addressed edits, resolved against a view into identifier-addressed
//! [`DocxOp`]s.
//!
//! This is the collaborative subset of jade's headless commands that the
//! jade-core prototype (`CollaborativeOperation`) carried, with the same wire
//! format. Paragraph indices are model ordinals — jade's body paragraphs,
//! table cells included, notes and text boxes excluded — and offsets are
//! UTF-8 byte offsets on character boundaries. Resolving pins the caret to
//! the character before it, so the edit keeps its meaning however long it
//! takes to reach the server.

use jade_collab_core::id::Id;
use jade_collab_sync::Lane;
use jade_docx_headless_api::{HeadlessCommand, HeadlessDeleteDirection};
use serde::{Deserialize, Serialize};

use crate::model::{DocxModel, DocxOp, Paragraph, StoryId};

fn one() -> usize {
    1
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IntentOp {
    InsertText {
        paragraph: usize,
        offset: usize,
        text: String,
    },
    ReplaceRange {
        start_paragraph: usize,
        start_offset: usize,
        end_paragraph: usize,
        end_offset: usize,
        text: String,
    },
    /// Deletes `count` characters (Unicode scalar values); crossing a
    /// paragraph end joins the paragraphs.
    DeleteText {
        paragraph: usize,
        offset: usize,
        #[serde(default)]
        direction: HeadlessDeleteDirection,
        #[serde(default = "one")]
        count: usize,
    },
    InsertParagraphBreak {
        paragraph: usize,
        offset: usize,
        #[serde(default = "one")]
        count: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IntentError {
    #[error("paragraph {0} does not exist")]
    NoSuchParagraph(usize),
    #[error("offset {offset} is not a character boundary of paragraph {paragraph}")]
    BadOffset { paragraph: usize, offset: usize },
    #[error("the range starts after it ends")]
    InvertedRange,
    #[error("the range spans more than one story (body, table cell)")]
    CrossStory,
}

/// A headless command with no collaborative intent form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedCommand {
    pub command_name: &'static str,
    pub lane: Lane,
}

impl TryFrom<HeadlessCommand> for IntentOp {
    type Error = UnsupportedCommand;

    fn try_from(command: HeadlessCommand) -> Result<Self, Self::Error> {
        let lane = lane_for_command(&command);
        match command {
            HeadlessCommand::InsertText {
                paragraph,
                offset,
                text,
            } => Ok(Self::InsertText {
                paragraph,
                offset,
                text,
            }),
            HeadlessCommand::ReplaceRange {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                text,
            } => Ok(Self::ReplaceRange {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                text,
            }),
            HeadlessCommand::DeleteText {
                paragraph,
                offset,
                direction,
                count,
            } => Ok(Self::DeleteText {
                paragraph,
                offset,
                direction,
                count,
            }),
            HeadlessCommand::InsertParagraphBreak {
                paragraph,
                offset,
                count,
            } => Ok(Self::InsertParagraphBreak {
                paragraph,
                offset,
                count,
            }),
            other => Err(UnsupportedCommand {
                command_name: other.name(),
                lane,
            }),
        }
    }
}

/// The lane a headless command needs. Plain text edits commute; everything
/// else — formatting, structure, comments, revisions, page setup — rewrites
/// state other edits depend on and goes through the sequencer.
pub fn lane_for_command(command: &HeadlessCommand) -> Lane {
    match command {
        HeadlessCommand::InsertText { .. }
        | HeadlessCommand::ReplaceRange { .. }
        | HeadlessCommand::DeleteText { .. }
        | HeadlessCommand::InsertParagraphBreak { .. } => Lane::Convergent,
        _ => Lane::Sequenced,
    }
}

/// A caret: the story it is in, the atom before it (`None`: story start) and
/// its index among the story's visible atoms.
struct Caret {
    story: StoryId,
    parent: Option<Id>,
    index: usize,
}

/// Visible atoms of `story` in order: each paragraph's characters, then its
/// mark. `Ok(id)` is a character, `Err(id)` a mark.
fn story_atoms(paragraphs: &[Paragraph], story: StoryId) -> Vec<Result<Id, Id>> {
    paragraphs
        .iter()
        .filter(|p| p.story == story)
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
    let preceding: Vec<&Paragraph> = paragraphs[..paragraph]
        .iter()
        .filter(|q| q.story == p.story)
        .collect();
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
    Ok(Caret {
        story: p.story,
        parent,
        index,
    })
}

/// Deleting `atoms`: one text deletion, then a join per mark.
fn deletion(story: StoryId, atoms: &[Result<Id, Id>]) -> Vec<DocxOp> {
    let ids: Vec<Id> = atoms.iter().filter_map(|a| a.ok()).collect();
    let mut ops = Vec::new();
    if !ids.is_empty() {
        ops.push(DocxOp::DeleteText { story, ids });
    }
    ops.extend(
        atoms
            .iter()
            .filter_map(|a| a.err())
            .map(|mark| DocxOp::JoinParagraph { story, mark }),
    );
    ops
}

/// Resolves `op` against `view`. `ids(n)` must reserve `n` consecutive fresh
/// identifiers (see `Replica::reserve`). The operations are meant to be
/// submitted in order.
pub fn resolve(
    view: &DocxModel,
    op: &IntentOp,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Result<Vec<DocxOp>, IntentError> {
    let paragraphs = view.paragraphs();
    match op {
        IntentOp::InsertText {
            paragraph,
            offset,
            text,
        } => {
            let at = caret(&paragraphs, *paragraph, *offset)?;
            if text.is_empty() {
                return Ok(Vec::new());
            }
            Ok(vec![DocxOp::InsertText {
                story: at.story,
                parent: at.parent,
                first: ids(text.chars().count() as u64),
                text: text.clone(),
            }])
        }
        IntentOp::InsertParagraphBreak {
            paragraph,
            offset,
            count,
        } => {
            let at = caret(&paragraphs, *paragraph, *offset)?;
            let mut parent = at.parent;
            Ok((0..*count)
                .map(|_| {
                    let id = ids(1);
                    let op = DocxOp::SplitParagraph {
                        story: at.story,
                        parent,
                        id,
                    };
                    parent = Some(id);
                    op
                })
                .collect())
        }
        IntentOp::DeleteText {
            paragraph,
            offset,
            direction,
            count,
        } => {
            let at = caret(&paragraphs, *paragraph, *offset)?;
            let atoms = story_atoms(&paragraphs, at.story);
            let range = match direction {
                HeadlessDeleteDirection::Forward => at.index..(at.index + count).min(atoms.len()),
                HeadlessDeleteDirection::Backward => at.index.saturating_sub(*count)..at.index,
            };
            Ok(deletion(at.story, &atoms[range]))
        }
        IntentOp::ReplaceRange {
            start_paragraph,
            start_offset,
            end_paragraph,
            end_offset,
            text,
        } => {
            let start = caret(&paragraphs, *start_paragraph, *start_offset)?;
            let end = caret(&paragraphs, *end_paragraph, *end_offset)?;
            if start.story != end.story {
                return Err(IntentError::CrossStory);
            }
            if start.index > end.index {
                return Err(IntentError::InvertedRange);
            }
            let atoms = story_atoms(&paragraphs, start.story);
            let mut ops = deletion(start.story, &atoms[start.index..end.index]);
            if !text.is_empty() {
                ops.push(DocxOp::InsertText {
                    story: start.story,
                    parent: start.parent,
                    first: ids(text.chars().count() as u64),
                    text: text.clone(),
                });
            }
            Ok(ops)
        }
    }
}
