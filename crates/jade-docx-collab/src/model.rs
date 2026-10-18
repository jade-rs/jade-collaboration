//! The collaborative DOCX model.
//!
//! A *story* — the body, one table cell, one footnote or endnote, one text
//! box — is an RGA of characters and paragraph marks; a paragraph is the run
//! of characters ending at a visible mark, and a story's last paragraph has
//! no mark (Word's final paragraph mark cannot be deleted). Tables are placed
//! *after* a mark (or at the start or end of their story), so a concurrent
//! split can never put a table inside a paragraph. A note reference or a
//! text box is a zero-width *anchor* atom in its host paragraph's RGA: text
//! deletion never removes it, and splits and joins move it with the text
//! around it, as jade moves the run. Walking the model in order gives the
//! same paragraphs, in the same order, as jade's paragraph surfaces (minus
//! equations and note separators), each with jade's story position — which
//! is how a model position becomes a jade paragraph.

use std::collections::{BTreeMap, BTreeSet};

use jade_collab_core::grid::{Grid, GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::rga::Rga;
use jade_collab_sync::{ApplyContext, Document, Lane, Rejection};
pub use jade_docx_document_core::word_document::ParagraphStory;
use serde::{Deserialize, Serialize};

/// Which story an operation edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoryId {
    Body,
    /// The story a table cell was created with. It keeps its identity when
    /// a merge shows it inside another cell.
    Cell {
        table: Id,
        row: Id,
        column: Id,
    },
    /// A story an unmerge created for a cell it freed.
    Split {
        table: Id,
        id: Id,
    },
    /// A footnote's text, by the id the model gave the note on load.
    Footnote {
        id: Id,
    },
    /// An endnote's text, by the id the model gave the note on load.
    Endnote {
        id: Id,
    },
    /// A text box's text, by the id the model gave the box on load.
    TextBox {
        id: Id,
    },
}

/// Where a block sits in its story.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "mark", rename_all = "snake_case")]
pub enum Anchor {
    /// Before the story's first paragraph.
    Start,
    /// Right after the paragraph that ends with this mark.
    After(Id),
    /// After the story's last paragraph (only found in loaded files).
    End,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Atom {
    Char(char),
    Mark,
    /// A zero-width run placing another story: a note reference or a text
    /// box. Text deletion never removes it.
    Anchor(Placement),
}

/// What an anchor places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Placement {
    Footnote(Id),
    Endnote(Id),
    TextBox(Id),
}

impl Placement {
    pub fn story(self) -> StoryId {
        match self {
            Self::Footnote(id) => StoryId::Footnote { id },
            Self::Endnote(id) => StoryId::Endnote { id },
            Self::TextBox(id) => StoryId::TextBox { id },
        }
    }
}

/// A footnote or endnote: its `w:id` and its text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Note {
    /// Empty for a note made in the session until the server says which
    /// number jade gave it ([`DocxOp::Numbered`]).
    pub(crate) file_id: String,
    pub(crate) story: Story,
    /// Deleted notes stay, invisible, so a concurrent edit of their text
    /// still applies.
    #[serde(default)]
    pub(crate) deleted: bool,
}

/// Which kind of note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    Footnote,
    Endnote,
}

impl NoteKind {
    fn placement(self, id: Id) -> Placement {
        match self {
            Self::Footnote => Placement::Footnote(id),
            Self::Endnote => Placement::Endnote(id),
        }
    }

