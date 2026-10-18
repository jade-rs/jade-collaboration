//! Where paragraphs are, without their text.
//!
//! Every operation names atoms by id, and the effects it produces name
//! paragraphs by jade's story position and offsets in their text. Walking the
//! whole model to find one atom's paragraph costs the whole document on every
//! keystroke, so the model keeps the walk's *skeleton* — each visible
//! paragraph in walk order as (story, ending mark, root, index in the root),
//! no text — and finds an atom's paragraph from the atom itself: back to the
//! previous visible mark of its story, forward to the next. That costs the
//! paragraph, not the document.
//!
//! Typing, deleting, tracked changes, marks, sequenced formatting and
//! comments never add, remove or reorder paragraphs, text boxes or notes, so
//! they keep the skeleton as it is. A paragraph split shifts the paragraphs
//! after it, which is done in place when the split paragraph holds no text
//! box or note reference. Everything else drops the skeleton and the next
//! query rebuilds it from the model. `DocxModel::check_index` compares it
//! with one built from scratch, which the property tests do after every
//! operation.

use std::collections::HashMap;
use std::sync::OnceLock;

use jade_collab_core::id::Id;

use super::StoryId;

/// The story whose paragraphs share one numbering in jade's story
/// positions: the body (table cells included), a text box (by its host body
/// paragraph and its index among the host's text boxes) or a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Root {
    Body,
    TextBox { host: usize, index: usize },
    Footnote(Id),
    Endnote(Id),
}

/// One visible paragraph, in walk order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    pub(crate) story: StoryId,
    /// The mark that ends it; `None` for its story's last paragraph.
    pub(crate) mark: Option<Id>,
    pub(crate) root: Root,
    /// Its index among its root's paragraphs.
    pub(crate) index: usize,
}

/// A paragraph's identity: its story and the mark that ends it.
pub(crate) type Key = (StoryId, Option<Id>);

/// The visible paragraphs in walk order, without their content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Skeleton {
    pub(crate) slots: Vec<Slot>,
    /// Every visible table of the body, nested ones included, in pre-order.
    pub(crate) all_tables: Vec<Id>,
    /// Each paragraph's (first, last) ordinal: they differ only when a story
    /// is walked twice (a text box anchored twice).
    ordinals: HashMap<Key, (usize, usize)>,
    repeated: bool,
}

impl Skeleton {
    pub(crate) fn new(slots: Vec<Slot>, all_tables: Vec<Id>) -> Self {
        let mut ordinals: HashMap<Key, (usize, usize)> = HashMap::with_capacity(slots.len());
        let mut repeated = false;
        for (ordinal, slot) in slots.iter().enumerate() {
            ordinals
                .entry((slot.story, slot.mark))
                .and_modify(|(_, last)| {
                    *last = ordinal;
                    repeated = true;
                })
                .or_insert((ordinal, ordinal));
        }
        Self {
            slots,
            all_tables,
            ordinals,
            repeated,
        }
    }

    /// The first ordinal of the paragraph `key` names, if it is visible.
    pub(crate) fn first(&self, key: &Key) -> Option<usize> {
        self.ordinals.get(key).map(|(first, _)| *first)
    }

    /// The last ordinal of the paragraph `key` names, if it is visible —
    /// where a walk-based lookup that keeps the last match finds it.
    pub(crate) fn last(&self, key: &Key) -> Option<usize> {
        self.ordinals.get(key).map(|(_, last)| *last)
    }

    /// Whether `story` is walked: its last paragraph is listed (a story
    /// with a paragraph lists all of them or none).
    pub(crate) fn lists(&self, story: StoryId) -> bool {
        self.ordinals.contains_key(&(story, None))
    }

    /// The paragraph ending with `mark` (an `old` paragraph of `story`, at
    /// most once in the walk, holding no text box or note reference) was
    /// split by the new mark `new`: the first half takes its place, the rest
    /// of its root moves one paragraph on, and so do the text boxes of the
    /// body paragraphs after it. Returns false, changing nothing, when the
    /// split cannot be applied in place.
    pub(crate) fn split(&mut self, story: StoryId, old: Option<Id>, new: Id) -> bool {
        if self.repeated {
            return false;
        }
        let Some(at) = self.first(&(story, old)) else {
            // Not walked: neither half is listed.
            return true;
        };
        let split = self.slots[at].clone();
        let body = split.root == Root::Body;
        for slot in &mut self.slots[at..] {
            if slot.root == split.root {
                slot.index += 1;
            } else if let (true, Root::TextBox { host, .. }) = (body, &mut slot.root) {
                *host += 1;
            }
        }
        self.slots.insert(
            at,
            Slot {
                story,
                mark: Some(new),
                root: split.root,
                index: split.index,
            },
        );
        for (ordinal, slot) in self.slots.iter().enumerate().skip(at) {
            self.ordinals
                .insert((slot.story, slot.mark), (ordinal, ordinal));
        }
        true
    }
}

/// The model's skeleton, built on first use and kept or dropped by every
/// operation. Not part of the model's value: two models are equal, and
/// serialize alike, whatever their caches hold.
#[derive(Default)]
pub(crate) struct IndexCache(OnceLock<Skeleton>);

impl IndexCache {
    pub(crate) fn get_or_build(&self, build: impl FnOnce() -> Skeleton) -> &Skeleton {
        self.0.get_or_init(build)
    }

    pub(crate) fn get(&self) -> Option<&Skeleton> {
        self.0.get()
    }

    pub(crate) fn take(&mut self) -> Option<Skeleton> {
        self.0.take()
    }

    pub(crate) fn put(&mut self, skeleton: Skeleton) {
        self.0 = OnceLock::from(skeleton);
    }

    pub(crate) fn invalidate(&mut self) {
        self.0.take();
    }
}

impl Clone for IndexCache {
    fn clone(&self) -> Self {
        Self(match self.0.get() {
            Some(skeleton) => OnceLock::from(skeleton.clone()),
            None => OnceLock::new(),
        })
    }
}

impl PartialEq for IndexCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for IndexCache {}

impl std::fmt::Debug for IndexCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IndexCache")
    }
}
