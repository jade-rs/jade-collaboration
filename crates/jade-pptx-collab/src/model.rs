//! The collaborative PPTX model.
//!
//! A deck is an ordered list of slide identifiers. Every text body jade can
//! address — a shape's (group children included) and each slide's speaker
//! notes — is a *story*: an RGA of characters and paragraph marks, exactly as
//! in `jade-docx-collab`, so typing and paragraph splits commute. A story's
//! last paragraph has no mark. Each top-level shape's placement is five
//! last-writer-wins registers (x, y, width, height, rotation), so a move and
//! a concurrent resize both survive.
//!
//! Slide structure and shape creation and deletion travel the sequenced lane
//! (see [`PptxOp`] for why each one does). Deletion only tombstones, so a
//! concurrent edit to a deleted slide or shape still lands — invisibly.
//!
//! A duplicated slide shares its source's character and shape identifiers
//! (stories are keyed by slide), and remembers where it came from. An edit
//! to the source whose author had not seen the duplication is applied to the
//! copy too: that is what the author would have got had the edit arrived
//! first, and it keeps every convergent operation commuting with the
//! duplication, which the replicas' fast path relies on.

use std::collections::{BTreeMap, BTreeSet};

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::lww::LwwRegister;
use jade_collab_core::marks::{self, Boundary, Expand, Mark, MarkSet};
use jade_collab_core::rga::Rga;
use jade_collab_sync::{ApplyContext, Document, Lane, Rejection};
use serde::{Deserialize, Serialize};

/// Which text body an operation edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoryId {
    Shape { slide: Id, shape: Id },
    Notes { slide: Id },
}

impl StoryId {
    pub fn slide(self) -> Id {
        match self {
            StoryId::Shape { slide, .. } | StoryId::Notes { slide } => slide,
        }
    }

    /// The same text body on another slide (a copy of this one).
    fn on(self, slide: Id) -> Self {
        match self {
            StoryId::Shape { shape, .. } => StoryId::Shape { slide, shape },
            StoryId::Notes { .. } => StoryId::Notes { slide },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Atom {
    Char(char),
    Mark,
}

/// What a shape is, as jade's reader reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Shape,
    Picture,
    Group,
    Table,
    Chart,
}

/// A shape's placement: offset and extent in EMU, rotation in 60000ths of a
/// degree, clockwise — the units of DrawingML's `<a:xfrm>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub rotation: i64,
}

/// The attributes a geometry write sets; the others are left alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<i64>,
}

impl GeometryEdit {
    fn values(&self) -> [Option<i64>; 5] {
        [self.x, self.y, self.width, self.height, self.rotation]
    }
}

/// A character attribute a mark sets in a shape's text — the run
/// properties jade's pptx writer formats (`TextFormat`), one per mark.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "attribute", rename_all = "snake_case")]
pub enum CharFormat {
    Bold {
        value: bool,
    },
    Italic {
        value: bool,
    },
    Underline {
        value: bool,
    },
    /// In hundredths of a point, as DrawingML stores sizes (`1800` is
    /// 18pt): 1pt to 4000pt.
    Size {
        hundredths: u32,
    },
    /// The Latin typeface.
    Font {
        typeface: String,
    },
    /// The East Asian typeface.
    EastAsianFont {
        typeface: String,
    },
    /// `#RRGGBB`.
    Color {
        color: String,
    },
}

/// The attribute a [`CharFormat`] sets: concurrent marks of one attribute
/// resolve to the newest, different attributes are independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkKey {
    Bold,
    Italic,
    Underline,
    Size,
    Font,
    EastAsianFont,
    Color,
}

impl MarkKey {
    /// Every attribute, in order.
    pub const ALL: [MarkKey; 7] = [
        MarkKey::Bold,
        MarkKey::Italic,
        MarkKey::Underline,
        MarkKey::Size,
        MarkKey::Font,
        MarkKey::EastAsianFont,
        MarkKey::Color,
    ];

    /// As in docx (and PowerPoint): text typed at the end of formatted
    /// text takes its formatting, so every attribute expands at its end.
    pub fn expand(self) -> Expand {
        Expand::End
    }
}

/// PowerPoint's `ST_TextFontSize`, in hundredths of a point.
const FONT_HUNDREDTHS: std::ops::RangeInclusive<u32> = 100..=400_000;

impl CharFormat {
    pub fn key(&self) -> MarkKey {
        match self {
            Self::Bold { .. } => MarkKey::Bold,
            Self::Italic { .. } => MarkKey::Italic,
            Self::Underline { .. } => MarkKey::Underline,
            Self::Size { .. } => MarkKey::Size,
            Self::Font { .. } => MarkKey::Font,
            Self::EastAsianFont { .. } => MarkKey::EastAsianFont,
            Self::Color { .. } => MarkKey::Color,
        }
    }

    /// The value as jade's writer takes it.
    pub fn to_jade(&self) -> jade_pptx_writer::TextFormat {
        let mut format = jade_pptx_writer::TextFormat::default();
        match self {
            Self::Bold { value } => format.bold = Some(*value),
            Self::Italic { value } => format.italic = Some(*value),
            Self::Underline { value } => format.underline = Some(*value),
            Self::Size { hundredths } => format.size_pt = Some(f64::from(*hundredths) / 100.0),
            Self::Font { typeface } => format.font = Some(typeface.clone()),
            Self::EastAsianFont { typeface } => format.east_asian_font = Some(typeface.clone()),
            Self::Color { color } => format.color = Some(color.clone()),
        }
        format
    }

    /// Refuses what jade would not write, by jade's own check.
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Size { hundredths } = self {
            if !FONT_HUNDREDTHS.contains(hundredths) {
                return Err(format!(
                    "{hundredths} is not a font size from 1pt to 4000pt"
                ));
            }
        }
        self.to_jade().validate().map_err(|e| e.to_string())
    }
}

/// Paragraph alignment (`algn`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Left,
    Center,
    Right,
    Justify,
    Distributed,
}

impl Alignment {
    pub fn to_jade(self) -> jade_pptx_writer::ParagraphAlignment {
        use jade_pptx_writer::ParagraphAlignment as A;
        match self {
            Self::Left => A::Left,
            Self::Center => A::Center,
            Self::Right => A::Right,
            Self::Justify => A::Justify,
            Self::Distributed => A::Distributed,
        }
    }
}

/// A run of characters of one paragraph a mark sets one attribute of: what
/// the materializer formats on the file it writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatRun {
    /// The visible slide's position.
    pub slide: usize,
    pub shape: ShapeRef,
    pub paragraph: usize,
    /// UTF-8 byte offsets of model text.
    pub start: usize,
    pub end: usize,
    pub format: CharFormat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Story {
    pub(crate) flow: Rga<Atom>,
    /// False when jade will not edit the text (locked, or holding fields).
    pub(crate) editable: bool,
    /// Character formatting made in the session (shape text only).
    #[serde(default, skip_serializing_if = "MarkSet::is_empty")]
    pub(crate) marks: MarkSet<MarkKey, CharFormat>,
}

impl Story {
    pub(crate) fn new(flow: Rga<Atom>, editable: bool) -> Self {
        Self {
            flow,
            editable,
            marks: MarkSet::new(),
        }
    }

    /// What the marks give each visible character, per attribute.
    fn marked(&self) -> BTreeMap<(Id, MarkKey), CharFormat> {
        let mut out = BTreeMap::new();
        if self.marks.is_empty() {
            return out;
        }
        let order = self.flow.ids();
        for span in self.marks.effective(&self.flow) {
            let Some(value) = span.value else { continue };
            for index in span.range {
                let id = order[index];
                if self
                    .flow
                    .get(id)
                    .is_some_and(|n| !n.deleted && n.value != Atom::Mark)
                {
                    out.insert((id, span.key), value.clone());
                }
            }
        }
        out
    }

