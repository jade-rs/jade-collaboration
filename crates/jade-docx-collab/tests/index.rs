//! The model's paragraph index — kept across operations instead of walking
//! the document for every one — against the walk it replaces: after every
//! operation of random histories (in any story, notes, text boxes, tables
//! and tracked changes included, some generated on a stale view so they land
//! after concurrent edits), the kept index equals one built from scratch and
//! every atom is found where the full walk finds it
//! (`DocxModel::check_index`).

mod common;

use common::{generate, generate_through_merges, open};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_docx_collab::{DocxModel, DocxOp, NoteKind, Numbered};
use proptest::prelude::*;

const FIXTURES: &[&str] = &[
    "jade-core/hello.docx",
    "table-cell-spacing.docx",
    "footnote-table-textbox-order.docx",
    "table-complex-nested-merges.docx",
    "comment.docx",
    "footnote.docx",
    "footnote-long-continuation.docx",
    "endnote.docx",
    "textbox.docx",
    "floating-wrap-sides.docx",
    "merged-table-over-page.docx",
    "table-complex-merged-continuations.docx",
];

fn checked(model: &DocxModel, context: &str) -> Result<(), TestCaseError> {
    model
        .check_index()
        .map_err(|e| TestCaseError::fail(format!("{context}: {e}")))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn the_kept_paragraph_index_matches_the_walk_after_every_operation(
        fixture_index in 0..FIXTURES.len(),
        steps in prop::collection::vec((any::<u64>(), 0u8..4), 1..40),
    ) {
        let name = FIXTURES[fixture_index];
        let (mut model, _) = open(name);
        checked(&model, &format!("{name} on load"))?;
        let mut stale = model.clone();
        let mut next = 1_000_000u64;
        let mut numbers = 900u32;
        for (step, (choice, mode)) in steps.into_iter().enumerate() {
            let mut ids = |n: u64| {
                let id = Id::new(next, ReplicaId(1));
                next += n;
                id
            };
            // Mostly the current view; now and then an older one (a
            // concurrent replica's), or table structure.
            let op = match mode {
                0 => generate(&stale, choice, &mut ids),
                1 => generate_through_merges(&model, choice, &mut ids),
                _ => generate(&model, choice, &mut ids),
            };
            let Some(op) = op else { continue };
            let ctx = ApplyContext { origin: ReplicaId(1), base_seq: 0, seq: Some(step as u64 + 1) };
            let applied = model.apply(&op, &ctx).is_ok();
            checked(&model, &format!("{name} after {op:?} (applied: {applied})"))?;
            // The server numbers new notes; that reorders the notes jade
            // lists by number.
            if let (true, DocxOp::InsertNote { kind, id, .. }) = (applied, &op) {
                numbers += 1;
                let item = match kind {
                    NoteKind::Footnote => Numbered::Footnote { id: *id },
                    NoteKind::Endnote => Numbered::Endnote { id: *id },
                };
                let origin = ApplyContext { origin: ReplicaId::ORIGIN, ..ctx };
                let numbered = DocxOp::Numbered { item, file_id: numbers.to_string() };
                model.apply(&numbered, &origin).expect("the server numbers a new note");
                checked(&model, &format!("{name} after {numbered:?}"))?;
            }
            if step % 4 == 3 {
                stale = model.clone();
            }
        }
    }
}

/// Splitting a paragraph keeps the index in place (no rebuild) and shifts
/// what follows, in the body, in a table cell and in a note.
#[test]
fn splits_shift_the_paragraphs_after_them() {
    for name in [
        "footnote-table-textbox-order.docx",
        "table-cell-spacing.docx",
    ] {
        let (mut model, _) = open(name);
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: 0,
            seq: Some(1),
        };
        let paragraphs = model.paragraphs();
        let mut next = 1_000_000u64;
        for (k, paragraph) in paragraphs.iter().enumerate() {
            if k % 2 == 1 {
                continue;
            }
            let parent = paragraph.chars.first().copied();
            let op = DocxOp::SplitParagraph {
                story: paragraph.story,
                parent,
                id: Id::new(next, ReplicaId(1)),
            };
            next += 1;
            let _ = model.apply(&op, &ctx);
            model
                .check_index()
                .unwrap_or_else(|e| panic!("{name} after {op:?}: {e}"));
        }
    }
}
