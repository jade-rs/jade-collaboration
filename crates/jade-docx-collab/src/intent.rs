//! Index-addressed edits, resolved against a view into identifier-addressed
//! [`DocxOp`]s.
//!
//! This is the collaborative subset of jade's headless commands that the
//! jade-core prototype (`CollaborativeOperation`) carried, with the same wire
//! format. Paragraph indices are model ordinals ([`DocxModel::paragraphs`]):
//! jade's paragraph surfaces without equations and note separators — the
//! body's paragraphs with table cells row by row, each text box's paragraphs
//! right after the body paragraph that anchors it, each footnote's right
//! after the body paragraph that first references it, then the other
//! footnotes (by `w:id`) and the endnotes (by first reference from the body
//! or a text box, then by `w:id`). Offsets are UTF-8 byte offsets on
//! character boundaries. Resolving pins the caret to the character before
//! it, so the edit keeps its meaning however long it takes to reach the
//! server. Text boxes and note references take no text: a caret at their
//! offset is before them, as jade types there, except at a paragraph's
//! start, where typing goes after them, as in jade.
//!
//! Formatting and comments are named the same way: a range becomes the ids
//! of its first and last character, so text edited concurrently moves with
//! it. Comments are named by the model's id (`lamport@rN`, as
//! [`DocxModel::comments`] lists them) or, for a comment that came with the
//! file, by jade's `w:id`.

use jade_collab_core::grid::Merge;
use jade_collab_core::id::Id;
use jade_collab_sync::Lane;
use jade_docx_headless_api::{HeadlessCommand, HeadlessDeleteDirection};
use serde::{Deserialize, Serialize};