    pub(crate) fn numbered(self, id: Id) -> Numbered {
        match self {
            Self::Footnote => Numbered::Footnote { id },
            Self::Endnote => Numbered::Endnote { id },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Story {
    pub(crate) flow: Rga<Atom>,
    pub(crate) blocks: Vec<Placed>,
    /// False for a story jade shows no paragraph for (a cell holding only a
    /// table, an empty text box, a text box inside a text box); such a story
    /// is read-only.
    pub(crate) has_paragraph: bool,
}

impl Story {
    pub(crate) fn empty_paragraph() -> Self {
        Self {
            flow: Rga::new(),
            blocks: Vec::new(),
            has_paragraph: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Placed {
    pub(crate) anchor: Anchor,
    pub(crate) block: Block,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Block {
    Table(Id),
    /// Content the model does not edit (equations): it only blocks joins.
    Opaque,
}

/// A table's story, by identity: the one a cell was created with, or one an
/// unmerge created.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum StoryKey {
    Cell(Id, Id),
    Split(Id),
}

/// A table: its grid (rows, columns, merges), every story it ever held, and
/// which stories each cell position shows.
///
/// Stories never move: a merge changes which position shows them, not
/// where they live, so text typed into a cell's story commutes with merging
/// and unmerging (Lean: `CellCommute.cell_edit_comm_grid`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Table {
    pub(crate) grid: Grid,
    #[serde(with = "pairs")]
    pub(crate) stories: BTreeMap<StoryKey, Story>,
    /// The stories each cell position shows, in order — jade's `w:tc` at that
    /// position. A position a horizontal merge covers has no entry (jade has
    /// no cell there); a vertical merge's follower shows nothing (jade
    /// cleared it) or, loaded from a file, what the file had there.
    #[serde(with = "pairs")]
    pub(crate) layout: BTreeMap<(Id, Id), Vec<StoryKey>>,
    /// Ragged or nested tables, or ones not at the top level: cell text is
    /// editable, structure is not.
    pub(crate) locked: bool,
    pub(crate) deleted: bool,
    pub(crate) top_level: bool,
}

impl Table {
    /// A grid of single cells, each showing its own new story.
    pub(crate) fn new(rows: &[Id], columns: &[Id], locked: bool, top_level: bool) -> Self {
        let mut table = Self {
            grid: Grid::new(rows.iter().copied(), columns.iter().copied()),
            stories: BTreeMap::new(),
            layout: BTreeMap::new(),
            locked,
            deleted: false,
            top_level,
        };
        for row in rows {
            for column in columns {
                table.add_cell(*row, *column, Story::empty_paragraph());
            }
        }
        table
    }

    pub(crate) fn add_cell(&mut self, row: Id, column: Id, story: Story) {
        let key = StoryKey::Cell(row, column);
        self.stories.insert(key, story);
        self.layout.insert((row, column), vec![key]);
    }

    /// Alive (row, column) positions of `merge`, row by row.
    fn positions(&self, merge: &Merge) -> Vec<(Id, Id)> {
        let between = |lines: Vec<Id>, first: Id, last: Id| {
            let a = lines.iter().position(|l| *l == first);
            let b = lines.iter().position(|l| *l == last);
            match (a, b) {
                (Some(a), Some(b)) if a <= b => lines[a..=b].to_vec(),
                _ => Vec::new(),
            }
        };
        let rows = between(self.grid.rows(), merge.top, merge.bottom);
        let columns = between(self.grid.columns(), merge.left, merge.right);
        rows.iter()
            .flat_map(|r| columns.iter().map(move |c| (*r, *c)))
            .collect()
    }

    /// (top, left, bottom, right) alive indices of every merge.
    fn merge_indices(&self) -> Vec<(usize, usize, usize, usize)> {
        self.grid
            .merges()
            .iter()
            .filter_map(|m| {
                Some((
                    self.grid.row_index(m.top)?,
                    self.grid.column_index(m.left)?,
                    self.grid.row_index(m.bottom)?,
                    self.grid.column_index(m.right)?,
                ))
            })
            .collect()
    }
}

fn story_id(table: Id, key: StoryKey) -> StoryId {
    match key {
        StoryKey::Cell(row, column) => StoryId::Cell { table, row, column },
        StoryKey::Split(id) => StoryId::Split { table, id },
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocxModel {
    pub(crate) body: Story,
    #[serde(with = "pairs")]
    pub(crate) tables: BTreeMap<Id, Table>,
    #[serde(default, with = "pairs")]
    pub(crate) footnotes: BTreeMap<Id, Note>,
    #[serde(default, with = "pairs")]
    pub(crate) endnotes: BTreeMap<Id, Note>,
    #[serde(default, with = "pairs")]
    pub(crate) text_boxes: BTreeMap<Id, Story>,
    /// Every comment ever made or loaded, deleted ones included, so later
    /// operations find it by its id.
    #[serde(default, with = "pairs")]
    pub(crate) comments: BTreeMap<Id, Comment>,
    pub(crate) max_lamport: u64,
}

/// A comment's text and who wrote it, as jade's comment commands take them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentContent {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initials: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

impl CommentContent {
    /// Text with no author, initials or date.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            author: None,
            initials: None,
            date: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Comment {
    /// The thread's root; `None` for a root.
    pub(crate) parent: Option<Id>,
    /// What a comment made in the session was anchored to; `None` for a reply
    /// (it shares its root's anchor) and for a comment loaded from the file.
    pub(crate) anchor: Option<(StoryId, Id, Id)>,
    pub(crate) content: CommentContent,
    pub(crate) resolved: bool,
    pub(crate) deleted: bool,
    /// jade's `w:id`: the file's for a comment loaded from it, the one jade
    /// gave for a comment made in the session (once the server says so).
    pub(crate) file_id: Option<String>,
}

/// A live comment as the model knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentView {
    pub id: Id,
    /// The root of its thread; `None` for a root.
    pub parent: Option<Id>,
    /// jade's `w:id`: the file's, or for a comment made in the session the
    /// number jade gave it (`None` until the server's derived
    /// [`DocxOp::Numbered`] arrives).
    pub file_id: Option<String>,
    /// (story, first, last character) for a root made in the session.
    pub anchor: Option<(StoryId, Id, Id)>,
    pub content: CommentContent,
    pub resolved: bool,
}

/// Horizontal paragraph alignment, as Word's ribbon offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParagraphAlignment {
    Left,
    Center,
    Right,
    #[serde(alias = "both")]
    Justify,
    Distribute,
}

impl ParagraphAlignment {
    /// The value jade's `set_paragraph_alignment` takes and its paragraph
    /// query reports.
    pub fn as_jade(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
            Self::Justify => "justify",
            Self::Distribute => "distribute",
        }
    }

    /// Parses jade's alignment names (`both` is `justify`), ignoring case.
    pub fn from_jade(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "left" => Self::Left,
            "center" => Self::Center,
            "right" => Self::Right,
            "justify" | "both" => Self::Justify,
            "distribute" => Self::Distribute,
            _ => return None,
        })
    }
}

/// What a `Format` operation does to its range. Character changes apply to
/// the range's visible characters; paragraph changes ([`Self::is_paragraph_level`])
/// to every paragraph holding a visible character or mark of it. The model
/// only validates the value — jade does the formatting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FormatChange {
    Bold {
        value: bool,
    },
    Italic {
        value: bool,
    },
    Underline {
        value: bool,
        /// OOXML `w:u@w:val` (`double`, `wave`, …); `None` is single.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<String>,
    },
    Strike {
        value: bool,
        /// Word's double strikethrough (`w:dstrike`).
        #[serde(default)]
        double: bool,
    },
    /// In half points, as Word stores sizes (`21` is 10.5pt).
    FontSize {
        half_points: u32,
    },
    FontFamily {
        family: String,
    },
    /// `#RRGGBB`.
    FontColor {
        color: String,
    },
    /// One of Word's named highlight colors (`yellow`, `darkBlue`, … `none`).
    Highlight {
        color: String,
    },
    /// Word's Ctrl+Space: direct character formatting off.
    ClearFormatting,
    Alignment {
        alignment: ParagraphAlignment,
    },
    /// `None` returns to the document's default paragraph style.
    ParagraphStyle {
        #[serde(default)]
        style_id: Option<String>,
    },
}

/// Word's limits: 1pt to 1638pt.
const FONT_HALF_POINTS: std::ops::RangeInclusive<u32> = 2..=3276;

/// `ST_HighlightColor`.
const HIGHLIGHT_COLORS: &[&str] = &[
    "black",
    "blue",
    "cyan",
    "green",
    "magenta",
    "red",
    "yellow",
    "white",
    "darkBlue",
    "darkCyan",
    "darkGreen",
    "darkMagenta",
    "darkRed",
    "darkYellow",
    "darkGray",
    "lightGray",
    "none",
];

/// `ST_Underline`.
const UNDERLINE_STYLES: &[&str] = &[
    "single",
    "words",
    "double",
    "thick",
    "dotted",
    "dottedHeavy",
    "dash",
    "dashedHeavy",
    "dashLong",
    "dashLongHeavy",
    "dotDash",
    "dashDotHeavy",
    "dotDotDash",
    "dashDotDotHeavy",
    "wave",
    "wavyHeavy",
    "wavyDouble",
    "none",
];

impl FormatChange {
    /// Whether the change applies to whole paragraphs.
    pub fn is_paragraph_level(&self) -> bool {
        matches!(self, Self::Alignment { .. } | Self::ParagraphStyle { .. })
    }

    /// Refuses values jade would write into the file verbatim but Word
    /// cannot read.
    pub fn validate(&self) -> Result<(), String> {
        let ok = match self {
            Self::Underline {
                style: Some(style), ..
            } => UNDERLINE_STYLES.contains(&style.as_str()),
            Self::FontSize { half_points } => FONT_HALF_POINTS.contains(half_points),
            Self::FontFamily { family } => !family.trim().is_empty(),
            Self::FontColor { color } => {
                color.len() == 7
                    && color.starts_with('#')
                    && color[1..].chars().all(|c| c.is_ascii_hexdigit())
            }
            Self::Highlight { color } => HIGHLIGHT_COLORS.contains(&color.as_str()),
            Self::ParagraphStyle {
                style_id: Some(style),
            } => !style.trim().is_empty(),
            _ => true,
        };
        if ok {
            Ok(())
        } else {
            Err(format!("{self:?} is not a valid formatting change"))
        }
    }
}

/// One paragraph as the model walks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    pub story: StoryId,
    /// The mark that ends it; `None` for its story's last paragraph.
    pub mark: Option<Id>,
    pub text: String,
    /// The visible characters, in order (one per `char` of `text`).
    pub chars: Vec<Id>,
    /// Where jade finds it.
    pub at: ParagraphStory,
    /// Its text boxes and note references, in order.
    pub anchors: Vec<AnchorView>,
}

/// A text box or note reference in a paragraph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorView {
    /// The anchor atom.
    pub id: Id,
    /// UTF-8 bytes of the paragraph's text before it.
    pub offset: usize,
    /// The story it places.
    pub story: StoryId,
}

impl Paragraph {
    fn new(story: StoryId) -> Self {
        Self {
            story,
            mark: None,
            text: String::new(),
            chars: Vec::new(),
            at: ParagraphStory::Body { index: 0 },
            anchors: Vec::new(),
        }
    }

    /// How many anchors come before a character at byte `offset` (or, past
    /// the text, before the paragraph's end).
    fn anchors_before(&self, offset: usize) -> usize {
        self.anchors.iter().filter(|a| a.offset <= offset).count()
    }
}

/// A top-level table as the model walks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableView {
    pub id: Id,
    pub rows: Vec<Id>,
    pub columns: Vec<Id>,
    pub locked: bool,
    /// Merged regions as (top, left, bottom, right) indices into `rows` and
    /// `columns`.
    pub merges: Vec<(usize, usize, usize, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocxOp {
    /// Convergent. Inserts `text` as a run after `parent` (`None`: story
    /// start); its characters get ids `first`, `first.offset(1)`, ….
    InsertText {
        story: StoryId,
        parent: Option<Id>,
        first: Id,
        text: String,
    },
    /// Convergent. Deletes characters (never marks or anchors); idempotent.
    DeleteText { story: StoryId, ids: Vec<Id> },
    /// Convergent. Inserts a paragraph mark after `parent`. Refused in a
    /// text box, whose paragraphs are fixed.
    SplitParagraph {
        story: StoryId,
        parent: Option<Id>,
        id: Id,
    },
    /// Sequenced. Deletes `mark`, joining its paragraph with the next one.
    /// Refused while a table, or a text box or footnote the paragraph
    /// places, follows the paragraph, and in a text box.
    JoinParagraph { story: StoryId, mark: Id },
    /// Sequenced. A new empty table in the body.
    InsertTable {
        anchor: Anchor,
        id: Id,
        rows: Vec<Id>,
        columns: Vec<Id>,
    },
    /// Sequenced. Row and column structure of a top-level table.
    Table { table: Id, op: GridOp },
    /// Sequenced. Merges a rectangle of cells as Word and jade do: the
    /// top-left cell shows every covered cell's content, row by row; each
    /// cell below it in its column becomes an empty follower with a new
    /// story `first`, `first.offset(1)`, … (top to bottom).
    MergeCells { table: Id, merge: Merge, first: Id },
    /// Sequenced. Undoes the merge whose top-left cell is (`top`, `left`); the
    /// cells it frees get new empty stories `first`, `first.offset(1)`, …
    /// (row by row, skipping the merge's first column).
    UnmergeCells {
        table: Id,
        top: Id,
        left: Id,
        first: Id,
    },
    /// Sequenced.
    DeleteTable { table: Id },
    /// Sequenced. Formats the atoms from `start` through `end` (characters,
    /// or paragraph marks to reach an empty paragraph) of one story. The
    /// range is resolved when the operation is applied: characters deleted
    /// before it shrink the range, characters inserted inside it are
    /// covered, and with nothing visible left it does nothing.
    Format {
        story: StoryId,
        start: Id,
        end: Id,
        change: FormatChange,
    },
    /// Sequenced. A comment on the visible characters from `start` through
    /// `end`; refused when none are left.
    AddComment {
        story: StoryId,
        start: Id,
        end: Id,
        id: Id,
        content: CommentContent,
    },
    /// Sequenced. Answers `comment`'s thread (a reply to a reply answers the
    /// root, as Word keeps threads flat).
    ReplyToComment {
        comment: Id,
        id: Id,
        content: CommentContent,
    },
    /// Sequenced.
    ResolveComment { comment: Id, resolved: bool },
    /// Sequenced. Deleting a root deletes its replies.
    DeleteComment { comment: Id },
    /// Sequenced. Replaces a live comment's text (root or reply); the last
    /// replacement in log order wins.
    UpdateComment { comment: Id, text: String },
    /// Sequenced. A new footnote or endnote, as jade's `insert_footnote` /
    /// `insert_endnote` makes one: its reference is an anchor atom `id`
    /// placed after `parent` in `story` (the body or a table cell), and the
    /// note is a new story keyed by `id` holding one paragraph with a tab
    /// and `text` (what jade writes into a new note), its characters
    /// `id.offset(1)`, `id.offset(2)`, …. Refused where jade cannot put the
    /// reference (in a text box or a note) and where a text box or note
    /// reference already sits: jade types only before or after all the
    /// anchors at an offset, and concurrent typing at the caret could land
    /// between the new reference and the old one.
    InsertNote {
        kind: NoteKind,
        story: StoryId,
        parent: Option<Id>,
        id: Id,
        #[serde(default)]
        text: String,
    },
    /// Sequenced. Deletes a footnote or endnote: every reference to it and
    /// its story.
    DeleteNote { kind: NoteKind, note: Id },
    /// Sequenced, derived: the number (`w:id`) jade gave a comment or note
    /// made in the session. Only the server commits these, right after the
    /// operation that made it.
    Numbered { item: Numbered, file_id: String },
}

/// Something jade numbers itself when it is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Numbered {
    Comment { id: Id },
    Footnote { id: Id },
    Endnote { id: Id },
}

