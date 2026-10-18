//! Collaborative editing of `.docx` on top of jade.
//!
//! [`DocxModel`] is the replicated model every participant holds;
//! [`DocxOp`]s address characters, marks, rows and columns by identifier.
//! The server realises each committed operation in a jade `WordDocument`
//! through [`JadeDocx`], so the file keeps everything the model does not
//! describe. [`intent`] accepts the paragraph/offset addressing jade's
//! headless API and agents use.

pub mod intent;
mod jade;
mod load;
mod model;

pub use jade::JadeDocx;
pub use jade_collab_core::marks::{Boundary, Expand};
pub use load::LoadError;
pub use model::{
    Anchor, AnchorView, Atom, CommentContent, CommentView, DocxEffect, DocxModel, DocxOp,
    FormatChange, MarkKey, Paragraph, ParagraphAlignment, ParagraphStory, Placement, StoryId,
    TableView, TextFormat,
};