use crate::model::{
    CommentContent, DocxModel, DocxOp, FormatChange, Paragraph, ParagraphAlignment, StoryId,
};

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
    /// Formats the text between two carets. A range that crosses a table
    /// formats every story it crosses; a paragraph change formats the
    /// paragraphs the range touches.
    Format {
        start_paragraph: usize,
        start_offset: usize,
        end_paragraph: usize,
        end_offset: usize,
        change: FormatChange,
    },
    /// Formats whole paragraphs `start_paragraph..=end_paragraph`.
    FormatParagraphs {
        start_paragraph: usize,
        end_paragraph: usize,
        change: FormatChange,
    },
    /// Like jade's `add_comment`: omitted offsets mean the start of the first
    /// paragraph and the end of the last. Both ends must be in one story.
    AddComment {
        start_paragraph: usize,
        #[serde(default)]
        start_offset: Option<usize>,
        end_paragraph: usize,
        #[serde(default)]
        end_offset: Option<usize>,
        text: String,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        initials: Option<String>,
        #[serde(default)]
        date: Option<String>,
    },
    ReplyToComment {
        comment_id: String,
        text: String,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        initials: Option<String>,
        #[serde(default)]
        date: Option<String>,
    },
    ResolveComment {
        comment_id: String,
        resolved: bool,
    },
    DeleteComment {
        comment_id: String,
    },
    /// Replaces a comment's text (a root or a reply).
    UpdateComment {
        comment_id: String,
        text: String,
    },
    /// Merges the rectangle of a top-level table's cells (zero-based rows and
    /// columns, as `read_document` lists them).
    MergeTableCells {
        table: usize,
        top_row: usize,
        left_column: usize,
        bottom_row: usize,
        right_column: usize,
    },
    /// Undoes the merge whose top-left cell is (`row`, `column`).
    UnmergeTableCells {
        table: usize,
        row: usize,
        column: usize,
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
    #[error("the range spans more than one story (body, table cell, note, text box)")]
    CrossStory,
    #[error("the range holds no text")]
    EmptyRange,
    #[error("paragraph {0} is empty and ends its story: there is nothing to name it by")]
    EmptyParagraph(usize),
    #[error("{0}")]
    InvalidFormat(String),
    #[error("no comment {0:?}")]
    NoSuchComment(String),
    #[error("no top-level table {0}")]
    NoSuchTable(usize),
    #[error("cell ({row}, {column}) is outside the table or starts no merge")]
    NoSuchCell { row: usize, column: usize },
    #[error("paragraph {0} is in a text box, whose paragraphs cannot be split or joined")]
    FixedParagraphs(usize),
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
            HeadlessCommand::ApplyBold {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                enabled: value,
            }
            | HeadlessCommand::SetBold {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                value,
            } => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                FormatChange::Bold { value },
            )),
            HeadlessCommand::ApplyItalic {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                enabled,
            } => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                FormatChange::Italic { value: enabled },
            )),
            HeadlessCommand::ApplyUnderline {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                kind,
                enabled,
            } => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                FormatChange::Underline {
                    value: enabled,
                    style: kind,
                },
            )),
            HeadlessCommand::ApplyStrike {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                double,
                enabled,
            } => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                FormatChange::Strike {
                    value: enabled,
                    double,
                },
            )),
            HeadlessCommand::SetFontSize {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                font_size,
            } if font_size.is_finite() && font_size > 0.0 => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                // jade rounds to half points too.
                FormatChange::FontSize {
                    half_points: (font_size * 2.0).round().min(u32::MAX as f32) as u32,
                },
            )),
            HeadlessCommand::SetFontFamily {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                font_family,
            } => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                FormatChange::FontFamily {
                    family: font_family,
                },
            )),
            HeadlessCommand::SetFontColor {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                color,
            } => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                FormatChange::FontColor { color },
            )),
            HeadlessCommand::SetHighlightColor {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                color,
            } => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                FormatChange::Highlight { color },
            )),
            HeadlessCommand::ClearFormatting {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
            } => Ok(format(
                (start_paragraph, start_offset),
                (end_paragraph, end_offset),
                FormatChange::ClearFormatting,
            )),
            HeadlessCommand::SetParagraphAlignment {
                start_paragraph,
                end_paragraph,
                ref alignment,
            } if ParagraphAlignment::from_jade(alignment).is_some() => Ok(Self::FormatParagraphs {
                start_paragraph,
                end_paragraph,
                change: FormatChange::Alignment {
                    alignment: ParagraphAlignment::from_jade(alignment).expect("checked"),
                },
            }),
            HeadlessCommand::SetParagraphStyle {
                start_paragraph,
                end_paragraph,
                style_id,
            } => Ok(Self::FormatParagraphs {
                start_paragraph,
                end_paragraph,
                change: FormatChange::ParagraphStyle { style_id },
            }),
            HeadlessCommand::AddComment {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                text,
                author,
                initials,
                date,
            } => Ok(Self::AddComment {
                start_paragraph,
                start_offset,
                end_paragraph,
                end_offset,
                text,
                author,
                initials,
                date,
            }),
            HeadlessCommand::ReplyToComment {
                comment_id,
                text,
                author,
                initials,
                date,
            } => Ok(Self::ReplyToComment {
                comment_id,
                text,
                author,
                initials,
                date,
            }),
            HeadlessCommand::ResolveComment {
                comment_id,
                resolved,
            } => Ok(Self::ResolveComment {
                comment_id,
                resolved,
            }),
            HeadlessCommand::DeleteComment { comment_id } => Ok(Self::DeleteComment { comment_id }),
            HeadlessCommand::UpdateComment { comment_id, text } => {
                Ok(Self::UpdateComment { comment_id, text })
            }
            other => Err(UnsupportedCommand {
                command_name: other.name(),
                lane,
            }),
        }
    }
}

fn format(start: (usize, usize), end: (usize, usize), change: FormatChange) -> IntentOp {
    IntentOp::Format {
        start_paragraph: start.0,
        start_offset: start.1,
        end_paragraph: end.0,
        end_offset: end.1,
        change,
    }
}