/// An operation's consequence in jade terms: paragraphs by jade's story
/// position ([`ParagraphStory`], resolved to a paragraph surface when it is
/// applied), UTF-8 byte offsets in model text, and tables by their pre-order
/// index in the body's table structure. `anchors` says how many of the
/// paragraph's text boxes and note references come before the position —
/// what an offset alone cannot say, since they take no text. Emitted in the
/// order to apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocxEffect {
    InsertText {
        paragraph: ParagraphStory,
        offset: usize,
        text: String,
        #[serde(default)]
        anchors: usize,
    },
    /// No anchor lies inside the range; `anchors` come before it.
    DeleteRange {
        paragraph: ParagraphStory,
        start: usize,
        end: usize,
        #[serde(default)]
        anchors: usize,
    },
    SplitParagraph {
        paragraph: ParagraphStory,
        offset: usize,
        #[serde(default)]
        anchors: usize,
    },
    /// Join `paragraph` (whose text is `offset` bytes long) with the next
    /// paragraph of its story.
    JoinParagraph {
        paragraph: ParagraphStory,
        offset: usize,
    },
    /// A table after `paragraph`, or above it when `above`.
    InsertTable {
        paragraph: ParagraphStory,
        above: bool,
        rows: usize,
        columns: usize,
    },
    InsertTableRow {
        table: usize,
        row: usize,
        template: Option<usize>,
    },
    DeleteTableRow {
        table: usize,
        row: usize,
    },
    InsertTableColumn {
        table: usize,
        column: usize,
        template: Option<usize>,
    },
    DeleteTableColumn {
        table: usize,
        column: usize,
    },
    /// Merge the cells at these (row, column) indices into the first one.
    MergeTableCells {
        table: usize,
        cells: Vec<(usize, usize)>,
    },
    UnmergeTableCells {
        table: usize,
        row: usize,
        column: usize,
        column_span: usize,
    },
    DeleteTable {
        table: usize,
    },
    /// A character change on `start..end` of one paragraph.
    FormatText {
        paragraph: ParagraphStory,
        start: usize,
        end: usize,
        change: FormatChange,
    },
    /// A paragraph change on one paragraph.
    FormatParagraph {
        paragraph: ParagraphStory,
        change: FormatChange,
    },
    /// A new comment `comment` from (`start_paragraph`, `start`) to
    /// (`end_paragraph`, `end`).
    AddComment {
        comment: Id,
        start_paragraph: ParagraphStory,
        start: usize,
        end_paragraph: ParagraphStory,
        end: usize,
        content: CommentContent,
    },
    /// Comments are named by model id; the materializer knows jade's.
    ReplyToComment {
        comment: Id,
        reply: Id,
        content: CommentContent,
    },
    ResolveComment {
        comment: Id,
        resolved: bool,
    },
    DeleteComment {
        comment: Id,
    },
    UpdateComment {
        comment: Id,
        text: String,
    },
    /// A new note `note` at `offset` of `paragraph`, after `anchors` of its
    /// text boxes and note references (all those before the offset; none
    /// sits at it).
    InsertNote {
        kind: NoteKind,
        note: Id,
        paragraph: ParagraphStory,
        offset: usize,
        anchors: usize,
        text: String,
    },
    /// Notes are named by model id; the model knows jade's number.
    DeleteNote {
        kind: NoteKind,
        note: Id,
    },
    /// jade gave `item` the number `file_id` (already done in jade).
    Numbered {
        item: Numbered,
        file_id: String,
    },
}

#[derive(Default)]
struct Walk {
    paragraphs: Vec<Paragraph>,
    tables: Vec<TableView>,
    /// Every visible table of the body, nested ones included, in pre-order.
    all_tables: Vec<Id>,
    /// The next body paragraph's index.
    body: usize,
    footnotes_listed: BTreeSet<Id>,
    /// Endnotes in order of first reference from the body or a text box.
    endnotes_referenced: Vec<Id>,
}

/// The story whose paragraphs share one numbering in jade's story
/// positions: the body (table cells included), a text box (by its host body
/// paragraph and its index among the host's text boxes) or a note.
#[derive(Clone, Copy, Debug)]
enum Root {
    Body,
    TextBox { host: usize, index: usize },
    Footnote(Id),
    Endnote(Id),
}

/// Where a visible atom is.
#[derive(Clone, Copy, Debug)]
struct Located {
    ordinal: usize,
    /// UTF-8 bytes of its paragraph before it (a mark: the paragraph's length).
    offset: usize,
    /// Its own length (0 for a mark).
    len: usize,
    /// Anchors of its paragraph before it.
    anchors: usize,
}

fn reject(message: impl Into<String>) -> Rejection {
    Rejection::new(message)
}

/// jade does not write a text box's new or removed paragraphs to the file
/// (it keeps them in memory only), so a text box's paragraphs are fixed.
fn fixed_paragraphs(story: StoryId) -> Result<(), Rejection> {
    match story {
        StoryId::TextBox { .. } => Err(reject(
            "a text box's paragraphs cannot be split or joined: jade does not write them to the file",
        )),
        _ => Ok(()),
    }
}

/// Refuses comment text jade would not read back from the file as written:
/// it keeps the text as typed while the file is open, but reads line breaks
/// back as spaces and trims the ends.
fn comment_text(text: &str) -> Result<(), Rejection> {
    if text.trim().is_empty() {
        return Err(reject("a comment needs text"));
    }
    if text.trim() != text {
        return Err(reject(
            "a comment's text cannot start or end with whitespace: jade does not keep it",
        ));
    }
    if text.chars().any(|c| c.is_control() && c != '\t') {
        return Err(reject(
            "a comment's text cannot hold line breaks or control characters: jade does not keep them",
        ));
    }
    Ok(())
}

impl DocxModel {
    /// A body-only document, one paragraph per entry; ids belong to
    /// [`ReplicaId::ORIGIN`].
    pub fn from_paragraphs(paragraphs: &[&str]) -> Self {
        let mut next = 1u64;
        let mut flow = Rga::new();
        let mut previous = None;
        for (i, text) in paragraphs.iter().enumerate() {
            let chars: Vec<Atom> = text.chars().map(Atom::Char).collect();
            if !chars.is_empty() {
                let first = Id::new(next, ReplicaId::ORIGIN);
                let count = chars.len() as u64;
                flow.insert_run(first, previous, chars).expect("fresh ids");
                next += count;
                previous = Some(first.offset(count - 1));
            }
            if i + 1 < paragraphs.len() {
                let mark = Id::new(next, ReplicaId::ORIGIN);
                flow.insert(mark, previous, Atom::Mark).expect("fresh id");
                next += 1;
                previous = Some(mark);
            }
        }
        Self {
            body: Story {
                flow,
                blocks: Vec::new(),
                has_paragraph: true,
            },
            tables: BTreeMap::new(),
            footnotes: BTreeMap::new(),
            endnotes: BTreeMap::new(),
            text_boxes: BTreeMap::new(),
            comments: BTreeMap::new(),
            max_lamport: next - 1,
        }
    }