    /// The paragraph holding the position right after `atom` (`None`: the
    /// story's start), counting visible paragraph marks — so a joined
    /// mark's paragraph is the one it was merged into.
    fn paragraph_after(&self, atom: Option<Id>) -> Option<usize> {
        let Some(atom) = atom else { return Some(0) };
        let position = self.flow.position(atom)?;
        Some(
            self.flow
                .nodes_in(..=position)
                .filter(|n| !n.deleted && n.value == Atom::Mark)
                .count(),
        )
    }
}

/// What jade lets a geometry write change, read from the shape's locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Locks {
    pub(crate) can_move: bool,
    /// Width and height may be written one at a time.
    pub(crate) can_resize: bool,
    pub(crate) can_rotate: bool,
    /// A group's extent must stay positive.
    pub(crate) group: bool,
    /// The aspect ratio is locked (`noChangeAspect`) on a shape that may
    /// otherwise be resized: only a sequenced resize that keeps the ratio,
    /// by jade's check against the current size, changes its extent.
    #[serde(default)]
    pub(crate) aspect: bool,
}

impl Locks {
    pub(crate) const NONE: Locks = Locks {
        can_move: true,
        can_resize: true,
        can_rotate: true,
        group: false,
        aspect: false,
    };

    /// How jade inserts a picture: free but for its ratio.
    pub(crate) const PICTURE: Locks = Locks {
        can_resize: false,
        aspect: true,
        ..Locks::NONE
    };
}

/// The most image bytes one picture may carry: the bytes travel inline, in
/// the operation (base64 on the wire, about 11 MiB of JSON at most), and
/// stay in the log and its journal.
pub const MAX_PICTURE_BYTES: usize = 8 * 1024 * 1024;

/// Image bytes inside an operation, base64 on the wire.
#[derive(Clone, PartialEq, Eq)]
pub struct Image(pub Vec<u8>);

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({} bytes)", self.0.len())
    }
}

impl Serialize for Image {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine;
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Image {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use base64::Engine;
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map(Image)
            .map_err(serde::de::Error::custom)
    }
}

