//! Collaborative editing of `.docx` on top of jade.
//!
//! [`DocxModel`] is the replicated model every participant holds;
//! [`DocxOp`]s address characters, marks, rows and columns by identifier.
//! The server realises each committed operation in a jade `WordDocument`
//! through [`JadeDocx`], so the file keeps everything the model does not
//! describe. [`intent`] accepts the paragraph/offset addressing jade's
//! headless API and agents use.

#[cfg(test)]
extern crate self as jade_docx_collab;

mod bindings;
mod inspection;
pub mod intent;
mod jade;
mod load;
mod model;
mod revision_metadata;
mod settings;
mod table_budget;

pub use inspection::{CharacterStyle, CharacterStyleRun, DocxFormatting, ParagraphFormat};
pub use jade::JadeDocx;
pub use jade_collab_core::marks::{Boundary, Expand};
pub use load::LoadError;
pub use model::{
    Anchor, AnchorView, Atom, CommentContent, CommentView, DocxEffect, DocxModel, DocxOp,
    FormatChange, MarkKey, NoteKind, Numbered, Paragraph, ParagraphAlignment, ParagraphStory,
    Placement, Revision, RevisionKind, RevisionView, StoryId, TableView, TextCursor, TextFormat,
    Tracked,
};
pub use table_budget::{TableBudgetExceeded, MAX_TABLE_CELLS_PER_OPERATION};