    fn walk(&self) -> Walk {
        let mut out = Walk::default();
        self.walk_story(Root::Body, StoryId::Body, &self.body, &mut out, &mut 0);
        // Footnotes no body paragraph referenced, then endnotes in order of
        // first reference, then the others — by w:id, as jade lists them.
        let by_file_id = |notes: &BTreeMap<Id, Note>, skip: &dyn Fn(&Id) -> bool| {
            let mut rest: Vec<(&str, Id)> = notes
                .iter()
                .filter(|(id, note)| !note.deleted && !skip(id))
                .map(|(id, note)| (note.file_id.as_str(), *id))
                .collect();
            rest.sort();
            rest.into_iter().map(|(_, id)| id).collect::<Vec<Id>>()
        };
        for id in by_file_id(&self.footnotes, &|id| out.footnotes_listed.contains(id)) {
            let story = &self.footnotes[&id].story;
            self.walk_story(
                Root::Footnote(id),
                StoryId::Footnote { id },
                story,
                &mut out,
                &mut 0,
            );
        }
        let mut endnotes = std::mem::take(&mut out.endnotes_referenced);
        endnotes.extend(by_file_id(&self.endnotes, &|id| endnotes.contains(id)));
        for id in endnotes {
            if let Some(note) = self.endnotes.get(&id).filter(|n| !n.deleted) {
                self.walk_story(
                    Root::Endnote(id),
                    StoryId::Endnote { id },
                    &note.story,
                    &mut out,
                    &mut 0,
                );
            }
        }
        out
    }

    /// Walks one story of `root`; `counter` numbers the root's paragraphs
    /// (the body uses `out.body`).
    fn walk_story(
        &self,
        root: Root,
        id: StoryId,
        story: &Story,
        out: &mut Walk,
        counter: &mut usize,
    ) {
        self.walk_blocks(root, id, story, Anchor::Start, out, counter);
        let mut current = Paragraph::new(id);
        for node in story.flow.iter().filter(|n| !n.deleted) {
            match node.value {
                Atom::Char(c) => {
                    current.text.push(c);
                    current.chars.push(node.id);
                }
                Atom::Anchor(placement) => current.anchors.push(AnchorView {
                    id: node.id,
                    offset: current.text.len(),
                    story: placement.story(),
                }),
                Atom::Mark => {
                    current.mark = Some(node.id);
                    let next = Paragraph::new(id);
                    self.emit(root, std::mem::replace(&mut current, next), out, counter);
                    self.walk_blocks(root, id, story, Anchor::After(node.id), out, counter);
                }
            }
        }
        if story.has_paragraph {
            self.emit(root, current, out, counter);
        }
        self.walk_blocks(root, id, story, Anchor::End, out, counter);
    }

    /// Lists `paragraph`, then — as jade does after a body paragraph — the
    /// text boxes it anchors and the footnotes it references first.
    fn emit(&self, root: Root, mut paragraph: Paragraph, out: &mut Walk, counter: &mut usize) {
        let mut next = || {
            *counter += 1;
            *counter - 1
        };
        paragraph.at = match root {
            Root::Body => {
                out.body += 1;
                ParagraphStory::Body {
                    index: out.body - 1,
                }
            }
            Root::TextBox { host, index } => ParagraphStory::TextBox {
                host,
                text_box: index,
                paragraph: next(),
            },
            Root::Footnote(id) => ParagraphStory::Footnote {
                note_id: self.footnotes[&id].file_id.clone(),
                paragraph: next(),
            },
            Root::Endnote(id) => ParagraphStory::Endnote {
                note_id: self.endnotes[&id].file_id.clone(),
                paragraph: next(),
            },
        };
        let host = match paragraph.at {
            ParagraphStory::Body { index } => Some(index),
            _ => None,
        };
        let anchors = paragraph.anchors.clone();
        out.paragraphs.push(paragraph);
        if matches!(root, Root::Body | Root::TextBox { .. }) {
            for anchor in &anchors {
                if let StoryId::Endnote { id } = anchor.story {
                    if !out.endnotes_referenced.contains(&id) {
                        out.endnotes_referenced.push(id);
                    }
                }
            }
        }
        let Some(host) = host else { return };
        let boxes = anchors.iter().filter_map(|a| match a.story {
            StoryId::TextBox { id } => Some(id),
            _ => None,
        });
        for (index, id) in boxes.enumerate() {
            if let Some(story) = self.text_boxes.get(&id) {
                let root = Root::TextBox { host, index };
                self.walk_story(root, StoryId::TextBox { id }, story, out, &mut 0);
            }
        }
        for anchor in &anchors {
            if let StoryId::Footnote { id } = anchor.story {
                if out.footnotes_listed.insert(id) {
                    if let Some(note) = self.footnotes.get(&id).filter(|n| !n.deleted) {
                        let root = Root::Footnote(id);
                        self.walk_story(root, anchor.story, &note.story, out, &mut 0);
                    }
                }
            }
        }
    }

    fn walk_blocks(
        &self,
        root: Root,
        id: StoryId,
        story: &Story,
        anchor: Anchor,
        out: &mut Walk,
        counter: &mut usize,
    ) {
        for placed in story.blocks.iter().filter(|p| p.anchor == anchor) {
            let Block::Table(table_id) = placed.block else {
                continue;
            };
            let table = &self.tables[&table_id];
            if table.deleted {
                continue;
            }
            let rows = table.grid.rows();
            let columns = table.grid.columns();
            if matches!(root, Root::Body) {
                out.all_tables.push(table_id);
            }
            if id == StoryId::Body {
                out.tables.push(TableView {
                    id: table_id,
                    rows: rows.clone(),
                    columns: columns.clone(),
                    locked: table.locked,
                    merges: table.merge_indices(),
                });
            }
            for &row in &rows {
                for &column in &columns {
                    for key in table.layout.get(&(row, column)).into_iter().flatten() {
                        let cell = story_id(table_id, *key);
                        self.walk_story(root, cell, &table.stories[key], out, counter);
                    }
                }
            }
        }
    }

    /// Every visible paragraph in jade's paragraph-surface order (equations
    /// and note separators left out): the body's, a table's cells row by
    /// row, a text box's right after the paragraph that anchors it, a
    /// footnote's right after the body paragraph that first references it,
    /// then the other footnotes and the endnotes (by first reference, then
    /// by `w:id`). Intent paragraph indices count in this order.
    pub fn paragraphs(&self) -> Vec<Paragraph> {
        self.walk().paragraphs
    }

    pub fn paragraph_texts(&self) -> Vec<String> {
        self.paragraphs().into_iter().map(|p| p.text).collect()
    }

    /// Top-level tables in order; the index is jade's `table_order_index`.
    pub fn tables(&self) -> Vec<TableView> {
        self.walk().tables
    }

    pub(crate) fn story(&self, id: StoryId) -> Option<&Story> {
        match id {
            StoryId::Body => Some(&self.body),
            StoryId::Cell { table, row, column } => self
                .tables
                .get(&table)?
                .stories
                .get(&StoryKey::Cell(row, column)),
            StoryId::Split { table, id } => {
                self.tables.get(&table)?.stories.get(&StoryKey::Split(id))
            }
            StoryId::Footnote { id } => self.footnotes.get(&id).map(|n| &n.story),
            StoryId::Endnote { id } => self.endnotes.get(&id).map(|n| &n.story),
            StoryId::TextBox { id } => self.text_boxes.get(&id),
        }
    }

    /// The `w:id` of a footnote's or endnote's story.
    pub fn note_id(&self, story: StoryId) -> Option<&str> {
        match story {
            StoryId::Footnote { id } => self.footnotes.get(&id),
            StoryId::Endnote { id } => self.endnotes.get(&id),
            _ => None,
        }
        .map(|note| note.file_id.as_str())
    }

    /// The live note of `kind` jade numbers `file_id`.
    pub fn note_by_file_id(&self, kind: NoteKind, file_id: &str) -> Option<Id> {
        self.notes(kind)
            .iter()
            .find(|(_, note)| !note.deleted && note.file_id == file_id)
            .map(|(id, _)| *id)
    }

    /// The table cell position (table, row, column) whose `w:tc` shows
    /// `story`: its own, or the merged cell it is shown in.
    pub fn cell_of(&self, story: StoryId) -> Option<(Id, Id, Id)> {
        let (table, key) = match story {
            StoryId::Body
            | StoryId::Footnote { .. }
            | StoryId::Endnote { .. }
            | StoryId::TextBox { .. } => return None,
            StoryId::Cell { table, row, column } => (table, StoryKey::Cell(row, column)),
            StoryId::Split { table, id } => (table, StoryKey::Split(id)),
        };
        self.tables
            .get(&table)?
            .layout
            .iter()
            .find(|(_, keys)| keys.contains(&key))
            .map(|((row, column), _)| (table, *row, *column))
    }