/// The lane a headless command needs. Plain text edits commute; everything
/// else — formatting, structure, comments, revisions, page setup — rewrites
/// state other edits depend on and goes through the sequencer. (Formatting
/// and comments are sequenced so jade applies them in one order: two
/// concurrent formats of overlapping ranges must end the same everywhere.)
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
/// its index among the story's visible atoms (text boxes and note
/// references not counted).
struct Caret {
    story: StoryId,
    parent: Option<Id>,
    index: usize,
    /// Where typed text goes: after `parent`, or at a paragraph's start
    /// after the anchors there (jade types after them).
    typing: Option<Id>,
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
    let leading = p.anchors.iter().rev().find(|a| a.offset == 0);
    let typing = match leading {
        Some(anchor) if chars_before == 0 => Some(anchor.id),
        _ => parent,
    };
    Ok(Caret {
        story: p.story,
        parent,
        index,
        typing,
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
                parent: at.typing,
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
            if matches!(at.story, StoryId::TextBox { .. }) {
                return Err(IntentError::FixedParagraphs(*paragraph));
            }
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
                    parent: start.typing,
                    first: ids(text.chars().count() as u64),
                    text: text.clone(),
                });
            }
            Ok(ops)
        }
        IntentOp::Format {
            start_paragraph,
            start_offset,
            end_paragraph,
            end_offset,
            change,
        } => {
            change.validate().map_err(IntentError::InvalidFormat)?;
            caret(&paragraphs, *start_paragraph, *start_offset)?;
            caret(&paragraphs, *end_paragraph, *end_offset)?;
            if (*start_paragraph, *start_offset) > (*end_paragraph, *end_offset) {
                return Err(IntentError::InvertedRange);
            }
            // Each paragraph's slice of the range: characters, and the mark
            // when the range runs past it.
            let slices = (*start_paragraph..=*end_paragraph).map(|n| {
                let p = &paragraphs[n];
                let from = if n == *start_paragraph {
                    chars_before(p, *start_offset)
                } else {
                    0
                };
                let to = if n == *end_paragraph {
                    chars_before(p, *end_offset)
                } else {
                    p.chars.len()
                };
                let mark = p.mark.filter(|_| n < *end_paragraph);
                (p.story, p.chars[from..to].iter().copied().chain(mark))
            });
            Ok(per_story(slices)
                .into_iter()
                .map(|(story, first, last)| DocxOp::Format {
                    story,
                    start: first,
                    end: last,
                    change: change.clone(),
                })
                .collect())
        }
        IntentOp::FormatParagraphs {
            start_paragraph,
            end_paragraph,
            change,
        } => {
            change.validate().map_err(IntentError::InvalidFormat)?;
            for n in [*start_paragraph, *end_paragraph] {
                if n >= paragraphs.len() {
                    return Err(IntentError::NoSuchParagraph(n));
                }
            }
            if start_paragraph > end_paragraph {
                return Err(IntentError::InvertedRange);
            }
            let mut slices = Vec::new();
            for (n, p) in paragraphs
                .iter()
                .enumerate()
                .take(end_paragraph + 1)
                .skip(*start_paragraph)
            {
                if p.chars.is_empty() && p.mark.is_none() {
                    return Err(IntentError::EmptyParagraph(n));
                }
                slices.push((p.story, p.chars.iter().copied().chain(p.mark)));
            }
            Ok(per_story(slices)
                .into_iter()
                .map(|(story, first, last)| DocxOp::Format {
                    story,
                    start: first,
                    end: last,
                    change: change.clone(),
                })
                .collect())
        }
        IntentOp::AddComment {
            start_paragraph,
            start_offset,
            end_paragraph,
            end_offset,
            text,
            author,
            initials,
            date,
        } => {
            let end_offset = match end_offset {
                Some(offset) => *offset,
                None => paragraphs
                    .get(*end_paragraph)
                    .ok_or(IntentError::NoSuchParagraph(*end_paragraph))?
                    .text
                    .len(),
            };
            let start = caret(&paragraphs, *start_paragraph, start_offset.unwrap_or(0))?;
            let end = caret(&paragraphs, *end_paragraph, end_offset)?;
            if start.story != end.story {
                return Err(IntentError::CrossStory);
            }
            if start.index > end.index {
                return Err(IntentError::InvertedRange);
            }
            let chars: Vec<Id> = story_atoms(&paragraphs, start.story)[start.index..end.index]
                .iter()
                .filter_map(|a| a.ok())
                .collect();
            let (Some(first), Some(last)) = (chars.first(), chars.last()) else {
                return Err(IntentError::EmptyRange);
            };
            Ok(vec![DocxOp::AddComment {
                story: start.story,
                start: *first,
                end: *last,
                id: ids(1),
                content: CommentContent {
                    text: text.clone(),
                    author: author.clone(),
                    initials: initials.clone(),
                    date: date.clone(),
                },
            }])
        }
        IntentOp::ReplyToComment {
            comment_id,
            text,
            author,
            initials,
            date,
        } => Ok(vec![DocxOp::ReplyToComment {
            comment: comment(view, comment_id)?,
            id: ids(1),
            content: CommentContent {
                text: text.clone(),
                author: author.clone(),
                initials: initials.clone(),
                date: date.clone(),
            },
        }]),
        IntentOp::ResolveComment {
            comment_id,
            resolved,
        } => Ok(vec![DocxOp::ResolveComment {
            comment: comment(view, comment_id)?,
            resolved: *resolved,
        }]),
        IntentOp::DeleteComment { comment_id } => Ok(vec![DocxOp::DeleteComment {
            comment: comment(view, comment_id)?,
        }]),
        IntentOp::UpdateComment { comment_id, text } => Ok(vec![DocxOp::UpdateComment {
            comment: comment(view, comment_id)?,
            text: text.clone(),
        }]),
        IntentOp::MergeTableCells {
            table,
            top_row,
            left_column,
            bottom_row,
            right_column,
        } => {
            let t = view
                .tables()
                .into_iter()
                .nth(*table)
                .ok_or(IntentError::NoSuchTable(*table))?;
            let outside = |row: usize, column: usize| IntentError::NoSuchCell { row, column };
            let row = |r: usize, c: usize| t.rows.get(r).copied().ok_or(outside(r, c));
            let column = |r: usize, c: usize| t.columns.get(c).copied().ok_or(outside(r, c));
            let merge = Merge {
                top: row(*top_row, *left_column)?,
                left: column(*top_row, *left_column)?,
                bottom: row(*bottom_row, *right_column)?,
                right: column(*bottom_row, *right_column)?,
            };
            // One new story per follower: the cells below the top-left one.
            let followers = bottom_row.saturating_sub(*top_row) as u64;
            Ok(vec![DocxOp::MergeCells {
                table: t.id,
                merge,
                first: ids(followers.max(1)),
            }])
        }
        IntentOp::UnmergeTableCells { table, row, column } => {
            let t = view
                .tables()
                .into_iter()
                .nth(*table)
                .ok_or(IntentError::NoSuchTable(*table))?;
            let &(top, left, bottom, right) = t
                .merges
                .iter()
                .find(|(top, left, _, _)| (*top, *left) == (*row, *column))
                .ok_or(IntentError::NoSuchCell {
                    row: *row,
                    column: *column,
                })?;
            // One new story per cell the merge removed (all but its first column).
            let freed = ((bottom - top + 1) * (right - left)) as u64;
            Ok(vec![DocxOp::UnmergeCells {
                table: t.id,
                top: t.rows[top],
                left: t.columns[left],
                first: ids(freed.max(1)),
            }])
        }
    }
}