/// What every replica checks of a picture: its size and that it claims to
/// be a PNG or JPEG — cheap, so a pending insertion can be replayed on
/// every rebuild of a view. Whether jade can decode it is decided once, by
/// the server's materializer, before the insertion commits.
fn check_image(image: &[u8]) -> Result<(), Rejection> {
    if image.len() > MAX_PICTURE_BYTES {
        return Err(reject(format!(
            "the picture has {} bytes; at most {MAX_PICTURE_BYTES} travel in an operation",
            image.len()
        )));
    }
    let png = image.starts_with(b"\x89PNG\r\n\x1a\n");
    let jpeg = image.starts_with(&[0xff, 0xd8, 0xff]);
    if !png && !jpeg {
        return Err(reject("only PNG and JPEG pictures are supported"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Placement {
    /// x, y, width, height, rotation.
    pub(crate) registers: [LwwRegister<i64>; 5],
    pub(crate) locks: Locks,
}

impl Placement {
    pub(crate) fn new(stamp: Id, geometry: Geometry, locks: Locks) -> Self {
        let Geometry {
            x,
            y,
            width,
            height,
            rotation,
        } = geometry;
        Self {
            registers: [x, y, width, height, rotation].map(|v| LwwRegister::new(stamp, v)),
            locks,
        }
    }

    pub(crate) fn geometry(&self) -> Geometry {
        let v = |k: usize| *self.registers[k].get().expect("registers start written");
        Geometry {
            x: v(0),
            y: v(1),
            width: v(2),
            height: v(3),
            rotation: v(4),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Shape {
    /// The shape's `cNvPr id` in the file, for shapes loaded or placed by
    /// jade's slide templates; for a text box made in the session, a number
    /// the model allocates (see [`ShapeView::number`]).
    pub(crate) number: u32,
    pub(crate) kind: ShapeKind,
    /// The group this shape sits in.
    pub(crate) group: Option<Id>,
    pub(crate) text: Option<Story>,
    /// Top-level shapes jade can move: their placement.
    pub(crate) placement: Option<Placement>,
    pub(crate) deletable: bool,
    pub(crate) deleted: bool,
}

/// A copy made by duplicating a slide, as its source remembers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CopyMade {
    pub(crate) slide: Id,
    pub(crate) origin: ReplicaId,
    /// The log position of the duplication; `None` while tentative.
    pub(crate) seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Slide {
    /// In jade's order: the slide's shape tree, depth first.
    pub(crate) shapes: Vec<(Id, Shape)>,
    pub(crate) notes: Story,
    /// The slide layout part, which decides what a slide inserted in front
    /// of this one starts with.
    pub(crate) layout: Option<String>,
    /// The number the next text box made on this slide gets.
    pub(crate) next_number: u32,
    pub(crate) copies: Vec<CopyMade>,
    pub(crate) deleted: bool,
    /// `show="0"`: skipped in a slide show. Unwritten (older checkpoints,
    /// new slides) reads as shown.
    #[serde(default)]
    pub(crate) hidden: LwwRegister<bool>,
}

impl Slide {
    fn shape(&self, id: Id) -> Option<&Shape> {
        self.shapes.iter().find(|(k, _)| *k == id).map(|(_, s)| s)
    }

    fn shape_mut(&mut self, id: Id) -> Option<&mut Shape> {
        self.shapes
            .iter_mut()
            .find(|(k, _)| *k == id)
            .map(|(_, s)| s)
    }

    fn visible_shapes(&self) -> impl Iterator<Item = &(Id, Shape)> {
        self.shapes.iter().filter(|(_, s)| !s.deleted)
    }

    pub(crate) fn is_hidden(&self) -> bool {
        self.hidden.get().copied().unwrap_or(false)
    }
}

/// What jade puts on a slide it inserts: the placeholders of the layout of
/// the slide it is inserted in front of (keyed by layout part; `None` is
/// jade's fallback title and body), read from jade when the deck is loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Template {
    pub(crate) layout: Option<String>,
    pub(crate) shapes: Vec<(Id, Shape)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PptxModel {
    #[serde(with = "pairs")]
    pub(crate) slides: BTreeMap<Id, Slide>,
    /// Visible slides, in presentation order.
    pub(crate) order: Vec<Id>,
    pub(crate) templates: Vec<Template>,
    pub(crate) max_lamport: u64,
}

/// One paragraph of a story.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    /// The mark that ends it; `None` for its story's last paragraph.
    pub mark: Option<Id>,
    pub text: String,
    /// The visible characters, in order (one per `char` of `text`).
    pub chars: Vec<Id>,
}

/// A visible shape as the model sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapeView {
    pub id: Id,
    /// The shape id an index-addressed caller uses: the file's `cNvPr id`
    /// for every shape loaded from the file or placed by a new slide's
    /// layout. A text box made during the session gets the next number the
    /// model has not used on its slide; jade may give it a different id in
    /// the file (it reuses the ids of deleted shapes), and the materializer
    /// keeps the mapping.
    pub number: u32,
    pub kind: ShapeKind,
    pub group: Option<Id>,
    /// Paragraphs joined with `\n`; `None` for a shape jade cannot edit the
    /// text of (no text body, a table, a nested group's child).
    pub text: Option<String>,
    pub editable: bool,
    /// Top-level shapes jade can place.
    pub geometry: Option<Geometry>,
    /// The aspect ratio is locked: the shape is resized (keeping it) by a
    /// sequenced `ResizeShape`, not by geometry writes.
    pub aspect_locked: bool,
    pub deletable: bool,
}

/// A visible slide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlideView {
    pub id: Id,
    pub shapes: Vec<ShapeView>,
    /// Speaker notes, paragraphs joined with `\n`.
    pub notes: String,
    pub notes_editable: bool,
    /// Skipped in a slide show (`show="0"`).
    pub hidden: bool,
}

/// A shape as the materializer needs it: by identity, and by the number the
/// model knows it under (the file id unless the materializer learnt
/// another).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapeRef {
    pub slide: Id,
    pub shape: Id,
    pub number: u32,
}

/// The text body an effect edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextTarget {
    Shape(ShapeRef),
    Notes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PptxOp {
    /// Convergent. Inserts `text` (no line breaks; split paragraphs instead)
    /// after `parent` (`None`: story start); its characters get ids `first`,
    /// `first.offset(1)`, ….
    InsertText {
        story: StoryId,
        parent: Option<Id>,
        first: Id,
        text: String,
    },
    /// Convergent. Deletes characters (never marks); idempotent.
    DeleteText { story: StoryId, ids: Vec<Id> },
    /// Convergent. Inserts a paragraph mark after `parent`.
    SplitParagraph {
        story: StoryId,
        parent: Option<Id>,
        id: Id,
    },
    /// Convergent. Writes some of a top-level shape's geometry registers;
    /// the newest `stamp` wins per attribute.
    SetGeometry {
        slide: Id,
        shape: Id,
        stamp: Id,
        edit: GeometryEdit,
    },
    /// Sequenced, as in docx: it deletes a mark, which merges two
    /// paragraphs' properties (jade keeps the first one's). Which two
    /// paragraphs meet depends on concurrent splits, so every copy of the
    /// file must merge them in the same order.
    JoinParagraph { story: StoryId, mark: Id },
    /// Sequenced. A new slide after `after` (`None`: first). jade fills it
    /// with the placeholders of the layout of the slide it lands in front
    /// of (the last slide when appended) — which slide that is depends on
    /// every concurrent insertion, move and deletion, so only the log
    /// decides it.
    InsertSlide { id: Id, after: Option<Id> },
    /// Sequenced. A deck keeps at least one slide: two concurrent deletions
    /// of the last two slides are each valid and together are not.
    DeleteSlide { slide: Id },
    /// Sequenced. Concurrent moves of one slide must end in one place, and
    /// a move after a concurrently deleted or moved slide has to be
    /// re-checked; ordering them is simpler than a move CRDT and needs no
    /// cycle handling.
    MoveSlide { slide: Id, after: Option<Id> },
    /// Sequenced. The copy (named `id`, placed right after its source, as
    /// jade places it) holds the source's content as of the duplication's
    /// log position; edits whose authors had not seen it are applied to
    /// both.
    DuplicateSlide { slide: Id, id: Id },
    /// Sequenced. An empty text box on top of the slide. jade picks its
    /// file id from the ids in use at that point of the log, and may
    /// refuse a slide whose shape tree it cannot extend.
    InsertTextBox {
        slide: Id,
        id: Id,
        geometry: Geometry,
    },
    /// Convergent. Hides a slide from the slide show (or shows it again):
    /// one last-writer-wins register per slide. Any combination of hidden
    /// slides is a valid deck and jade sets `show` on any slide, so
    /// concurrent writes need no coordination; the newest stamp wins, as
    /// for geometry.
    SetSlideHidden { slide: Id, stamp: Id, hidden: bool },
    /// Convergent. Character formatting as a Peritext mark, as in docx:
    /// `format` holds from `start` to `end` (sides of atoms of a shape's
    /// text, or its ends) and the newest mark of an attribute wins on each
    /// character it covers. The intent API anchors it before the first
    /// character and before the atom after the last one (or at the end), so
    /// typing at its end is covered. Marks never reach jade's document:
    /// they are realised on the file when it is written. Speaker notes take
    /// none (jade formats slide text only).
    Mark {
        story: StoryId,
        id: Id,
        start: Boundary,
        end: Boundary,
        format: CharFormat,
    },
    /// Sequenced. Aligns the paragraphs from the one beginning after atom
    /// `from` through the one beginning after `to` (`None`: the first
    /// paragraph), resolved in log order: paragraph properties live on
    /// paragraph marks, which concurrent splits and joins create and merge
    /// (jade keeps the first paragraph's on a join), so every copy of the
    /// file has to see them in one order. Naming a paragraph by the atom
    /// before it reaches an empty last paragraph too; a joined mark names
    /// the paragraph it was merged into.
    AlignParagraphs {
        story: StoryId,
        from: Option<Id>,
        to: Option<Id>,
        alignment: Alignment,
    },
    /// Sequenced. A picture on top of the slide, its image bytes inline
    /// (at most [`MAX_PICTURE_BYTES`]). Like a text box, jade picks its
    /// file id (and the media part, named by the bytes' hash) at that point
    /// of the log and may refuse the slide or the image.
    InsertPicture {
        slide: Id,
        id: Id,
        geometry: Geometry,
        image: Image,
    },
    /// Sequenced. Writes both extent registers of a top-level shape at
    /// once. For a shape whose ratio is locked (every picture jade
    /// inserts) jade checks the new size against the current one, so the
    /// writes have to be checked in log order; it is refused unless it
    /// keeps the ratio within jade's tolerance.
    ResizeShape {
        slide: Id,
        shape: Id,
        stamp: Id,
        width: i64,
        height: i64,
    },
    /// Sequenced. Deletes a top-level shape (a group with its children).
    /// jade refuses shapes other parts of the slide depend on in ways it
    /// cannot untangle, which only the file shows.
    DeleteShape { slide: Id, shape: Id },
}

/// An operation's consequence in jade terms: slides by their 0-based
/// position in jade's order, paragraphs by index within their text body,
/// offsets as UTF-8 bytes of model text. The materializer maps these to
/// jade's UTF-16 text ranges and file shape ids. Emitted in the order to
/// apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PptxEffect {
    InsertText {
        slide: usize,
        target: TextTarget,
        paragraph: usize,
        offset: usize,
        text: String,
    },
    DeleteRange {
        slide: usize,
        target: TextTarget,
        paragraph: usize,
        start: usize,
        end: usize,
    },
    SplitParagraph {
        slide: usize,
        target: TextTarget,
        paragraph: usize,
        offset: usize,
    },
    /// Join `paragraph` (whose text is `offset` bytes long) with the next.
    JoinParagraph {
        slide: usize,
        target: TextTarget,
        paragraph: usize,
        offset: usize,
    },
    /// The shape's whole placement after the write.
    SetTransform {
        slide: usize,
        shape: ShapeRef,
        geometry: Geometry,
    },
    /// Aligns one paragraph of a shape's text.
    AlignParagraph {
        slide: usize,
        shape: ShapeRef,
        paragraph: usize,
        alignment: Alignment,
    },
    /// The slide's `show` attribute after the write.
    SetSlideHidden {
        slide: usize,
        hidden: bool,
    },
    InsertSlide {
        at: usize,
    },
    DeleteSlide {
        at: usize,
    },
    /// `to` is the slide's final index, as jade's `move_slide` takes it.
    MoveSlide {
        from: usize,
        to: usize,
    },
    /// Duplicates the slide at `at`; the copy lands at `at + 1`.
    DuplicateSlide {
        at: usize,
        source: Id,
        copy: Id,
    },
    InsertTextBox {
        slide: usize,
        shape: ShapeRef,
        geometry: Geometry,
    },
    DeleteShape {
        slide: usize,
        shape: ShapeRef,
    },
    InsertPicture {
        slide: usize,
        shape: ShapeRef,
        geometry: Geometry,
        image: Image,
    },
}

fn reject(message: impl Into<String>) -> Rejection {
    Rejection::new(message)
}

/// The paragraphs of a flow.
fn paragraphs_of(flow: &Rga<Atom>) -> Vec<Paragraph> {
    let empty = || Paragraph {
        mark: None,
        text: String::new(),
        chars: Vec::new(),
    };
    let mut out = Vec::new();
    let mut current = empty();
    for (id, atom) in flow.visible() {
        match atom {
            Atom::Char(c) => {
                current.text.push(*c);
                current.chars.push(id);
            }
            Atom::Mark => {
                current.mark = Some(id);
                out.push(std::mem::replace(&mut current, empty()));
            }
        }
    }
    out.push(current);
    out
}

fn joined(flow: &Rga<Atom>) -> String {
    flow.visible()
        .map(|(_, atom)| match atom {
            Atom::Char(c) => *c,
            Atom::Mark => '\n',
        })
        .collect()
}

/// Builds a flow from text whose `\n`s are paragraph marks, with ids from
/// `next`.
pub(crate) fn flow_from_text(text: &str, next: &mut u64) -> Rga<Atom> {
    let mut flow = Rga::new();
    let mut previous = None;
    for c in text.chars() {
        let id = Id::new(*next, ReplicaId::ORIGIN);
        *next += 1;
        let atom = if c == '\n' { Atom::Mark } else { Atom::Char(c) };
        flow.insert(id, previous, atom).expect("fresh id");
        previous = Some(id);
    }
    flow
}

/// jade's text rule: characters XML can hold, no line breaks (those are
/// paragraph marks here). Checked with jade's own validation, so that every
/// replica refuses exactly what jade would.
fn check_text(text: &str) -> Result<(), Rejection> {
    if text.is_empty() {
        return Err(reject("nothing to insert"));
    }
    if text.contains('\n') {
        return Err(reject(
            "split the paragraph instead of inserting a line break",
        ));
    }
    let unit = Geometry {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
        rotation: 0,
    };
    jade_pptx_writer::validate_text_box(
        transform(unit).expect("in range"),
        text,
        &jade_pptx_writer::TextFormat::default(),
    )
    .map_err(|e| reject(e.to_string()))
}

/// The jade transform for a placement (no flips).
pub(crate) fn transform(geometry: Geometry) -> Option<jade_pptx_reader::Transform> {
    Some(jade_pptx_reader::Transform {
        offset_emu: (geometry.x, geometry.y),
        extent_emu: (geometry.width, geometry.height),
        rotation_60000ths: i32::try_from(geometry.rotation).ok()?,
        flip_h: false,
        flip_v: false,
    })
}

/// Whether jade would write these values (PowerPoint's 32-bit EMU range, no
/// negative extent), by jade's own check.
fn check_geometry(geometry: Geometry) -> Result<(), Rejection> {
    let t = transform(geometry).ok_or_else(|| reject("rotation is out of range"))?;
    jade_pptx_writer::validate_transform(t).map_err(|e| reject(e.to_string()))
}

impl PptxModel {
    /// A deck for tests and new documents: one slide per entry, one text
    /// box per string (numbered 2, 3, … like PowerPoint's shape ids), `\n`
    /// separating paragraphs. Ids belong to [`ReplicaId::ORIGIN`]; a slide
    /// inserted into it starts empty.
    pub fn from_texts(slides: &[&[&str]]) -> Self {
        let mut next = 1u64;
        let mut model = PptxModel {
            slides: BTreeMap::new(),
            order: Vec::new(),
            templates: vec![Template {
                layout: None,
                shapes: Vec::new(),
            }],
            max_lamport: 0,
        };
        let mut ids = Vec::new();
        for shapes in slides {
            let slide = Id::new(next, ReplicaId::ORIGIN);
            let keys: Vec<Id> = (1..=shapes.len() as u64)
                .map(|k| Id::new(next + k, ReplicaId::ORIGIN))
                .collect();
            let stamp = Id::new(next + shapes.len() as u64 + 1, ReplicaId::ORIGIN);
            next += shapes.len() as u64 + 2;
            ids.push((slide, keys, stamp));
        }
        for ((slide, keys, stamp), texts) in ids.into_iter().zip(slides) {
            let shapes: Vec<(Id, Shape)> = keys
                .into_iter()
                .zip(texts.iter())
                .enumerate()
                .map(|(k, (key, text))| {
                    let shape = Shape {
                        number: k as u32 + 2,
                        kind: ShapeKind::Shape,
                        group: None,
                        text: Some(Story::new(flow_from_text(text, &mut next), true)),
                        placement: Some(Placement::new(
                            stamp,
                            Geometry {
                                x: 0,
                                y: 914_400 * k as i64,
                                width: 914_400,
                                height: 914_400,
                                rotation: 0,
                            },
                            Locks::NONE,
                        )),
                        deletable: true,
                        deleted: false,
                    };
                    (key, shape)
                })
                .collect();
            model.order.push(slide);
            model.slides.insert(
                slide,
                Slide {
                    next_number: shapes.len() as u32 + 2,
                    shapes,
                    notes: Story::new(Rga::new(), true),
                    layout: None,
                    copies: Vec::new(),
                    deleted: false,
                    hidden: LwwRegister::default(),
                },
            );
        }
        model.max_lamport = next - 1;
        model
    }

    /// Visible slides in presentation order, with their visible shapes.
    pub fn slides(&self) -> Vec<SlideView> {
        self.order
            .iter()
            .map(|id| {
                let slide = &self.slides[id];
                SlideView {
                    id: *id,
                    shapes: slide
                        .visible_shapes()
                        .map(|(key, shape)| ShapeView {
                            id: *key,
                            number: shape.number,
                            kind: shape.kind,
                            group: shape.group,
                            text: shape.text.as_ref().map(|s| joined(&s.flow)),
                            editable: shape.text.as_ref().is_some_and(|s| s.editable),
                            geometry: shape.placement.as_ref().map(Placement::geometry),
                            aspect_locked: shape.placement.as_ref().is_some_and(|p| p.locks.aspect),
                            deletable: shape.deletable,
                        })
                        .collect(),
                    notes: joined(&slide.notes.flow),
                    notes_editable: slide.notes.editable,
                    hidden: slide.is_hidden(),
                }
            })
            .collect()
    }

    /// The 0-based position of a visible slide.
    pub fn slide_index(&self, slide: Id) -> Option<usize> {
        self.order.iter().position(|s| *s == slide)
    }

    pub(crate) fn story(&self, id: StoryId) -> Option<&Story> {
        let slide = self.slides.get(&id.slide())?;
        match id {
            StoryId::Shape { shape, .. } => slide.shape(shape)?.text.as_ref(),
            StoryId::Notes { .. } => Some(&slide.notes),
        }
    }

    fn story_mut(&mut self, id: StoryId) -> Option<&mut Story> {
        let slide = self.slides.get_mut(&id.slide())?;
        match id {
            StoryId::Shape { shape, .. } => slide.shape_mut(shape)?.text.as_mut(),
            StoryId::Notes { .. } => Some(&mut slide.notes),
        }
    }

    /// The story's paragraphs (also for a deleted slide or shape); `None`
    /// when there is no such text body.
    pub fn paragraphs(&self, story: StoryId) -> Option<Vec<Paragraph>> {
        self.story(story).map(|s| paragraphs_of(&s.flow))
    }

    /// The story's text, paragraphs joined with `\n`.
    pub fn text(&self, story: StoryId) -> Option<String> {
        self.story(story).map(|s| joined(&s.flow))
    }

    /// The visible shape numbered `number` on the visible slide at `index`,
    /// if exactly one has that number.
    pub fn shape_by_number(&self, index: usize, number: u32) -> Option<Id> {
        let slide = &self.slides[self.order.get(index)?];
        let mut found = slide
            .visible_shapes()
            .filter(|(_, s)| s.number == number)
            .map(|(k, _)| *k);
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    fn observe(&mut self, ids: impl IntoIterator<Item = Id>) {
        for id in ids {
            self.max_lamport = self.max_lamport.max(id.lamport);
        }
    }

    /// Where an effect on `story` lands: the visible slide's index and the
    /// text target, if both the slide and the shape are visible.
    fn realised(&self, story: StoryId) -> Option<(usize, TextTarget)> {
        let slide_id = story.slide();
        let index = self.slide_index(slide_id)?;
        let target = match story {
            StoryId::Notes { .. } => TextTarget::Notes,
            StoryId::Shape { shape, .. } => {
                let s = self.slides[&slide_id].shape(shape)?;
                if s.deleted {
                    return None;
                }
                TextTarget::Shape(ShapeRef {
                    slide: slide_id,
                    shape,
                    number: s.number,
                })
            }
        };
        Some((index, target))
    }

    /// The story and every copy of it an edit by `ctx` must also reach:
    /// copies made by duplications its author had not seen when making it,
    /// transitively. An author has seen its own duplications (it submits in
    /// order) and those committed at or before its `base_seq`; a tentative
    /// duplication (`seq: None`) by someone else it has not.
    fn reach(&self, story: StoryId, ctx: &ApplyContext) -> Vec<StoryId> {
        let mut out = vec![story];
        let mut k = 0;
        while k < out.len() {
            let current = out[k];
            if let Some(slide) = self.slides.get(&current.slide()) {
                for copy in &slide.copies {
                    let unseen =
                        copy.origin != ctx.origin && copy.seq.is_none_or(|seq| ctx.base_seq < seq);
                    if unseen {
                        out.push(current.on(copy.slide));
                    }
                }
            }
            k += 1;
        }
        out
    }

    fn editable_story(&self, id: StoryId) -> Result<&Story, Rejection> {
        let story = self
            .story(id)
            .ok_or_else(|| reject(format!("text body {id:?} does not exist")))?;
        if !story.editable {
            return Err(reject("jade does not edit this text (locked, or fields)"));
        }
        Ok(story)
    }

    /// (paragraph, byte offset, byte length) of each requested visible atom
    /// of a story; a mark reports its paragraph's end.
    fn locate(&self, story: StoryId, ids: &BTreeSet<Id>) -> BTreeMap<Id, (usize, usize, usize)> {
        let mut found = BTreeMap::new();
        for (index, paragraph) in self
            .paragraphs(story)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let mut offset = 0;
            for (c, id) in paragraph.text.chars().zip(&paragraph.chars) {
                if ids.contains(id) {
                    found.insert(*id, (index, offset, c.len_utf8()));
                }
                offset += c.len_utf8();
            }
            if let Some(mark) = paragraph.mark.filter(|m| ids.contains(m)) {
                found.insert(mark, (index, offset, 0));
            }
        }
        found
    }

    fn locate_one(&self, story: StoryId, id: Id) -> Option<(usize, usize)> {
        self.locate(story, &BTreeSet::from([id]))
            .get(&id)
            .map(|&(p, offset, _)| (p, offset))
    }

    fn insert_text(
        &mut self,
        story: StoryId,
        parent: Option<Id>,
        first: Id,
        text: &str,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        check_text(text)?;
        self.editable_story(story)?;
        let count = text.chars().count() as u64;
        let mut effects = Vec::new();
        for (k, target) in self.reach(story, ctx).into_iter().enumerate() {
            let Some(flow) = self.story_mut(target).map(|s| &mut s.flow) else {
                continue;
            };
            match flow.insert_run(first, parent, text.chars().map(Atom::Char)) {
                Ok(_) => {}
                // The named story must take it. A copy always can (whatever
                // the author saw there, the copy got too), but is left alone
                // rather than failing the edit if not.
                Err(e) if k == 0 => return Err(e.into()),
                Err(_) => continue,
            }
            if let (Some((slide, target_ref)), Some((paragraph, offset))) =
                (self.realised(target), self.locate_one(target, first))
            {
                effects.push(PptxEffect::InsertText {
                    slide,
                    target: target_ref,
                    paragraph,
                    offset,
                    text: text.to_string(),
                });
            }
        }
        self.observe([first.offset(count - 1)]);
        Ok(effects)
    }

    fn delete_text(
        &mut self,
        story: StoryId,
        ids: &[Id],
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        let flow = &self.editable_story(story)?.flow;
        for id in ids {
            match flow.get(*id) {
                None => return Err(reject(format!("character {id} does not exist"))),
                Some(node) if node.value == Atom::Mark => {
                    return Err(reject("paragraph marks are joined, not deleted"))
                }
                Some(_) => {}
            }
        }
        let mut effects = Vec::new();
        for target in self.reach(story, ctx) {
            let present: Vec<Id> = match self.story(target) {
                Some(s) => ids
                    .iter()
                    .copied()
                    .filter(|id| s.flow.contains(*id))
                    .collect(),
                None => continue,
            };
            let positions = self.locate(target, &present.iter().copied().collect());
            let flow = &mut self.story_mut(target).expect("checked").flow;
            for id in &present {
                flow.delete(*id)?;
            }
            let Some((slide, target_ref)) = self.realised(target) else {
                continue;
            };
            // Adjacent characters merge into ranges, applied from the end so
            // earlier offsets stay valid.
            let mut spans: Vec<(usize, usize, usize)> = positions
                .values()
                .map(|&(p, offset, len)| (p, offset, offset + len))
                .collect();
            spans.sort();
            let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
            for (p, start, end) in spans {
                match ranges.last_mut() {
                    Some(last) if last.0 == p && last.2 == start => last.2 = end,
                    _ => ranges.push((p, start, end)),
                }
            }
            effects.extend(ranges.into_iter().rev().map(|(paragraph, start, end)| {
                PptxEffect::DeleteRange {
                    slide,
                    target: target_ref,
                    paragraph,
                    start,
                    end,
                }
            }));
        }
        Ok(effects)
    }

    fn split(
        &mut self,
        story: StoryId,
        parent: Option<Id>,
        id: Id,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        self.editable_story(story)?;
        let mut effects = Vec::new();
        for (k, target) in self.reach(story, ctx).into_iter().enumerate() {
            let Some(flow) = self.story_mut(target).map(|s| &mut s.flow) else {
                continue;
            };
            match flow.insert(id, parent, Atom::Mark) {
                Ok(_) => {}
                Err(e) if k == 0 => return Err(e.into()),
                Err(_) => continue,
            }
            if let (Some((slide, target_ref)), Some((paragraph, offset))) =
                (self.realised(target), self.locate_one(target, id))
            {
                effects.push(PptxEffect::SplitParagraph {
                    slide,
                    target: target_ref,
                    paragraph,
                    offset,
                });
            }
        }
        self.observe([id]);
        Ok(effects)
    }

    /// Joins in the story and, like any other edit, in the copies its author
    /// had not seen that still have the mark.
    fn join(
        &mut self,
        story: StoryId,
        mark: Id,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        match self.editable_story(story)?.flow.get(mark) {
            Some(node) if node.value == Atom::Mark && !node.deleted => {}
            Some(node) if node.value == Atom::Mark => {
                return Err(reject("the paragraphs are already joined"))
            }
            _ => return Err(reject(format!("{mark} is not a paragraph mark"))),
        }
        let mut effects = Vec::new();
        for target in self.reach(story, ctx) {
            if !self.story(target).is_some_and(|s| s.flow.is_visible(mark)) {
                continue;
            }
            if let (Some((slide, target_ref)), Some((paragraph, offset))) =
                (self.realised(target), self.locate_one(target, mark))
            {
                effects.push(PptxEffect::JoinParagraph {
                    slide,
                    target: target_ref,
                    paragraph,
                    offset,
                });
            }
            self.story_mut(target).expect("checked").flow.delete(mark)?;
        }
        Ok(effects)
    }

    fn shape_text(&self, story: StoryId) -> Result<&Story, Rejection> {
        if let StoryId::Notes { .. } = story {
            return Err(reject("jade formats the text of slide shapes only"));
        }
        self.editable_story(story)
    }

    /// Applies a mark. The checks depend only on the op and on atoms that
    /// exist and never reorder, so every replica decides alike wherever the
    /// mark lands in the log.
    fn mark(
        &mut self,
        story: StoryId,
        id: Id,
        start: Boundary,
        end: Boundary,
        format: &CharFormat,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        format.validate().map_err(reject)?;
        let target = self.shape_text(story)?;
        if target.marks.iter().any(|m| m.id == id) {
            return Err(reject(format!("mark {id} already exists")));
        }
        let mark = Mark::new(id, format.key(), start, end, Some(format.clone()));
        MarkSet::validate(&target.flow, &mark)?;
        for (k, copy) in self.reach(story, ctx).into_iter().enumerate() {
            let Some(target) = self.story_mut(copy) else {
                continue;
            };
            // A copy always has the anchors its author saw, but is left
            // alone rather than failing the mark if not.
            if k > 0 && MarkSet::validate(&target.flow, &mark).is_err() {
                continue;
            }
            let _ = target.marks.insert(mark.clone());
        }
        self.observe([id]);
        Ok(Vec::new())
    }

    /// The boundaries of a mark over the atoms `first ..= last` of `story`
    /// under `expand`, as the intent API makes them.
    pub fn mark_anchors(
        &self,
        story: StoryId,
        first: Id,
        last: Id,
        expand: Expand,
    ) -> Option<(Boundary, Boundary)> {
        marks::anchors(&self.story(story)?.flow, first, last, expand)
    }

    /// The formats the marks set on each visible character of a story, in
    /// attribute order; characters no mark covers are left out (they show
    /// jade's formatting).
    pub fn char_formats(&self, story: StoryId) -> Option<BTreeMap<Id, Vec<CharFormat>>> {
        let mut out: BTreeMap<Id, Vec<CharFormat>> = BTreeMap::new();
        for ((id, _), format) in self.story(story)?.marked() {
            out.entry(id).or_default().push(format);
        }
        Some(out)
    }

    /// What makes a file show the marks: maximal runs of one value, per
    /// visible shape, paragraph and attribute, in that order.
    pub fn formatting(&self) -> Vec<FormatRun> {
        let mut out = Vec::new();
        for (index, slide_id) in self.order.iter().enumerate() {
            for (key, shape) in self.slides[slide_id].visible_shapes() {
                let Some(story) = shape.text.as_ref().filter(|s| !s.marks.is_empty()) else {
                    continue;
                };
                let shape_ref = ShapeRef {
                    slide: *slide_id,
                    shape: *key,
                    number: shape.number,
                };
                let marked = story.marked();
                for (p, paragraph) in paragraphs_of(&story.flow).iter().enumerate() {
                    for attribute in MarkKey::ALL {
                        let mut run: Option<FormatRun> = None;
                        let mut offset = 0;
                        for (ch, c) in paragraph.text.chars().zip(&paragraph.chars) {
                            let next = offset + ch.len_utf8();
                            match marked.get(&(*c, attribute)) {
                                Some(format) => match &mut run {
                                    Some(r) if r.end == offset && r.format == *format => {
                                        r.end = next
                                    }
                                    _ => {
                                        out.extend(run.take());
                                        run = Some(FormatRun {
                                            slide: index,
                                            shape: shape_ref,
                                            paragraph: p,
                                            start: offset,
                                            end: next,
                                            format: format.clone(),
                                        });
                                    }
                                },
                                None => out.extend(run.take()),
                            }
                            offset = next;
                        }
                        out.extend(run);
                    }
                }
            }
        }
        out
    }

    /// Aligns paragraphs in the story and in the copies its author had not
    /// seen.
    fn align(
        &mut self,
        story: StoryId,
        from: Option<Id>,
        to: Option<Id>,
        alignment: Alignment,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        let target = self.shape_text(story)?;
        for atom in [from, to].into_iter().flatten() {
            if !target.flow.contains(atom) {
                return Err(reject(format!("{atom} is not in this text")));
            }
        }
        let mut effects = Vec::new();
        for copy in self.reach(story, ctx) {
            let Some(target) = self.story(copy) else {
                continue;
            };
            let (Some(first), Some(last)) =
                (target.paragraph_after(from), target.paragraph_after(to))
            else {
                continue;
            };
            if let Some((slide, TextTarget::Shape(shape))) = self.realised(copy) {
                effects.extend((first..=last).map(|paragraph| PptxEffect::AlignParagraph {
                    slide,
                    shape,
                    paragraph,
                    alignment,
                }));
            }
        }
        Ok(effects)
    }

    fn set_geometry(
        &mut self,
        slide_id: Id,
        shape_id: Id,
        stamp: Id,
        edit: &GeometryEdit,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        let shape = self
            .slides
            .get(&slide_id)
            .and_then(|s| s.shape(shape_id))
            .ok_or_else(|| reject(format!("shape {shape_id} does not exist")))?;
        let placement = shape
            .placement
            .as_ref()
            .ok_or_else(|| reject("jade cannot place this shape"))?;
        // Everything below depends on the write and the shape's locks only,
        // never on other writes: a refusal is the same on every replica in
        // every order.
        let locks = placement.locks;
        let [x, y, width, height, rotation] = edit.values();
        if (!locks.can_move && (x.is_some() || y.is_some()))
            || (!locks.can_resize && (width.is_some() || height.is_some()))
            || (!locks.can_rotate && rotation.is_some())
        {
            return Err(reject("the shape locks this change"));
        }
        if locks.group && [width, height].iter().flatten().any(|v| *v <= 0) {
            return Err(reject("a group must keep a positive size"));
        }
        check_geometry(Geometry {
            x: x.unwrap_or(0),
            y: y.unwrap_or(0),
            width: width.unwrap_or(0),
            height: height.unwrap_or(0),
            rotation: rotation.unwrap_or(0),
        })?;

        let story = StoryId::Shape {
            slide: slide_id,
            shape: shape_id,
        };
        let mut effects = Vec::new();
        for target in self.reach(story, ctx) {
            let Some(placement) = self
                .slides
                .get_mut(&target.slide())
                .and_then(|s| s.shape_mut(shape_id))
                .and_then(|s| s.placement.as_mut())
            else {
                continue;
            };
            let mut changed = false;
            for (register, value) in placement.registers.iter_mut().zip(edit.values()) {
                if let Some(value) = value {
                    changed |= register.apply(stamp, value);
                }
            }
            let geometry = placement.geometry();
            if let (true, Some((index, TextTarget::Shape(shape)))) =
                (changed, self.realised(target))
            {
                effects.push(PptxEffect::SetTransform {
                    slide: index,
                    shape,
                    geometry,
                });
            }
        }
        self.observe([stamp]);
        Ok(effects)
    }

    fn set_hidden(
        &mut self,
        slide: Id,
        stamp: Id,
        hidden: bool,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        if !self.slides.contains_key(&slide) {
            return Err(reject(format!("slide {slide} does not exist")));
        }
        let mut effects = Vec::new();
        for target in self.reach(StoryId::Notes { slide }, ctx) {
            let Some(s) = self.slides.get_mut(&target.slide()) else {
                continue;
            };
            let before = s.is_hidden();
            s.hidden.apply(stamp, hidden);
            let after = s.is_hidden();
            if let (true, Some(index)) = (before != after, self.slide_index(target.slide())) {
                effects.push(PptxEffect::SetSlideHidden {
                    slide: index,
                    hidden: after,
                });
            }
        }
        self.observe([stamp]);
        Ok(effects)
    }

    fn visible_slide(&self, slide: Id) -> Result<usize, Rejection> {
        match self.slides.get(&slide) {
            None => Err(reject(format!("slide {slide} does not exist"))),
            Some(s) if s.deleted => Err(reject(format!("slide {slide} was deleted"))),
            Some(_) => Ok(self.slide_index(slide).expect("visible slides are ordered")),
        }
    }

    /// The position a slide placed after `after` takes in `order`.
    fn position_after(&self, order: &[Id], after: Option<Id>) -> Result<usize, Rejection> {
        match after {
            None => Ok(0),
            Some(after) => {
                self.visible_slide(after)?;
                order
                    .iter()
                    .position(|s| *s == after)
                    .map(|p| p + 1)
                    .ok_or_else(|| reject("a slide cannot be placed after itself"))
            }
        }
    }

    fn fresh_slide_id(&self, id: Id) -> Result<(), Rejection> {
        if self.slides.contains_key(&id) {
            return Err(reject(format!("slide {id} already exists")));
        }
        Ok(())
    }

    fn insert_slide(&mut self, id: Id, after: Option<Id>) -> Result<Vec<PptxEffect>, Rejection> {
        self.fresh_slide_id(id)?;
        let at = self.position_after(&self.order, after)?;
        // jade copies the layout of the slide now at `at`, or of the last
        // slide when appending.
        let reference = self.order.get(at).or(self.order.last());
        let layout = reference.and_then(|r| self.slides[r].layout.clone());
        let template = self
            .templates
            .iter()
            .find(|t| t.layout == layout)
            .ok_or_else(|| reject("jade's template for this layout is unknown"))?;
        let shapes = template.shapes.clone();
        let next_number = shapes.iter().map(|(_, s)| s.number).max().unwrap_or(1) + 1;
        self.slides.insert(
            id,
            Slide {
                shapes,
                notes: Story::new(Rga::new(), true),
                layout,
                next_number,
                copies: Vec::new(),
                deleted: false,
                hidden: LwwRegister::default(),
            },
        );
        self.order.insert(at, id);
        self.observe([id]);
        Ok(vec![PptxEffect::InsertSlide { at }])
    }

    fn delete_slide(&mut self, slide: Id) -> Result<Vec<PptxEffect>, Rejection> {
        let at = self.visible_slide(slide)?;
        if self.order.len() == 1 {
            return Err(reject("a deck keeps at least one slide"));
        }
        self.order.remove(at);
        self.slides.get_mut(&slide).expect("checked").deleted = true;
        Ok(vec![PptxEffect::DeleteSlide { at }])
    }

    fn move_slide(&mut self, slide: Id, after: Option<Id>) -> Result<Vec<PptxEffect>, Rejection> {
        let from = self.visible_slide(slide)?;
        if after == Some(slide) {
            return Err(reject("a slide cannot be placed after itself"));
        }
        let mut order = self.order.clone();
        order.remove(from);
        let to = self.position_after(&order, after)?;
        order.insert(to, slide);
        self.order = order;
        Ok(if from == to {
            Vec::new()
        } else {
            vec![PptxEffect::MoveSlide { from, to }]
        })
    }

    fn duplicate_slide(
        &mut self,
        source: Id,
        id: Id,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        let at = self.visible_slide(source)?;
        self.fresh_slide_id(id)?;
        let original = &self.slides[&source];
        let copy = Slide {
            shapes: original.shapes.clone(),
            notes: original.notes.clone(),
            layout: original.layout.clone(),
            next_number: original.next_number,
            copies: Vec::new(),
            deleted: false,
            hidden: original.hidden.clone(),
        };
        self.slides.insert(id, copy);
        self.slides
            .get_mut(&source)
            .expect("checked")
            .copies
            .push(CopyMade {
                slide: id,
                origin: ctx.origin,
                seq: ctx.seq,
            });
        self.order.insert(at + 1, id);
        self.observe([id]);
        Ok(vec![PptxEffect::DuplicateSlide {
            at,
            source,
            copy: id,
        }])
    }

    fn insert_text_box(
        &mut self,
        slide_id: Id,
        id: Id,
        geometry: Geometry,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        let index = self.visible_slide(slide_id)?;
        let t = transform(geometry).ok_or_else(|| reject("rotation is out of range"))?;
        jade_pptx_writer::validate_text_box(t, "", &jade_pptx_writer::TextFormat::default())
            .map_err(|e| reject(e.to_string()))?;
        let slide = self.slides.get_mut(&slide_id).expect("checked");
        if slide.shape(id).is_some() {
            return Err(reject(format!("shape {id} already exists")));
        }
        let number = slide.next_number;
        slide.next_number += 1;
        slide.shapes.push((
            id,
            Shape {
                number,
                kind: ShapeKind::Shape,
                group: None,
                text: Some(Story::new(Rga::new(), true)),
                placement: Some(Placement::new(id, geometry, Locks::NONE)),
                deletable: true,
                deleted: false,
            },
        ));
        self.observe([id]);
        Ok(vec![PptxEffect::InsertTextBox {
            slide: index,
            shape: ShapeRef {
                slide: slide_id,
                shape: id,
                number,
            },
            geometry,
        }])
    }

    fn insert_picture(
        &mut self,
        slide_id: Id,
        id: Id,
        geometry: Geometry,
        image: &Image,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        let index = self.visible_slide(slide_id)?;
        check_geometry(geometry)?;
        if geometry.width <= 0 || geometry.height <= 0 {
            return Err(reject("a picture needs a positive size"));
        }
        check_image(&image.0)?;
        let slide = self.slides.get_mut(&slide_id).expect("checked");
        if slide.shape(id).is_some() {
            return Err(reject(format!("shape {id} already exists")));
        }
        let number = slide.next_number;
        slide.next_number += 1;
        slide.shapes.push((
            id,
            Shape {
                number,
                kind: ShapeKind::Picture,
                group: None,
                text: None,
                placement: Some(Placement::new(id, geometry, Locks::PICTURE)),
                deletable: true,
                deleted: false,
            },
        ));
        self.observe([id]);
        Ok(vec![PptxEffect::InsertPicture {
            slide: index,
            shape: ShapeRef {
                slide: slide_id,
                shape: id,
                number,
            },
            geometry,
            image: image.clone(),
        }])
    }

    /// Writes both extent registers, in log order; on copies the author
    /// had not seen too, where the size still allows it.
    fn resize(
        &mut self,
        slide_id: Id,
        shape_id: Id,
        stamp: Id,
        width: i64,
        height: i64,
        ctx: &ApplyContext,
    ) -> Result<Vec<PptxEffect>, Rejection> {
        let shape = self
            .slides
            .get(&slide_id)
            .and_then(|s| s.shape(shape_id))
            .ok_or_else(|| reject(format!("shape {shape_id} does not exist")))?;
        let placement = shape
            .placement
            .as_ref()
            .ok_or_else(|| reject("jade cannot place this shape"))?;
        check_geometry(Geometry {
            x: 0,
            y: 0,
            width,
            height,
            rotation: 0,
        })?;
        let fits = |placement: &Placement| -> Result<(), Rejection> {
            let locks = placement.locks;
            if !locks.can_resize && !locks.aspect {
                return Err(reject("the shape locks its size"));
            }
            if locks.group && (width <= 0 || height <= 0) {
                return Err(reject("a group must keep a positive size"));
            }
            let current = placement.geometry();
            let (w, h) = (i128::from(current.width), i128::from(current.height));
            // jade's own tolerance for a locked ratio.
            if locks.aspect
                && (w * i128::from(height) - h * i128::from(width)).abs() > w.max(0) + h.max(0)
            {
                return Err(reject("the shape's aspect ratio is locked"));
            }
            Ok(())
        };
        fits(placement)?;
        let story = StoryId::Shape {
            slide: slide_id,
            shape: shape_id,
        };
        let mut effects = Vec::new();
        for target in self.reach(story, ctx) {
            let Some(placement) = self
                .slides
                .get_mut(&target.slide())
                .and_then(|s| s.shape_mut(shape_id))
                .and_then(|s| s.placement.as_mut())
            else {
                continue;
            };
            if fits(placement).is_err() {
                continue;
            }
            let changed = placement.registers[2].apply(stamp, width)
                | placement.registers[3].apply(stamp, height);
            let geometry = placement.geometry();
            if let (true, Some((index, TextTarget::Shape(shape)))) =
                (changed, self.realised(target))
            {
                effects.push(PptxEffect::SetTransform {
                    slide: index,
                    shape,
                    geometry,
                });
            }
        }
        self.observe([stamp]);
        Ok(effects)
    }

    fn delete_shape(&mut self, slide_id: Id, shape_id: Id) -> Result<Vec<PptxEffect>, Rejection> {
        let index = self.visible_slide(slide_id)?;
        let slide = self.slides.get_mut(&slide_id).expect("checked");
        let shape = slide
            .shape(shape_id)
            .ok_or_else(|| reject(format!("shape {shape_id} does not exist")))?;
        if shape.deleted {
            return Err(reject(format!("shape {shape_id} was deleted")));
        }
        if !shape.deletable {
            return Err(reject("jade cannot delete this shape"));
        }
        let number = shape.number;
        // The shape and everything grouped under it (groups precede their
        // children in the list).
        let mut doomed = BTreeSet::from([shape_id]);
        for (key, shape) in &slide.shapes {
            if shape.group.is_some_and(|g| doomed.contains(&g)) {
                doomed.insert(*key);
            }
        }
        for (key, shape) in &mut slide.shapes {
            if doomed.contains(key) {
                shape.deleted = true;
            }
        }
        Ok(vec![PptxEffect::DeleteShape {
            slide: index,
            shape: ShapeRef {
                slide: slide_id,
                shape: shape_id,
                number,
            },
        }])
    }

    /// For the materializer's admission check: the slide's index and the
    /// shape's reference, if it names a visible shape.
    pub(crate) fn shape_ref(&self, slide: Id, shape: Id) -> Option<(usize, ShapeRef)> {
        let index = self.slide_index(slide)?;
        let s = self.slides[&slide].shape(shape).filter(|s| !s.deleted)?;
        Some((
            index,
            ShapeRef {
                slide,
                shape,
                number: s.number,
            },
        ))
    }
}

impl Document for PptxModel {
    type Op = PptxOp;
    type Effect = Vec<PptxEffect>;

    fn validate_submission(&self, op: &PptxOp, origin: ReplicaId) -> Result<(), Rejection> {
        use jade_collab_sync::check_id_owner;
        match op {
            PptxOp::InsertText { first, text, .. } => {
                if !text.is_empty() {
                    check_id_owner(*first, origin)?;
                }
            }
            PptxOp::SplitParagraph { id, .. }
            | PptxOp::InsertSlide { id, .. }
            | PptxOp::DuplicateSlide { id, .. }
            | PptxOp::InsertTextBox { id, .. }
            | PptxOp::Mark { id, .. }
            | PptxOp::InsertPicture { id, .. } => check_id_owner(*id, origin)?,
            PptxOp::SetGeometry { stamp, .. }
            | PptxOp::SetSlideHidden { stamp, .. }
            | PptxOp::ResizeShape { stamp, .. } => check_id_owner(*stamp, origin)?,
            PptxOp::DeleteText { .. }
            | PptxOp::JoinParagraph { .. }
            | PptxOp::DeleteSlide { .. }
            | PptxOp::MoveSlide { .. }
            | PptxOp::AlignParagraphs { .. }
            | PptxOp::DeleteShape { .. } => {}
        }
        Ok(())
    }

    fn lane(op: &PptxOp) -> Lane {
        match op {
            PptxOp::InsertText { .. }
            | PptxOp::DeleteText { .. }
            | PptxOp::SplitParagraph { .. }
            | PptxOp::SetGeometry { .. }
            | PptxOp::SetSlideHidden { .. }
            | PptxOp::Mark { .. } => Lane::Convergent,
            PptxOp::JoinParagraph { .. }
            | PptxOp::AlignParagraphs { .. }
            | PptxOp::InsertSlide { .. }
            | PptxOp::DeleteSlide { .. }
            | PptxOp::MoveSlide { .. }
            | PptxOp::DuplicateSlide { .. }
            | PptxOp::InsertTextBox { .. }
            | PptxOp::InsertPicture { .. }
            | PptxOp::ResizeShape { .. }
            | PptxOp::DeleteShape { .. } => Lane::Sequenced,
        }
    }

    fn apply(&mut self, op: &PptxOp, ctx: &ApplyContext) -> Result<Vec<PptxEffect>, Rejection> {
        match op {
            PptxOp::InsertText {
                story,
                parent,
                first,
                text,
            } => self.insert_text(*story, *parent, *first, text, ctx),
            PptxOp::DeleteText { story, ids } => self.delete_text(*story, ids, ctx),
            PptxOp::SplitParagraph { story, parent, id } => self.split(*story, *parent, *id, ctx),
            PptxOp::SetGeometry {
                slide,
                shape,
                stamp,
                edit,
            } => self.set_geometry(*slide, *shape, *stamp, edit, ctx),
            PptxOp::SetSlideHidden {
                slide,
                stamp,
                hidden,
            } => self.set_hidden(*slide, *stamp, *hidden, ctx),
            PptxOp::Mark {
                story,
                id,
                start,
                end,
                format,
            } => self.mark(*story, *id, *start, *end, format, ctx),
            PptxOp::AlignParagraphs {
                story,
                from,
                to,
                alignment,
            } => self.align(*story, *from, *to, *alignment, ctx),
            PptxOp::JoinParagraph { story, mark } => self.join(*story, *mark, ctx),
            PptxOp::InsertSlide { id, after } => self.insert_slide(*id, *after),
            PptxOp::DeleteSlide { slide } => self.delete_slide(*slide),
            PptxOp::MoveSlide { slide, after } => self.move_slide(*slide, *after),
            PptxOp::DuplicateSlide { slide, id } => self.duplicate_slide(*slide, *id, ctx),
            PptxOp::InsertTextBox {
                slide,
                id,
                geometry,
            } => self.insert_text_box(*slide, *id, *geometry),
            PptxOp::DeleteShape { slide, shape } => self.delete_shape(*slide, *shape),
            PptxOp::InsertPicture {
                slide,
                id,
                geometry,
                image,
            } => self.insert_picture(*slide, *id, *geometry, image),
            PptxOp::ResizeShape {
                slide,
                shape,
                stamp,
                width,
                height,
            } => self.resize(*slide, *shape, *stamp, *width, *height, ctx),
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