    fn story_mut(&mut self, id: StoryId) -> Option<&mut Story> {
        match id {
            StoryId::Body => Some(&mut self.body),
            StoryId::Cell { table, row, column } => self
                .tables
                .get_mut(&table)?
                .stories
                .get_mut(&StoryKey::Cell(row, column)),
            StoryId::Split { table, id } => self
                .tables
                .get_mut(&table)?
                .stories
                .get_mut(&StoryKey::Split(id)),
            StoryId::Footnote { id } => self.footnotes.get_mut(&id).map(|n| &mut n.story),
            StoryId::Endnote { id } => self.endnotes.get_mut(&id).map(|n| &mut n.story),
            StoryId::TextBox { id } => self.text_boxes.get_mut(&id),
        }
    }

    fn editable_story(&self, id: StoryId) -> Result<&Story, Rejection> {
        let story = self
            .story(id)
            .ok_or_else(|| reject(format!("story {id:?} does not exist")))?;
        if !story.has_paragraph {
            return Err(reject(format!("story {id:?} has no paragraph to edit")));
        }
        Ok(story)
    }

    /// Where each requested visible atom is; a mark reports its paragraph's
    /// end.
    fn locate(&self, ids: &BTreeSet<Id>) -> BTreeMap<Id, Located> {
        let mut found = BTreeMap::new();
        for (ordinal, paragraph) in self.paragraphs().into_iter().enumerate() {
            let mut offset = 0;
            for (c, id) in paragraph.text.chars().zip(&paragraph.chars) {
                if ids.contains(id) {
                    let anchors = paragraph.anchors_before(offset);
                    let len = c.len_utf8();
                    found.insert(
                        *id,
                        Located {
                            ordinal,
                            offset,
                            len,
                            anchors,
                        },
                    );
                }
                offset += c.len_utf8();
            }
            if let Some(mark) = paragraph.mark.filter(|m| ids.contains(m)) {
                let anchors = paragraph.anchors.len();
                found.insert(
                    mark,
                    Located {
                        ordinal,
                        offset,
                        len: 0,
                        anchors,
                    },
                );
            }
        }
        found
    }

    fn locate_one(&self, id: Id) -> Option<Located> {
        self.locate(&BTreeSet::from([id])).get(&id).copied()
    }

    /// jade's story position of the visible paragraph at `ordinal`.
    fn at(&self, ordinal: usize) -> ParagraphStory {
        self.paragraphs()[ordinal].at.clone()
    }

    /// The ordinal of the visible paragraph `mark` ends.
    pub(crate) fn paragraph_ending_with(&self, story: StoryId, mark: Id) -> Option<usize> {
        self.paragraphs()
            .iter()
            .position(|p| p.story == story && p.mark == Some(mark))
    }

    /// The table's index among all visible tables of the body, nested ones
    /// included, in document pre-order — the order of the body's table
    /// structure. The materializer maps it to jade's `table_order_index`
    /// (which also counts tables inside text boxes).
    fn table_order(&self, table: Id) -> Option<usize> {
        self.walk().all_tables.iter().position(|t| *t == table)
    }

    fn observe(&mut self, ids: impl IntoIterator<Item = Id>) {
        for id in ids {
            self.max_lamport = self.max_lamport.max(id.lamport);
        }
    }

    fn insert_text(
        &mut self,
        story: StoryId,
        parent: Option<Id>,
        first: Id,
        text: &str,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if text.is_empty() {
            return Err(reject("nothing to insert"));
        }
        self.editable_story(story)?;
        let count = text.chars().count() as u64;
        self.story_mut(story).expect("checked").flow.insert_run(
            first,
            parent,
            text.chars().map(Atom::Char),
        )?;
        self.observe([first.offset(count - 1)]);
        Ok(self
            .locate_one(first)
            .map(|at| DocxEffect::InsertText {
                paragraph: self.at(at.ordinal),
                offset: at.offset,
                text: text.to_string(),
                anchors: at.anchors,
            })
            .into_iter()
            .collect())
    }

    fn delete_text(&mut self, story: StoryId, ids: &[Id]) -> Result<Vec<DocxEffect>, Rejection> {
        let flow = &self.editable_story(story)?.flow;
        for id in ids {
            match flow.get(*id) {
                None => return Err(reject(format!("character {id} does not exist"))),
                Some(node) if node.value == Atom::Mark => {
                    return Err(reject("paragraph marks are joined, not deleted"))
                }
                Some(node) if matches!(node.value, Atom::Anchor(_)) => {
                    return Err(reject(
                        "text boxes and note references are not deleted as text",
                    ))
                }
                Some(_) => {}
            }
        }
        let positions = self.locate(&ids.iter().copied().collect());
        let flow = &mut self.story_mut(story).expect("checked").flow;
        for id in ids {
            flow.delete(*id)?;
        }
        // Merge adjacent characters into ranges — never across an anchor, so
        // jade gets no range holding a text box's or a reference's run — and
        // apply from the end so earlier offsets stay valid.
        let mut spans: Vec<(usize, usize, usize, usize)> = positions
            .values()
            .map(|at| (at.ordinal, at.offset, at.offset + at.len, at.anchors))
            .collect();
        spans.sort();
        let mut ranges: Vec<(usize, usize, usize, usize)> = Vec::new();
        for (p, start, end, anchors) in spans {
            match ranges.last_mut() {
                Some(last) if last.0 == p && last.2 == start && last.3 == anchors => last.2 = end,
                _ => ranges.push((p, start, end, anchors)),
            }
        }
        let paragraphs = self.paragraphs();
        Ok(ranges
            .into_iter()
            .rev()
            .map(|(p, start, end, anchors)| DocxEffect::DeleteRange {
                paragraph: paragraphs[p].at.clone(),
                start,
                end,
                anchors,
            })
            .collect())
    }

    fn split(
        &mut self,
        story: StoryId,
        parent: Option<Id>,
        id: Id,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        self.editable_story(story)?;
        fixed_paragraphs(story)?;
        self.story_mut(story)
            .expect("checked")
            .flow
            .insert(id, parent, Atom::Mark)?;
        self.observe([id]);
        Ok(self
            .locate_one(id)
            .map(|at| DocxEffect::SplitParagraph {
                paragraph: self.at(at.ordinal),
                offset: at.offset,
                anchors: at.anchors,
            })
            .into_iter()
            .collect())
    }

    fn join(&mut self, story_id: StoryId, mark: Id) -> Result<Vec<DocxEffect>, Rejection> {
        fixed_paragraphs(story_id)?;
        let story = self.editable_story(story_id)?;
        match story.flow.get(mark) {
            Some(node) if node.value == Atom::Mark && !node.deleted => {}
            Some(node) if node.value == Atom::Mark => {
                return Err(reject("the paragraphs are already joined"))
            }
            _ => return Err(reject(format!("{mark} is not a paragraph mark"))),
        }
        let blocked = story.blocks.iter().any(|placed| {
            placed.anchor == Anchor::After(mark)
                && match placed.block {
                    Block::Table(table) => !self.tables[&table].deleted,
                    Block::Opaque => true,
                }
        });
        if blocked {
            return Err(reject(
                "a table separates the paragraphs; they cannot be joined",
            ));
        }
        let paragraphs = self.paragraphs();
        let ordinal = paragraphs
            .iter()
            .position(|p| p.story == story_id && p.mark == Some(mark));
        // jade lists the text boxes a paragraph anchors and the footnotes it
        // references first between it and the next one, and cannot join
        // across them.
        if let Some(p) = ordinal {
            if paragraphs.get(p + 1).map(|q| q.story) != Some(story_id) {
                return Err(reject(
                    "a text box or a note sits between the paragraphs; they cannot be joined",
                ));
            }
        }
        let effect = ordinal.map(|p| DocxEffect::JoinParagraph {
            paragraph: paragraphs[p].at.clone(),
            offset: paragraphs[p].text.len(),
        });
        self.story_mut(story_id)
            .expect("checked")
            .flow
            .delete(mark)?;
        Ok(effect.into_iter().collect())
    }

