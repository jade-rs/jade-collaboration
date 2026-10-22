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
//! Formatting and comments are named the same way, so text edited
//! concurrently moves with them. Character formatting (bold, italic,
//! underline, strike, font size and family, colour, highlight) becomes a
//! convergent [`DocxOp::Mark`] per story, anchored before the range's first
//! character and before the atom after its last one (the attributes expand
//! at their end, as in Word); clearing becomes a mark that unsets them plus
//! the sequenced clear of the file's own formatting; paragraph formatting
//! and comments name the ids of the range's first and last atoms and are
//! sequenced. Comments are named by the model's id (`lamport@rN`, as
//! [`DocxModel::comments`] lists them) or, for a comment that came with the
//! file, by jade's `w:id`.

use std::collections::BTreeSet;

use jade_collab_core::grid::Merge;
use jade_collab_core::id::{Id, IdError, ReplicaId};
use jade_collab_core::marks::Expand;
use jade_collab_sync::Lane;
use jade_docx_headless_api::{HeadlessCommand, HeadlessDeleteDirection};
use serde::{Deserialize, Serialize};

use crate::model::{
    CommentContent, DocxModel, DocxOp, FormatChange, NoteKind, Paragraph, ParagraphAlignment,
    RevisionKind, StoryId,
};

/// Who resolves an edit, and when: the replica whose own tracked insertions
/// a deletion removes outright, and the date its tracked changes carry
/// (`w:date`). The author's name is not here: the server says who a replica
/// is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub replica: Option<ReplicaId>,
    pub date: Option<String>,
}

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
    /// A new footnote at a caret of a body paragraph or table cell, before
    /// any text box or note reference at that offset (as jade inserts one);
    /// its text starts with a tab, as jade writes it.
    InsertFootnote {
        paragraph: usize,
        offset: usize,
        #[serde(default)]
        text: String,
    },
    InsertEndnote {
        paragraph: usize,
        offset: usize,
        #[serde(default)]
        text: String,
    },
    /// Deletes a footnote (by its `w:id`) and every reference to it.
    DeleteFootnote {
        footnote_id: String,
    },
    DeleteEndnote {
        endnote_id: String,
    },
    /// A new text box (a rectangle holding one paragraph of `text`) at a
    /// caret of a body paragraph or table cell where no text box or note
    /// reference sits; its paragraphs are listed right after the one that
    /// anchors it.
    InsertTextBox {
        paragraph: usize,
        offset: usize,
        #[serde(default)]
        text: String,
        /// Default 216.
        #[serde(default)]
        width_px: Option<u32>,
        /// Default 96.
        #[serde(default)]
        height_px: Option<u32>,
    },
    /// Deletes the `text_box`-th text box (in run order) anchored in
    /// `paragraph`, with its content.
    DeleteTextBox {
        paragraph: usize,
        text_box: usize,
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
    /// Switches the document's Track Changes on or off for everyone.
    SetTracking {
        on: bool,
    },
    /// Accepts the listed revisions (model ids or the file's `w:id`s), or
    /// those touching paragraphs `start_paragraph..=end_paragraph`, or all.
    AcceptRevisions {
        #[serde(default)]
        revision_ids: Option<Vec<String>>,
        #[serde(default)]
        start_paragraph: Option<usize>,
        #[serde(default)]
        end_paragraph: Option<usize>,
    },
    /// Rejects revisions, chosen as for `AcceptRevisions`.
    RejectRevisions {
        #[serde(default)]
        revision_ids: Option<Vec<String>>,
        #[serde(default)]
        start_paragraph: Option<usize>,
        #[serde(default)]
        end_paragraph: Option<usize>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IntentError {
    #[error(transparent)]
    Identifiers(#[from] IdError),
    #[error(transparent)]
    TableBudget(#[from] crate::TableBudgetExceeded),
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
    #[error("no footnote or endnote {0:?}")]
    NoSuchNote(String),
    #[error("paragraph {paragraph} has no text box {text_box}")]
    NoSuchTextBox { paragraph: usize, text_box: usize },
    #[error("no top-level table {0}")]
    NoSuchTable(usize),
    #[error("cell ({row}, {column}) is outside the table or starts no merge")]
    NoSuchCell { row: usize, column: usize },
    #[error("no revision {0:?}")]
    NoSuchRevision(String),
    #[error("there are no revisions to decide")]
    NoRevisions,
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
            HeadlessCommand::InsertFootnote {
                paragraph,
                offset,
                text,
            } => Ok(Self::InsertFootnote {
                paragraph,
                offset,
                text,
            }),
            HeadlessCommand::InsertEndnote {
                paragraph,
                offset,
                text,
            } => Ok(Self::InsertEndnote {
                paragraph,
                offset,
                text,
            }),
            HeadlessCommand::DeleteFootnote { footnote_id } => {
                Ok(Self::DeleteFootnote { footnote_id })
            }
            HeadlessCommand::DeleteEndnote { endnote_id } => Ok(Self::DeleteEndnote { endnote_id }),
            HeadlessCommand::DeleteTextBox {
                paragraph,
                text_box,
            } => Ok(Self::DeleteTextBox {
                paragraph,
                text_box,
            }),
            // The author is the server's to say; jade's session fields are
            // not carried.
            HeadlessCommand::StartChangeTracking { .. } => Ok(Self::SetTracking { on: true }),
            HeadlessCommand::StopChangeTracking => Ok(Self::SetTracking { on: false }),
            HeadlessCommand::AcceptRevisions {
                kind: None,
                start_paragraph,
                end_paragraph,
                revision_id,
                revision_ids,
                revision_targets: None,
            } => Ok(Self::AcceptRevisions {
                revision_ids: revision_ids.or(revision_id.map(|id| vec![id])),
                start_paragraph,
                end_paragraph,
            }),
            HeadlessCommand::RejectRevisions {
                kind: None,
                start_paragraph,
                end_paragraph,
                revision_id,
                revision_ids,
                revision_targets: None,
            } => Ok(Self::RejectRevisions {
                revision_ids: revision_ids.or(revision_id.map(|id| vec![id])),
                start_paragraph,
                end_paragraph,
            }),
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

/// The lane a headless command needs. Plain text edits and simple character
/// formatting (a mark: concurrent marks resolve to the newest per attribute
/// on every replica) commute; everything else — clearing the file's
/// formatting, paragraph formatting, structure, comments, revisions, page
/// setup — rewrites state other edits depend on and goes through the
/// sequencer.
pub fn lane_for_command(command: &HeadlessCommand) -> Lane {
    match command {
        HeadlessCommand::InsertText { .. }
        | HeadlessCommand::ReplaceRange { .. }
        | HeadlessCommand::DeleteText { .. }
        | HeadlessCommand::InsertParagraphBreak { .. }
        | HeadlessCommand::ApplyBold { .. }
        | HeadlessCommand::SetBold { .. }
        | HeadlessCommand::ApplyItalic { .. }
        | HeadlessCommand::ApplyUnderline { .. }
        | HeadlessCommand::ApplyStrike { .. }
        | HeadlessCommand::SetFontSize { .. }
        | HeadlessCommand::SetFontFamily { .. }
        | HeadlessCommand::SetFontColor { .. }
        | HeadlessCommand::SetHighlightColor { .. } => Lane::Convergent,
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

/// Deleting `atoms`: the characters (one plain deletion, and with tracking
/// on one tracked deletion), then a join per mark.
fn deletion(
    view: &DocxModel,
    story: StoryId,
    atoms: &[Result<Id, Id>],
    context: &Context,
    ids: &mut dyn FnMut(u64) -> Result<Id, IdError>,
) -> Result<Vec<DocxOp>, IntentError> {
    let chars: Vec<Id> = atoms.iter().filter_map(|a| a.ok()).collect();
    let (plain, tracked) = if view.tracking() {
        tracked_deletion(view, &chars, context)
    } else {
        (chars, Vec::new())
    };
    let mut ops = Vec::new();
    if !plain.is_empty() {
        ops.push(DocxOp::DeleteText { story, ids: plain });
    }
    if !tracked.is_empty() {
        ops.push(DocxOp::TrackDeleteText {
            story,
            ids: tracked,
            revision: ids(1)?,
            date: context.date.clone(),
        });
    }
    ops.extend(
        atoms
            .iter()
            .filter_map(|a| a.err())
            .map(|mark| DocxOp::JoinParagraph { story, mark }),
    );
    Ok(ops)
}

/// Splits characters a tracked deletion names into (removed outright,
/// tracked): characters already shown as deleted are left alone; one's own
/// still-tracked insertions go (Word's rule); the rest are tracked.
fn tracked_deletion(view: &DocxModel, chars: &[Id], context: &Context) -> (Vec<Id>, Vec<Id>) {
    let me = context.replica;
    let my_name = me.and_then(|me| view.author(me));
    let own = |revision: Id| {
        view.revision(revision)
            .is_some_and(|r| match (&r.author, my_name) {
                (Some(author), Some(name)) => author == name,
                _ => Some(r.replica) == me,
            })
    };
    let named: BTreeSet<Id> = chars.iter().copied().collect();
    let (mut plain, mut tracked) = (Vec::new(), Vec::new());
    for paragraph in view.paragraphs() {
        for (id, marks) in paragraph.chars.iter().zip(&paragraph.tracked) {
            if !named.contains(id) || marks.deleted.is_some() {
                continue;
            }
            if marks.inserted.is_some_and(own) {
                plain.push(*id);
            } else {
                tracked.push(*id);
            }
        }
    }
    (plain, tracked)
}

/// The revisions an accept or reject names: listed by id, touching the
/// paragraphs, or all.
fn chosen_revisions(
    view: &DocxModel,
    revision_ids: &Option<Vec<String>>,
    start_paragraph: Option<usize>,
    end_paragraph: Option<usize>,
) -> Result<Vec<Id>, IntentError> {
    let live = view.revisions();
    let paragraphs = view.paragraphs().len();
    for n in [start_paragraph, end_paragraph].into_iter().flatten() {
        if n >= paragraphs {
            return Err(IntentError::NoSuchParagraph(n));
        }
    }
    let range = match (start_paragraph, end_paragraph) {
        (None, None) => None,
        (start, end) => {
            let (start, end) = (start.unwrap_or(0), end.unwrap_or(paragraphs - 1));
            if start > end {
                return Err(IntentError::InvertedRange);
            }
            Some(start..=end)
        }
    };
    let mut chosen: Vec<Id> = match revision_ids {
        Some(keys) => {
            let mut chosen = Vec::new();
            for key in keys {
                let parsed = parse_id(key);
                let found: Vec<Id> = live
                    .iter()
                    .filter(|r| Some(r.id) == parsed || r.revision.file_id.as_deref() == Some(key))
                    .map(|r| r.id)
                    .collect();
                if found.is_empty() {
                    return Err(IntentError::NoSuchRevision(key.clone()));
                }
                chosen.extend(found);
            }
            chosen
        }
        None => live.iter().map(|r| r.id).collect(),
    };
    if let Some(range) = range {
        chosen.retain(|id| {
            live.iter()
                .any(|r| r.id == *id && r.paragraphs.iter().any(|p| range.contains(p)))
        });
    }
    let mut seen = BTreeSet::new();
    chosen.retain(|id| seen.insert(*id));
    if chosen.is_empty() {
        return Err(IntentError::NoRevisions);
    }
    Ok(chosen)
}

/// `lamport@rN`.
fn parse_id(key: &str) -> Option<Id> {
    key.split_once('@').and_then(|(lamport, replica)| {
        Some(Id::new(
            lamport.parse().ok()?,
            ReplicaId(replica.strip_prefix('r')?.parse().ok()?),
        ))
    })
}

/// Resolves `op` against `view`. `ids(n)` must reserve `n` consecutive fresh
/// identifiers (see `Replica::reserve`). The operations are meant to be
/// submitted in order. Tracked changes, if the view's tracking is on, carry
/// no date and treat no insertion as one's own; see [`resolve_with`].
pub fn resolve(
    view: &DocxModel,
    op: &IntentOp,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Result<Vec<DocxOp>, IntentError> {
    resolve_with(view, op, &Context::default(), ids)
}

/// [`resolve`] on behalf of `context`'s replica, dating tracked changes
/// with `context.date`.
pub fn resolve_with(
    view: &DocxModel,
    op: &IntentOp,
    context: &Context,
    ids: &mut dyn FnMut(u64) -> Id,
) -> Result<Vec<DocxOp>, IntentError> {
    try_resolve_with(view, op, context, &mut |n| Ok(ids(n)))
}

/// Resolves with an allocator that can report identifier exhaustion.
pub fn try_resolve(
    view: &DocxModel,
    op: &IntentOp,
    ids: &mut dyn FnMut(u64) -> Result<Id, IdError>,
) -> Result<Vec<DocxOp>, IntentError> {
    try_resolve_with(view, op, &Context::default(), ids)
}

/// Checked allocation with revision context. No operations are returned if
/// any reservation fails, including a later part of a replacement edit.
pub fn try_resolve_with(
    view: &DocxModel,
    op: &IntentOp,
    context: &Context,
    ids: &mut dyn FnMut(u64) -> Result<Id, IdError>,
) -> Result<Vec<DocxOp>, IntentError> {
    let paragraphs = view.paragraphs();
    // Typing: a tracked insertion when the document tracks changes.
    let typing = |story: StoryId, parent: Option<Id>, first: Id, text: &str| {
        if view.tracking() {
            DocxOp::TrackInsertText {
                story,
                parent,
                first,
                text: text.to_string(),
                date: context.date.clone(),
            }
        } else {
            DocxOp::InsertText {
                story,
                parent,
                first,
                text: text.to_string(),
            }
        }
    };
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
            Ok(vec![typing(
                at.story,
                at.typing,
                ids(text.chars().count() as u64)?,
                text,
            )])
        }
        IntentOp::InsertParagraphBreak {
            paragraph,
            offset,
            count,
        } => {
            let at = caret(&paragraphs, *paragraph, *offset)?;
            let mut parent = at.parent;
            (0..*count)
                .map(|_| {
                    let id = ids(1)?;
                    let op = DocxOp::SplitParagraph {
                        story: at.story,
                        parent,
                        id,
                    };
                    parent = Some(id);
                    Ok(op)
                })
                .collect()
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
                HeadlessDeleteDirection::Forward => {
                    at.index..at.index.saturating_add(*count).min(atoms.len())
                }
                HeadlessDeleteDirection::Backward => at.index.saturating_sub(*count)..at.index,
            };
            deletion(view, at.story, &atoms[range], context, ids)
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
            let mut ops = deletion(
                view,
                start.story,
                &atoms[start.index..end.index],
                context,
                ids,
            )?;
            if !text.is_empty() {
                ops.push(typing(
                    start.story,
                    start.typing,
                    ids(text.chars().count() as u64)?,
                    text,
                ));
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
            // Each paragraph's slice of the range: its characters, and its
            // mark when the range runs past it.
            let slices: Vec<(StoryId, &[Id], Option<Id>)> = (*start_paragraph..=*end_paragraph)
                .map(|n| {
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
                    (
                        p.story,
                        &p.chars[from..to],
                        p.mark.filter(|_| n < *end_paragraph),
                    )
                })
                .collect();
            let mut ops = Vec::new();
            if change.mark_key().is_some() || *change == FormatChange::ClearFormatting {
                // A mark per story, over its first through last character.
                let expand = change.mark_key().map_or(Expand::End, |key| key.expand());
                let chars = slices
                    .iter()
                    .map(|(story, chars, _)| (*story, chars.iter().copied()));
                for (story, first, last) in per_story(chars) {
                    let (start, end) = view
                        .mark_anchors(story, first, last, expand)
                        .expect("a view's characters are in their story, in order");
                    ops.push(DocxOp::Mark {
                        story,
                        id: ids(1)?,
                        start,
                        end,
                        change: change.clone(),
                    });
                }
            }
            if change.mark_key().is_none() {
                // Clearing the file's formatting, or paragraph formatting.
                let atoms = slices
                    .iter()
                    .map(|(story, chars, mark)| (*story, chars.iter().copied().chain(*mark)));
                ops.extend(per_story(atoms).into_iter().map(|(story, first, last)| {
                    DocxOp::Format {
                        story,
                        start: first,
                        end: last,
                        change: change.clone(),
                    }
                }));
            }
            Ok(ops)
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
                id: ids(1)?,
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
            id: ids(1)?,
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
        IntentOp::InsertFootnote {
            paragraph,
            offset,
            text,
        }
        | IntentOp::InsertEndnote {
            paragraph,
            offset,
            text,
        } => {
            let at = caret(&paragraphs, *paragraph, *offset)?;
            let kind = match op {
                IntentOp::InsertFootnote { .. } => NoteKind::Footnote,
                _ => NoteKind::Endnote,
            };
            // The reference, then the note's tab and text.
            let id = ids(2 + text.chars().count() as u64)?;
            Ok(vec![DocxOp::InsertNote {
                kind,
                story: at.story,
                parent: at.parent,
                id,
                text: text.clone(),
            }])
        }
        IntentOp::DeleteFootnote {
            footnote_id: file_id,
        }
        | IntentOp::DeleteEndnote {
            endnote_id: file_id,
        } => {
            let kind = match op {
                IntentOp::DeleteFootnote { .. } => NoteKind::Footnote,
                _ => NoteKind::Endnote,
            };
            let note = view
                .note_by_file_id(kind, file_id)
                .ok_or_else(|| IntentError::NoSuchNote(file_id.clone()))?;
            Ok(vec![DocxOp::DeleteNote { kind, note }])
        }
        IntentOp::InsertTextBox {
            paragraph,
            offset,
            text,
            width_px,
            height_px,
        } => {
            let at = caret(&paragraphs, *paragraph, *offset)?;
            Ok(vec![DocxOp::InsertTextBox {
                story: at.story,
                parent: at.parent,
                id: ids(1 + text.chars().count() as u64)?,
                text: text.clone(),
                width_px: width_px.unwrap_or(216),
                height_px: height_px.unwrap_or(96),
            }])
        }
        IntentOp::DeleteTextBox {
            paragraph,
            text_box,
        } => {
            let missing = IntentError::NoSuchTextBox {
                paragraph: *paragraph,
                text_box: *text_box,
            };
            let p = paragraphs.get(*paragraph).ok_or(missing.clone())?;
            let id = p
                .anchors
                .iter()
                .filter_map(|a| match a.story {
                    StoryId::TextBox { id } => Some(id),
                    _ => None,
                })
                .nth(*text_box)
                .ok_or(missing)?;
            Ok(vec![DocxOp::DeleteTextBox { text_box: id }])
        }
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
            let height = bottom_row
                .checked_sub(*top_row)
                .and_then(|n| n.checked_add(1))
                .ok_or(IntentError::InvertedRange)?;
            let width = right_column
                .checked_sub(*left_column)
                .and_then(|n| n.checked_add(1))
                .ok_or(IntentError::InvertedRange)?;
            crate::table_budget::check(height, width)?;
            // One new story per follower: the cells below the top-left one.
            let followers = bottom_row.saturating_sub(*top_row) as u64;
            Ok(vec![DocxOp::MergeCells {
                table: t.id,
                merge,
                first: if followers == 0 {
                    Id::new(0, ReplicaId::ORIGIN)
                } else {
                    ids(followers)?
                },
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
            let height = bottom - top + 1;
            let width = right - left + 1;
            crate::table_budget::check(height, width)?;
            let freed = (height * (width - 1)) as u64;
            Ok(vec![DocxOp::UnmergeCells {
                table: t.id,
                top: t.rows[top],
                left: t.columns[left],
                first: if freed == 0 {
                    Id::new(0, ReplicaId::ORIGIN)
                } else {
                    ids(freed)?
                },
            }])
        }
        IntentOp::SetTracking { on } => Ok(vec![DocxOp::SetTracking { on: *on }]),
        IntentOp::AcceptRevisions {
            revision_ids,
            start_paragraph,
            end_paragraph,
        } => Ok(vec![DocxOp::AcceptRevisions {
            revisions: chosen_revisions(view, revision_ids, *start_paragraph, *end_paragraph)?,
        }]),
        IntentOp::RejectRevisions {
            revision_ids,
            start_paragraph,
            end_paragraph,
        } => Ok(vec![DocxOp::RejectRevisions {
            revisions: chosen_revisions(view, revision_ids, *start_paragraph, *end_paragraph)?,
        }]),
    }
}

/// `time` as Word dates a tracked change (`w:date`, UTC to the second):
/// `2026-10-18T09:30:00Z`.
pub fn word_date(time: std::time::SystemTime) -> String {
    let seconds = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Howard Hinnant's civil_from_days, for days since 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// The kinds of revisions, for callers listing them.
pub fn revision_kind_name(kind: RevisionKind) -> &'static str {
    match kind {
        RevisionKind::Insertion => "insertion",
        RevisionKind::Deletion => "deletion",
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
    let parsed = parse_id(key);
    view.comments()
        .into_iter()
        .find(|c| Some(c.id) == parsed || c.file_id.as_deref() == Some(key))
        .map(|c| c.id)
        .ok_or_else(|| IntentError::NoSuchComment(key.to_string()))
}
