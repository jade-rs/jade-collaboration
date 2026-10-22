//! Format-neutral collaboration primitives.
//!
//! Everything here is addressed by stable [`id::Id`]s rather than indices, so
//! an operation computed against an old view keeps its meaning on a newer
//! state. See `docs/design.md` for how these pieces fit the two-lane
//! (convergent / sequenced) protocol.

pub mod grid;
pub mod id;
pub mod lww;
pub mod marks;
pub mod rga;

pub mod text;