    fn insert_table(
        &mut self,
        anchor: Anchor,
        id: Id,
        rows: &[Id],
        columns: &[Id],
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if rows.is_empty() || columns.is_empty() {
            return Err(reject("a table needs at least one row and one column"));
        }
        if self.tables.contains_key(&id) {
            return Err(reject(format!("table {id} already exists")));
        }
        let mut all: BTreeSet<Id> = BTreeSet::from([id]);
        if !rows.iter().chain(columns).all(|i| all.insert(*i)) {
            return Err(reject("table, row and column ids must be distinct"));
        }
        let paragraphs = self.paragraphs();
        let effect = match anchor {
            Anchor::Start => {
                let first = paragraphs
                    .iter()
                    .find(|p| p.story == StoryId::Body)
                    .ok_or_else(|| reject("the body has no paragraph"))?;
                DocxEffect::InsertTable {
                    paragraph: first.at.clone(),
                    above: true,
                    rows: rows.len(),
                    columns: columns.len(),
                }
            }
            Anchor::After(mark) => {
                let paragraph = paragraphs
                    .iter()
                    .find(|p| p.story == StoryId::Body && p.mark == Some(mark))
                    .ok_or_else(|| {
                        reject(format!(
                            "{mark} is not a visible paragraph mark in the body"
                        ))
                    })?;
                DocxEffect::InsertTable {
                    paragraph: paragraph.at.clone(),
                    above: false,
                    rows: rows.len(),
                    columns: columns.len(),
                }
            }
            Anchor::End => {
                return Err(reject("split the last paragraph to place a table after it"))
            }
        };
        self.tables
            .insert(id, Table::new(rows, columns, false, true));
        let placed = Placed {
            anchor,
            block: Block::Table(id),
        };
        // jade puts a table straight after its paragraph (before older ones
        // there), or straight above the first paragraph (after older ones).
        match anchor {
            Anchor::After(_) => {
                let at = self
                    .body
                    .blocks
                    .iter()
                    .position(|p| p.anchor == anchor)
                    .unwrap_or(self.body.blocks.len());
                self.body.blocks.insert(at, placed);
            }
            _ => self.body.blocks.push(placed),
        }
        self.observe(all);
        Ok(vec![effect])
    }

    fn structural_table(&self, table: Id) -> Result<(&Table, usize), Rejection> {
        let entry = self
            .tables
            .get(&table)
            .ok_or_else(|| reject(format!("table {table} does not exist")))?;
        if entry.deleted {
            return Err(reject(format!("table {table} was deleted")));
        }
        if !entry.top_level {
            return Err(reject("only top-level tables can change structure"));
        }
        let order = self.table_order(table).expect("a visible table is walked");
        Ok((entry, order))
    }

    fn table_op(&mut self, table_id: Id, op: &GridOp) -> Result<Vec<DocxEffect>, Rejection> {
        let (table, order) = self.structural_table(table_id)?;
        if table.locked {
            return Err(reject(
                "this table has ragged or nested cells; its structure is read-only",
            ));
        }
        if let GridOp::Merge(_) | GridOp::Unmerge { .. } = op {
            return Err(reject(
                "merge and unmerge cells with merge_cells / unmerge_cells",
            ));
        }
        // Rows and columns stay clear of merged regions: jade would have to
        // split or grow the merge, which the model does not follow.
        let mut grid = table.grid.clone();
        let deleted_at = match op {
            GridOp::DeleteRow { id } => table.grid.row_index(*id),
            GridOp::DeleteColumn { id } => table.grid.column_index(*id),
            _ => None,
        };
        grid.apply(op)?;
        let rows_axis = matches!(op, GridOp::InsertRow { .. } | GridOp::DeleteRow { .. });
        let at = match op {
            GridOp::InsertRow { id, .. } => grid.row_index(*id),
            GridOp::InsertColumn { id, .. } => grid.column_index(*id),
            _ => deleted_at,
        }
        .expect("the grid validated the line");
        let inserted = matches!(op, GridOp::InsertRow { .. } | GridOp::InsertColumn { .. });
        let merges = table.merge_indices();
        let span = |(top, left, bottom, right): (usize, usize, usize, usize)| {
            if rows_axis {
                (top, bottom)
            } else {
                (left, right)
            }
        };
        let through = merges.iter().any(|m| {
            let (first, last) = span(*m);
            if inserted {
                first < at && at <= last
            } else {
                first <= at && at <= last
            }
        });
        if through {
            return Err(reject("rows and columns cannot go through merged cells"));
        }
        // A new line copies its neighbour's formatting, unless the neighbour
        // is part of a merge (jade would copy its spans).
        let template = at.checked_sub(1).filter(|i| {
            !merges.iter().any(|m| {
                let (first, last) = span(*m);
                first <= *i && *i <= last
            })
        });
        let (all_rows, all_columns) = (table.grid.all_rows(), table.grid.all_columns());
        let table = self.tables.get_mut(&table_id).expect("checked");
        table.grid = grid;
        let effect = match op {
            GridOp::InsertRow { id, .. } => {
                for column in &all_columns {
                    table.add_cell(*id, *column, Story::empty_paragraph());
                }
                DocxEffect::InsertTableRow {
                    table: order,
                    row: at,
                    template,
                }
            }
            GridOp::InsertColumn { id, .. } => {
                for row in &all_rows {
                    table.add_cell(*row, *id, Story::empty_paragraph());
                }
                DocxEffect::InsertTableColumn {
                    table: order,
                    column: at,
                    template,
                }
            }
            GridOp::DeleteRow { .. } => DocxEffect::DeleteTableRow {
                table: order,
                row: at,
            },
            GridOp::DeleteColumn { .. } => DocxEffect::DeleteTableColumn {
                table: order,
                column: at,
            },
            GridOp::Merge(_) | GridOp::Unmerge { .. } => unreachable!("handled above"),
        };
        if let GridOp::InsertRow { id, .. } | GridOp::InsertColumn { id, .. } = op {
            self.observe([*id]);
        }
        Ok(vec![effect])
    }

    /// Merges `merge` as jade does: the top-left cell shows every covered
    /// cell's stories, row by row; the cells below it in its column stay as
    /// empty followers; the other covered cells go.
    fn merge_cells(
        &mut self,
        table_id: Id,
        merge: &Merge,
        first: Id,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        let (table, order) = self.structural_table(table_id)?;
        if table.locked {
            return Err(reject("this table's structure is read-only"));
        }
        let mut grid = table.grid.clone();
        grid.apply(&GridOp::Merge(*merge))?;
        let positions = table.positions(merge);
        if positions.iter().any(|p| !table.layout.contains_key(p)) {
            return Err(reject("only whole cells can be merged"));
        }
        let cells = positions
            .iter()
            .map(|(row, column)| {
                (
                    table.grid.row_index(*row).expect("alive"),
                    table.grid.column_index(*column).expect("alive"),
                )
            })
            .collect();
        let followers = positions
            .iter()
            .filter(|p| p.1 == merge.left && p.0 != merge.top)
            .count();
        let fresh = |k: usize| first.offset(k as u64);
        if (0..followers).any(|k| table.stories.contains_key(&StoryKey::Split(fresh(k)))) {
            return Err(reject(format!("story id {first} is taken")));
        }
        let table = self.tables.get_mut(&table_id).expect("checked");
        table.grid = grid;
        let master = (merge.top, merge.left);
        let shown: Vec<StoryKey> = positions
            .iter()
            .flat_map(|p| table.layout.get(p).cloned().unwrap_or_default())
            .collect();
        let mut k = 0;
        for position in &positions {
            if *position == master {
                table.layout.insert(master, shown.clone());
            } else if position.1 == merge.left {
                let key = StoryKey::Split(fresh(k));
                k += 1;
                table.stories.insert(key, Story::empty_paragraph());
                table.layout.insert(*position, vec![key]);
            } else {
                table.layout.remove(position);
            }
        }
        if followers > 0 {
            self.observe([fresh(followers - 1)]);
        }
        Ok(vec![DocxEffect::MergeTableCells {
            table: order,
            cells,
        }])
    }

    /// Undoes the merge whose top-left cell is (`top`, `left`): what the
    /// merged cell shows stays there; the cells the merge removed come back
    /// with new empty stories `first`, `first.offset(1)`, … (row by row).
    fn unmerge_cells(
        &mut self,
        table_id: Id,
        top: Id,
        left: Id,
        first: Id,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        let (table, order) = self.structural_table(table_id)?;
        if table.locked {
            return Err(reject("this table's structure is read-only"));
        }
        let merge = *table
            .grid
            .merges()
            .iter()
            .find(|m| m.top == top && m.left == left)
            .ok_or_else(|| reject("no merged region starts at that cell"))?;
        let positions = table.positions(&merge);
        let freed: Vec<(Id, Id)> = positions.iter().copied().filter(|p| p.1 != left).collect();
        let fresh = |k: usize| first.offset(k as u64);
        if (0..freed.len()).any(|k| table.stories.contains_key(&StoryKey::Split(fresh(k)))) {
            return Err(reject(format!("story id {first} is taken")));
        }
        let row = table.grid.row_index(top).expect("alive");
        let column = table.grid.column_index(left).expect("alive");
        let width = positions.iter().filter(|p| p.0 == top).count();
        let table = self.tables.get_mut(&table_id).expect("checked");
        table.grid.apply(&GridOp::Unmerge { top, left })?;
        for (k, position) in freed.iter().enumerate() {
            let key = StoryKey::Split(fresh(k));
            table.stories.insert(key, Story::empty_paragraph());
            table.layout.insert(*position, vec![key]);
        }
        if !freed.is_empty() {
            self.observe([fresh(freed.len() - 1)]);
        }
        Ok(vec![DocxEffect::UnmergeTableCells {
            table: order,
            row,
            column,
            column_span: width,
        }])
    }

