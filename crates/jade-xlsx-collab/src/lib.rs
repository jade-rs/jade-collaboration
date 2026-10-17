//! Collaborative editing of `.xlsx` on top of jade.
//!
//! Cells are last-writer-wins registers keyed by stable row and column
//! identifiers, so concurrent value edits commute (convergent lane). Row and
//! column insertion and deletion, and formulas, go through the sequencer:
//! a structural change moves every formula's references with jade's own
//! `shift_formula`, and a formula written against an older grid is moved
//! through the changes its author had not seen — the same function, applied
//! in log order on every replica.

mod jade;
mod model;

pub use jade::JadeXlsx;
pub use jade_xlsx_headless_api::CellInput;
pub use model::{
    column_label, parse_reference, LoadError, LoadOptions, SheetView, XlsxEffect, XlsxModel, XlsxOp,
};
