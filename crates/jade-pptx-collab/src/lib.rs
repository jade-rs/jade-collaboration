//! Collaborative editing of `.pptx` on top of jade.
//!
//! [`PptxModel`] is the replicated model every participant holds: slides as
//! an ordered list of ids, every text body (shapes and speaker notes) an RGA
//! of characters and paragraph marks, top-level shapes' geometry as
//! per-attribute last-writer-wins registers. [`PptxOp`]s address all of it
//! by identifier. The server realises each committed operation through
//! jade's pptx writer with [`JadePptx`], so the file keeps everything the
//! model does not describe. [`intent`] accepts the slide-number / shape-id
//! addressing jade's headless tools and agents use.

mod inspection;
pub mod intent;
mod jade;
mod load;
mod model;

pub use inspection::{
    CharacterStyle, CharacterStyleRun, ParagraphFormat, ParagraphStyle, PptxFormatting,
    ShapeFormatting, SlideFormatting, Spacing,
};
pub use jade::JadePptx;
pub use load::LoadError;
pub use model::MAX_PICTURE_BYTES;
pub use model::{
    Alignment, Atom, CharFormat, FormatRun, Geometry, GeometryEdit, Image, MarkKey, Paragraph,
    PptxEffect, PptxModel, PptxOp, ShapeKind, ShapeRef, ShapeView, SlideView, StoryId, TextTarget,
};