    /// The visible atoms of `story` from `start` through `end`, in order —
    /// whatever was inserted between them, minus whatever was deleted.
    fn range_atoms(&self, story: StoryId, start: Id, end: Id) -> Result<Vec<Id>, Rejection> {
        let flow = &self.editable_story(story)?.flow;
        let position = |id: Id| {
            flow.position(id)
                .ok_or_else(|| reject(format!("{id} is not in story {story:?}")))
        };
        let (first, last) = (position(start)?, position(end)?);
        if first > last {
            return Err(reject("the range starts after it ends"));
        }
        let ids: BTreeSet<Id> = flow
            .nodes_in(first..=last)
            .filter(|n| !n.deleted)
            .map(|n| n.id)
            .collect();
        // Only atoms of visible paragraphs (not of a deleted table's cells).
        let located = self.locate(&ids);
        Ok(flow
            .nodes_in(first..=last)
            .map(|n| n.id)
            .filter(|id| located.contains_key(id))
            .collect())
    }

    /// The visible characters of `story` from `start` through `end`, in
    /// document order; `None` when the ids do not name a range of the story.
    pub fn range_chars(&self, story: StoryId, start: Id, end: Id) -> Option<Vec<Id>> {
        let flow = &self.story(story)?.flow;
        let atoms = self.range_atoms(story, start, end).ok()?;
        Some(
            atoms
                .into_iter()
                .filter(|id| flow.get(*id).is_some_and(|n| n.value != Atom::Mark))
                .collect(),
        )
    }

    fn format(
        &mut self,
        story: StoryId,
        start: Id,
        end: Id,
        change: &FormatChange,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        change.validate().map_err(reject)?;
        let atoms = self.range_atoms(story, start, end)?;
        let located = self.locate(&atoms.iter().copied().collect());
        let paragraphs = self.paragraphs();
        if change.is_paragraph_level() {
            let touched: BTreeSet<usize> = located.values().map(|at| at.ordinal).collect();
            return Ok(touched
                .into_iter()
                .map(|p| DocxEffect::FormatParagraph {
                    paragraph: paragraphs[p].at.clone(),
                    change: change.clone(),
                })
                .collect());
        }
        // A range is contiguous, so its characters make one slice per
        // paragraph (marks report a zero-length position and are skipped).
        let mut slices: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
        for at in located.values().filter(|at| at.len > 0) {
            let slice = slices
                .entry(at.ordinal)
                .or_insert((at.offset, at.offset + at.len));
            slice.0 = slice.0.min(at.offset);
            slice.1 = slice.1.max(at.offset + at.len);
        }
        Ok(slices
            .into_iter()
            .map(|(p, (start, end))| DocxEffect::FormatText {
                paragraph: paragraphs[p].at.clone(),
                start,
                end,
                change: change.clone(),
            })
            .collect())
    }

    /// Every live comment, oldest id first.
    pub fn comments(&self) -> Vec<CommentView> {
        self.comments
            .iter()
            .filter(|(_, c)| !c.deleted)
            .map(|(id, c)| CommentView {
                id: *id,
                parent: c.parent,
                file_id: c.file_id.clone(),
                anchor: c.anchor,
                content: c.content.clone(),
                resolved: c.resolved,
            })
            .collect()
    }

    /// jade's `w:id` of a comment: the file's, or the number jade gave it.
    pub fn comment_file_id(&self, id: Id) -> Option<&str> {
        self.comments.get(&id)?.file_id.as_deref()
    }

    fn live_comment(&self, id: Id) -> Result<&Comment, Rejection> {
        match self.comments.get(&id) {
            Some(comment) if !comment.deleted => Ok(comment),
            Some(_) => Err(reject(format!("comment {id} was deleted"))),
            None => Err(reject(format!("comment {id} does not exist"))),
        }
    }

    fn new_comment(&self, id: Id, content: &CommentContent) -> Result<(), Rejection> {
        comment_text(&content.text)?;
        if self.comments.contains_key(&id) {
            return Err(reject(format!("comment {id} already exists")));
        }
        Ok(())
    }

    fn add_comment(
        &mut self,
        story: StoryId,
        start: Id,
        end: Id,
        id: Id,
        content: &CommentContent,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        self.new_comment(id, content)?;
        if matches!(story, StoryId::Footnote { .. } | StoryId::Endnote { .. }) {
            return Err(reject(
                "jade cannot anchor a comment in a footnote or endnote",
            ));
        }
        let chars = self
            .range_chars(story, start, end)
            .ok_or_else(|| reject("the comment's range is not a range of its story"))?;
        let (Some(first), Some(last)) = (chars.first(), chars.last()) else {
            return Err(reject("the text to comment on was deleted"));
        };
        let located = self.locate(&BTreeSet::from([*first, *last]));
        let (from, to) = (located[first], located[last]);
        let (start_paragraph, start_offset) = (self.at(from.ordinal), from.offset);
        let (end_paragraph, end_offset, len) = (self.at(to.ordinal), to.offset, to.len);
        self.comments.insert(
            id,
            Comment {
                parent: None,
                anchor: Some((story, start, end)),
                content: content.clone(),
                resolved: false,
                deleted: false,
                file_id: None,
            },
        );
        self.observe([id]);
        Ok(vec![DocxEffect::AddComment {
            comment: id,
            start_paragraph,
            start: start_offset,
            end_paragraph,
            end: end_offset + len,
            content: content.clone(),
        }])
    }

    fn reply_to_comment(
        &mut self,
        comment: Id,
        id: Id,
        content: &CommentContent,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        self.live_comment(comment)?;
        // Loaded files may nest replies; Word answers the thread's root.
        let mut root = comment;
        for _ in 0..self.comments.len() {
            match self.comments[&root].parent {
                Some(parent) => root = parent,
                None => break,
            }
        }
        self.live_comment(root)?;
        self.new_comment(id, content)?;
        self.comments.insert(
            id,
            Comment {
                parent: Some(root),
                anchor: None,
                content: content.clone(),
                resolved: false,
                deleted: false,
                file_id: None,
            },
        );
        self.observe([id]);
        Ok(vec![DocxEffect::ReplyToComment {
            comment: root,
            reply: id,
            content: content.clone(),
        }])
    }

    fn resolve_comment(
        &mut self,
        comment: Id,
        resolved: bool,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        self.live_comment(comment)?;
        self.comments.get_mut(&comment).expect("checked").resolved = resolved;
        Ok(vec![DocxEffect::ResolveComment { comment, resolved }])
    }

    fn update_comment(&mut self, comment: Id, text: &str) -> Result<Vec<DocxEffect>, Rejection> {
        self.live_comment(comment)?;
        comment_text(text)?;
        self.comments
            .get_mut(&comment)
            .expect("checked")
            .content
            .text = text.to_string();
        Ok(vec![DocxEffect::UpdateComment {
            comment,
            text: text.to_string(),
        }])
    }

    /// Records the number jade gave `item`; only the server says so.
    fn numbered(
        &mut self,
        item: Numbered,
        file_id: &str,
        ctx: &ApplyContext,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if ctx.origin != ReplicaId::ORIGIN {
            return Err(reject("only the server records the numbers jade gives"));
        }
        if file_id.is_empty() {
            return Err(reject("jade's number cannot be empty"));
        }
        match item {
            Numbered::Comment { id } => {
                let comment = self
                    .comments
                    .get_mut(&id)
                    .ok_or_else(|| reject(format!("comment {id} does not exist")))?;
                comment.file_id = Some(file_id.to_string());
            }
            Numbered::Footnote { id } | Numbered::Endnote { id } => {
                let notes = match item {
                    Numbered::Footnote { .. } => &mut self.footnotes,
                    _ => &mut self.endnotes,
                };
                let note = notes
                    .get_mut(&id)
                    .ok_or_else(|| reject(format!("note {id} does not exist")))?;
                note.file_id = file_id.to_string();
            }
        }
        Ok(vec![DocxEffect::Numbered {
            item,
            file_id: file_id.to_string(),
        }])
    }

    fn delete_comment(&mut self, comment: Id) -> Result<Vec<DocxEffect>, Rejection> {
        self.live_comment(comment)?;
        self.delete_thread(comment);
        Ok(vec![DocxEffect::DeleteComment { comment }])
    }

    /// Deletes `comment` and every reply under it, at any depth (as jade).
    fn delete_thread(&mut self, comment: Id) {
        let mut doomed = BTreeSet::from([comment]);
        loop {
            let before = doomed.len();
            for (id, entry) in &self.comments {
                if entry.parent.is_some_and(|p| doomed.contains(&p)) {
                    doomed.insert(*id);
                }
            }
            if doomed.len() == before {
                break;
            }
        }
        for id in doomed {
            self.comments.get_mut(&id).expect("listed").deleted = true;
        }
    }

    fn notes(&self, kind: NoteKind) -> &BTreeMap<Id, Note> {
        match kind {
            NoteKind::Footnote => &self.footnotes,
            NoteKind::Endnote => &self.endnotes,
        }
    }