fn chars_before(p: &Paragraph, offset: usize) -> usize {
    p.text[..offset].chars().count()
}

/// The first and last atom of each story among `slices`, in the order the
/// stories first appear; stories with no atom in range are left out.
fn per_story<I: IntoIterator<Item = Id>>(
    slices: impl IntoIterator<Item = (StoryId, I)>,
) -> Vec<(StoryId, Id, Id)> {
    let mut out: Vec<(StoryId, Id, Id)> = Vec::new();
    for (story, atoms) in slices {
        for atom in atoms {
            match out.iter_mut().find(|(s, _, _)| *s == story) {
                Some(entry) => entry.2 = atom,
                None => out.push((story, atom, atom)),
            }
        }
    }
    out
}

/// A live comment named by its model id (`lamport@rN`) or its `w:id` in the
/// file.
fn comment(view: &DocxModel, key: &str) -> Result<Id, IntentError> {
    let parsed = key.split_once('@').and_then(|(lamport, replica)| {
        Some(Id::new(
            lamport.parse().ok()?,
            jade_collab_core::id::ReplicaId(replica.strip_prefix('r')?.parse().ok()?),
        ))
    });
    view.comments()
        .into_iter()
        .find(|c| Some(c.id) == parsed || c.file_id.as_deref() == Some(key))
        .map(|c| c.id)
        .ok_or_else(|| IntentError::NoSuchComment(key.to_string()))
}
