//! An optional runtime index for consecutive text insertions. Structural
//! operations use the canonical walk and discard the index. It is not logical
//! state: clones and deserialized models start cold, equality ignores it.
use super::{Atom, DocxModel, Id, Located, Paragraph, ParagraphStory, Rga, StoryId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct Positions(Mutex<Cache>);
#[derive(Default)]
struct Cache {
    enabled: bool,
    unsupported: bool,
    index: Option<Index>,
}
impl Clone for Positions {
    fn clone(&self) -> Self {
        Self::default()
    }
}
impl PartialEq for Positions {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Positions {}
impl std::fmt::Debug for Positions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Positions")
    }
}
impl Positions {
    pub(super) fn enable(&self) {
        #[cfg(test)]
        if super::position_tests::forced_walk() {
            self.disable();
            return;
        }
        self.0.lock().unwrap().enabled = true;
    }
    pub(super) fn disable(&self) {
        *self.0.lock().unwrap() = Cache::default();
    }
    fn ensure(cache: &mut Cache, model: &DocxModel) {
        if !cache.enabled || cache.unsupported || cache.index.is_some() {
            return;
        }
        cache.index = Index::build(model.paragraphs());
        cache.unsupported = cache.index.is_none();
    }
    // Do not construct a cold projection before RGA insertion validation.
    pub(super) fn take(&self) -> Option<Index> {
        self.0.lock().unwrap().index.take()
    }
    pub(super) fn restore(&self, index: Option<Index>) {
        self.0.lock().unwrap().index = index;
    }
    pub(super) fn put(&self, index: Option<Index>) {
        let mut cache = self.0.lock().unwrap();
        if cache.enabled && index.is_none() {
            cache.unsupported = true;
        }
        cache.index = index;
    }
    pub(super) fn locate(
        &self,
        model: &DocxModel,
        ids: &BTreeSet<Id>,
    ) -> Option<BTreeMap<Id, Located>> {
        let mut cache = self.0.lock().unwrap();
        Self::ensure(&mut cache, model);
        cache.index.as_ref().map(|index| {
            ids.iter()
                .filter_map(|id| {
                    index
                        .atoms
                        .get(id)
                        .filter(|entry| !matches!(entry.kind, Kind::Anchor))
                        .map(|entry| (*id, entry.at))
                })
                .collect()
        })
    }
    pub(super) fn paragraph(&self, model: &DocxModel, ordinal: usize) -> Option<ParagraphStory> {
        let mut cache = self.0.lock().unwrap();
        Self::ensure(&mut cache, model);
        cache
            .index
            .as_ref()
            .map(|index| index.paragraphs[ordinal].at.clone())
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Character,
    Anchor,
    Mark,
}
struct Entry {
    at: Located,
    kind: Kind,
}
struct IndexedParagraph {
    at: ParagraphStory,
    ids: Vec<Id>,
    next: Option<usize>,
}
pub(super) struct Index {
    atoms: BTreeMap<Id, Entry>,
    paragraphs: Vec<IndexedParagraph>,
    first: BTreeMap<StoryId, usize>,
}
impl Index {
    fn build(paragraphs: Vec<Paragraph>) -> Option<Self> {
        let mut index = Self {
            atoms: BTreeMap::new(),
            paragraphs: Vec::new(),
            first: BTreeMap::new(),
        };
        let mut last = BTreeMap::new();
        for (ordinal, paragraph) in paragraphs.into_iter().enumerate() {
            index.first.entry(paragraph.story).or_insert(ordinal);
            if let Some(previous) = last.insert(paragraph.story, ordinal) {
                index.paragraphs[previous].next = Some(ordinal);
            }
            let mut ids = Vec::new();
            let mut anchors = 0;
            for ((offset, ch), id) in paragraph.text.char_indices().zip(paragraph.chars) {
                while anchors < paragraph.anchors.len()
                    && paragraph.anchors[anchors].offset <= offset
                {
                    anchors += 1;
                }
                let at = Located {
                    ordinal,
                    offset,
                    len: ch.len_utf8(),
                    anchors,
                };
                if index
                    .atoms
                    .insert(
                        id,
                        Entry {
                            at,
                            kind: Kind::Character,
                        },
                    )
                    .is_some()
                {
                    return None;
                }
                ids.push(id);
            }
            for (number, anchor) in paragraph.anchors.iter().enumerate() {
                let at = Located {
                    ordinal,
                    offset: anchor.offset,
                    len: 0,
                    anchors: number + 1,
                };
                if index
                    .atoms
                    .insert(
                        anchor.id,
                        Entry {
                            at,
                            kind: Kind::Anchor,
                        },
                    )
                    .is_some()
                {
                    return None;
                }
                ids.push(anchor.id);
            }
            if let Some(mark) = paragraph.mark {
                let at = Located {
                    ordinal,
                    offset: paragraph.text.len(),
                    len: 0,
                    anchors: paragraph.anchors.len(),
                };
                if index
                    .atoms
                    .insert(
                        mark,
                        Entry {
                            at,
                            kind: Kind::Mark,
                        },
                    )
                    .is_some()
                {
                    return None;
                }
                ids.push(mark);
            }
            index.paragraphs.push(IndexedParagraph {
                at: paragraph.at,
                ids,
                next: None,
            });
        }
        Some(index)
    }
    /// Resolve the actual RGA predecessor after insertion, then shift only
    /// this paragraph's suffix. Anchors at the insertion byte position are
    /// ordered by their own RGA positions, not merely by byte offset.
    pub(super) fn inserted(
        mut self,
        story: StoryId,
        first: Id,
        text: &str,
        flow: &Rga<Atom>,
    ) -> Option<Self> {
        if text
            .chars()
            .enumerate()
            .any(|(i, _)| self.atoms.contains_key(&first.offset(i as u64)))
        {
            return None;
        }
        let Some(&start) = self.first.get(&story) else {
            return Some(self);
        };
        let (ordinal, offset, anchors) = match flow.visible_before(first) {
            None => (start, 0, 0),
            Some(previous) => {
                let entry = self.atoms.get(&previous)?;
                match entry.kind {
                    Kind::Mark => (self.paragraphs[entry.at.ordinal].next?, 0, 0),
                    Kind::Character => (
                        entry.at.ordinal,
                        entry.at.offset + entry.at.len,
                        entry.at.anchors,
                    ),
                    Kind::Anchor => (entry.at.ordinal, entry.at.offset, entry.at.anchors),
                }
            }
        };
        #[cfg(test)]
        super::position_tests::repairing(self.paragraphs[ordinal].ids.len());
        for id in &self.paragraphs[ordinal].ids {
            let entry = self.atoms.get_mut(id).expect("indexed paragraph atom");
            let follows = match entry.kind {
                Kind::Anchor => {
                    entry.at.offset > offset
                        || (entry.at.offset == offset && entry.at.anchors > anchors)
                }
                Kind::Character | Kind::Mark => entry.at.offset >= offset,
            };
            if follows {
                entry.at.offset += text.len();
            }
        }
        for (i, (delta, ch)) in text.char_indices().enumerate() {
            let id = first.offset(i as u64);
            let at = Located {
                ordinal,
                offset: offset + delta,
                len: ch.len_utf8(),
                anchors,
            };
            self.atoms.insert(
                id,
                Entry {
                    at,
                    kind: Kind::Character,
                },
            );
            self.paragraphs[ordinal].ids.push(id);
        }
        Some(self)
    }
}