    fn notes_mut(&mut self, kind: NoteKind) -> &mut BTreeMap<Id, Note> {
        match kind {
            NoteKind::Footnote => &mut self.footnotes,
            NoteKind::Endnote => &mut self.endnotes,
        }
    }

    /// Every story's RGA, deleted ones included.
    fn flows_mut(&mut self) -> impl Iterator<Item = &mut Rga<Atom>> {
        std::iter::once(&mut self.body.flow)
            .chain(
                self.tables
                    .values_mut()
                    .flat_map(|t| t.stories.values_mut().map(|s| &mut s.flow)),
            )
            .chain(self.text_boxes.values_mut().map(|s| &mut s.flow))
            .chain(self.footnotes.values_mut().map(|n| &mut n.story.flow))
            .chain(self.endnotes.values_mut().map(|n| &mut n.story.flow))
    }

    fn insert_note(
        &mut self,
        kind: NoteKind,
        story: StoryId,
        parent: Option<Id>,
        id: Id,
        text: &str,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if !matches!(
            story,
            StoryId::Body | StoryId::Cell { .. } | StoryId::Split { .. }
        ) {
            return Err(reject(
                "jade puts footnotes and endnotes in body paragraphs and table cells only",
            ));
        }
        if text.chars().any(|c| c.is_control() && c != '\t') {
            return Err(reject(
                "a note's text cannot hold line breaks or control characters",
            ));
        }
        if self.footnotes.contains_key(&id) || self.endnotes.contains_key(&id) {
            return Err(reject(format!("note {id} already exists")));
        }
        let host = self.editable_story(story)?;
        let mut flow = host.flow.clone();
        let placement = kind.placement(id);
        flow.insert(id, parent, Atom::Anchor(placement))?;
        // jade puts a new reference before the anchors already at its
        // offset and types only before or after all of them: text typed at
        // this caret concurrently could land between the new reference and
        // an old one, which jade cannot write. So no other anchor may sit
        // right next to it.
        let index = flow.visible_index(id).expect("just inserted");
        let neighbours = [
            index.checked_sub(1).and_then(|i| flow.visible_id_at(i)),
            flow.visible_id_at(index + 1),
        ];
        let anchor = |id: Option<Id>| {
            matches!(
                id.and_then(|id| flow.get(id)).map(|n| &n.value),
                Some(Atom::Anchor(_))
            )
        };
        if neighbours.into_iter().any(anchor) {
            return Err(reject(
                "a note cannot go where a text box or note reference already sits",
            ));
        }
        let host = std::mem::replace(&mut self.story_mut(story).expect("checked").flow, flow);
        let located = self.paragraphs().into_iter().find_map(|p| {
            let at = p.anchors.iter().position(|a| a.id == id)?;
            Some((p.at.clone(), p.anchors[at].offset, at))
        });
        let Some((paragraph, offset, anchors)) = located else {
            self.story_mut(story).expect("checked").flow = host;
            return Err(reject("the note's paragraph is gone"));
        };
        let mut note = Story::empty_paragraph();
        let initial: Vec<Atom> = std::iter::once('\t')
            .chain(text.chars())
            .map(Atom::Char)
            .collect();
        let count = initial.len() as u64;
        note.flow
            .insert_run(id.offset(1), None, initial)
            .expect("a new story takes any ids");
        self.notes_mut(kind).insert(
            id,
            Note {
                file_id: String::new(),
                story: note,
                deleted: false,
            },
        );
        self.observe([id.offset(count)]);
        Ok(vec![DocxEffect::InsertNote {
            kind,
            note: id,
            paragraph,
            offset,
            anchors,
            text: text.to_string(),
        }])
    }

    fn delete_note(&mut self, kind: NoteKind, note: Id) -> Result<Vec<DocxEffect>, Rejection> {
        match self.notes(kind).get(&note) {
            Some(entry) if !entry.deleted => {}
            Some(_) => return Err(reject(format!("note {note} was deleted"))),
            None => return Err(reject(format!("{kind:?} {note} does not exist"))),
        }
        self.notes_mut(kind)
            .get_mut(&note)
            .expect("checked")
            .deleted = true;
        let placement = Atom::Anchor(kind.placement(note));
        for flow in self.flows_mut() {
            let references: Vec<Id> = flow
                .iter()
                .filter(|n| !n.deleted && n.value == placement)
                .map(|n| n.id)
                .collect();
            for id in references {
                flow.delete(id).expect("listed");
            }
        }
        self.drop_comments_in_removed_cells();
        Ok(vec![DocxEffect::DeleteNote { kind, note }])
    }

    /// jade removes the comments anchored in table cells it deletes; so does
    /// the model, after any operation that removes cells.
    fn drop_comments_in_removed_cells(&mut self) {
        let visible: BTreeSet<StoryId> = self.paragraphs().iter().map(|p| p.story).collect();
        let orphaned: Vec<Id> = self
            .comments
            .iter()
            .filter(|(_, c)| !c.deleted)
            .filter_map(|(id, c)| c.anchor.map(|(story, _, _)| (id, story)))
            .filter(|(_, story)| !visible.contains(story))
            .map(|(id, _)| *id)
            .collect();
        for id in orphaned {
            self.delete_thread(id);
        }
    }

    fn delete_table(&mut self, table: Id) -> Result<Vec<DocxEffect>, Rejection> {
        let (_, order) = self.structural_table(table)?;
        self.tables.get_mut(&table).expect("checked").deleted = true;
        Ok(vec![DocxEffect::DeleteTable { table: order }])
    }
}

impl Document for DocxModel {
    type Op = DocxOp;
    type Effect = Vec<DocxEffect>;

    fn lane(op: &DocxOp) -> Lane {
        match op {
            DocxOp::InsertText { .. }
            | DocxOp::DeleteText { .. }
            | DocxOp::SplitParagraph { .. } => Lane::Convergent,
            DocxOp::JoinParagraph { .. }
            | DocxOp::InsertTable { .. }
            | DocxOp::Table { .. }
            | DocxOp::MergeCells { .. }
            | DocxOp::UnmergeCells { .. }
            | DocxOp::DeleteTable { .. }
            | DocxOp::Format { .. }
            | DocxOp::AddComment { .. }
            | DocxOp::ReplyToComment { .. }
            | DocxOp::ResolveComment { .. }
            | DocxOp::DeleteComment { .. }
            | DocxOp::UpdateComment { .. }
            | DocxOp::InsertNote { .. }
            | DocxOp::DeleteNote { .. }
            | DocxOp::Numbered { .. } => Lane::Sequenced,
        }
    }

    fn apply(&mut self, op: &DocxOp, ctx: &ApplyContext) -> Result<Vec<DocxEffect>, Rejection> {
        match op {
            DocxOp::InsertText {
                story,
                parent,
                first,
                text,
            } => self.insert_text(*story, *parent, *first, text),
            DocxOp::DeleteText { story, ids } => self.delete_text(*story, ids),
            DocxOp::SplitParagraph { story, parent, id } => self.split(*story, *parent, *id),
            DocxOp::JoinParagraph { story, mark } => self.join(*story, *mark),
            DocxOp::InsertTable {
                anchor,
                id,
                rows,
                columns,
            } => self.insert_table(*anchor, *id, rows, columns),
            DocxOp::Table { table, op } => {
                let effects = self.table_op(*table, op)?;
                self.drop_comments_in_removed_cells();
                Ok(effects)
            }
            DocxOp::MergeCells {
                table,
                merge,
                first,
            } => self.merge_cells(*table, merge, *first),
            DocxOp::UnmergeCells {
                table,
                top,
                left,
                first,
            } => self.unmerge_cells(*table, *top, *left, *first),
            DocxOp::DeleteTable { table } => {
                let effects = self.delete_table(*table)?;
                self.drop_comments_in_removed_cells();
                Ok(effects)
            }
            DocxOp::Format {
                story,
                start,
                end,
                change,
            } => self.format(*story, *start, *end, change),
            DocxOp::AddComment {
                story,
                start,
                end,
                id,
                content,
            } => self.add_comment(*story, *start, *end, *id, content),
            DocxOp::ReplyToComment {
                comment,
                id,
                content,
            } => self.reply_to_comment(*comment, *id, content),
            DocxOp::ResolveComment { comment, resolved } => {
                self.resolve_comment(*comment, *resolved)
            }
            DocxOp::DeleteComment { comment } => self.delete_comment(*comment),
            DocxOp::UpdateComment { comment, text } => self.update_comment(*comment, text),
            DocxOp::InsertNote {
                kind,
                story,
                parent,
                id,
                text,
            } => self.insert_note(*kind, *story, *parent, *id, text),
            DocxOp::DeleteNote { kind, note } => self.delete_note(*kind, *note),
            DocxOp::Numbered { item, file_id } => self.numbered(*item, file_id, ctx),
        }
    }

    fn max_lamport(&self) -> u64 {
        self.max_lamport
    }
}

/// Serializes a map with non-string keys as a list of pairs.
mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        map: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K, V, D>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}
