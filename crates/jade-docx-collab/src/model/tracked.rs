//! Tracked changes: revisions as attributes of characters.
//!
//! A character carries the tracked insertion that made it (until it is
//! accepted) and the set of tracked deletions that named it. The set only
//! grows under the convergent lane — a union, so concurrent deletions
//! commute — and Word shows the least revision of it. Decisions are
//! sequenced and name whole revisions; every one is a removal keyed by a
//! revision id, so it commutes with every convergent operation that does
//! not create that revision (`lean/JadeCollab/Revision.lean`).

use std::collections::{BTreeMap, BTreeSet};

use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::Rejection;

use super::{
    reject, Atom, DocxEffect, DocxModel, Revision, RevisionKind, RevisionView, StoryId, Tracked,
};

impl DocxModel {
    /// Whether edits made through the intent API are tracked.
    pub fn tracking(&self) -> bool {
        self.tracking
    }

    /// The name the server gave `replica`.
    pub fn author(&self, replica: ReplicaId) -> Option<&str> {
        self.authors.get(&replica).map(String::as_str)
    }

    /// A revision by id, decided ones included.
    pub fn revision(&self, id: Id) -> Option<&Revision> {
        self.revisions.get(&id)
    }

    /// `character`'s tracked changes as Word shows them.
    pub(crate) fn tracked(&self, character: Id) -> Tracked {
        Tracked {
            inserted: self.inserted.get(&character).copied(),
            deleted: self
                .deleted
                .get(&character)
                .and_then(|set| set.first())
                .copied(),
        }
    }

    /// Every revision with characters left in the document, by id.
    pub fn revisions(&self) -> Vec<RevisionView> {
        let mut found: BTreeMap<Id, (String, Vec<usize>)> = BTreeMap::new();
        for (ordinal, paragraph) in self.paragraphs().into_iter().enumerate() {
            for (c, id) in paragraph.text.chars().zip(&paragraph.chars) {
                let deletions = self.deleted.get(id).into_iter().flatten();
                for revision in self.inserted.get(id).into_iter().chain(deletions) {
                    let (text, paragraphs) = found.entry(*revision).or_default();
                    text.push(c);
                    if paragraphs.last() != Some(&ordinal) {
                        paragraphs.push(ordinal);
                    }
                }
            }
        }
        found
            .into_iter()
            .filter_map(|(id, (text, paragraphs))| {
                Some(RevisionView {
                    id,
                    revision: self.revisions.get(&id)?.clone(),
                    text,
                    paragraphs,
                })
            })
            .collect()
    }

    fn new_revision(
        &mut self,
        id: Id,
        kind: RevisionKind,
        date: &Option<String>,
        origin: ReplicaId,
    ) {
        let revision = Revision {
            kind,
            author: self.authors.get(&origin).cloned(),
            date: date.clone(),
            replica: origin,
            file_id: None,
            moved: false,
        };
        self.revisions.insert(id, revision);
        self.observe([id]);
    }

    pub(super) fn track_insert(
        &mut self,
        story: StoryId,
        parent: Option<Id>,
        first: Id,
        text: &str,
        date: &Option<String>,
        origin: ReplicaId,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if self.revisions.contains_key(&first) {
            return Err(reject(format!("revision {first} already exists")));
        }
        let effects = self.insert_text(story, parent, first, text)?;
        self.new_revision(first, RevisionKind::Insertion, date, origin);
        let count = text.chars().count() as u64;
        for k in 0..count {
            self.inserted.insert(first.offset(k), first);
        }
        let author = self.revisions[&first].author.clone();
        Ok(effects
            .into_iter()
            .map(|effect| match effect {
                DocxEffect::InsertText {
                    paragraph,
                    offset,
                    text,
                    anchors,
                } => DocxEffect::TrackInsertText {
                    paragraph,
                    offset,
                    text,
                    anchors,
                    author: author.clone(),
                    date: date.clone(),
                },
                other => other,
            })
            .collect())
    }

    pub(super) fn track_delete(
        &mut self,
        story: StoryId,
        ids: &[Id],
        revision: Id,
        date: &Option<String>,
        origin: ReplicaId,
    ) -> Result<Vec<DocxEffect>, Rejection> {
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
        if self.revisions.contains_key(&revision) {
            return Err(reject(format!("revision {revision} already exists")));
        }
        // What Word shows before and after: where this deletion becomes the
        // least one, jade must show it — after giving back the characters a
        // later one had (jade keeps a character's first deleter).
        let before: BTreeMap<Id, Option<Id>> = ids
            .iter()
            .map(|id| (*id, self.tracked(*id).deleted))
            .collect();
        self.new_revision(revision, RevisionKind::Deletion, date, origin);
        for id in ids {
            self.deleted.entry(*id).or_default().insert(revision);
        }
        let mut taken = BTreeSet::new();
        let mut shown = BTreeSet::new();
        for (id, previous) in before {
            if self.tracked(id).deleted == Some(revision) {
                shown.insert(id);
                if previous.is_some() {
                    taken.insert(id);
                }
            }
        }
        let mut effects = self.restoring(&taken);
        effects.extend(self.deleting(&shown, revision));
        Ok(effects)
    }

    /// jade gives back the deletion it shows on the visible characters
    /// among `ids`.
    fn restoring(&self, ids: &BTreeSet<Id>) -> Vec<DocxEffect> {
        let paragraphs = self.paragraphs();
        self.ranges(ids)
            .into_iter()
            .map(|(p, start, end, _)| DocxEffect::RestoreRange {
                paragraph: paragraphs[p].at.clone(),
                start,
                end,
            })
            .collect()
    }

    /// jade shows the visible characters among `ids` as deleted by
    /// `revision`.
    fn deleting(&self, ids: &BTreeSet<Id>, revision: Id) -> Vec<DocxEffect> {
        let paragraphs = self.paragraphs();
        let made = &self.revisions[&revision];
        self.ranges(ids)
            .into_iter()
            .map(|(p, start, end, anchors)| DocxEffect::TrackDeleteRange {
                paragraph: paragraphs[p].at.clone(),
                start,
                end,
                anchors,
                author: made.author.clone(),
                date: made.date.clone(),
            })
            .collect()
    }

    pub(super) fn set_author(
        &mut self,
        replica: ReplicaId,
        name: &str,
        origin: ReplicaId,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if origin != ReplicaId::ORIGIN {
            return Err(reject("only the server names authors"));
        }
        if name.trim().is_empty() {
            return Err(reject("an author needs a name"));
        }
        self.authors.insert(replica, name.to_string());
        Ok(Vec::new())
    }

    /// Tombstones `ids` wherever they are (already removed ones stay so).
    fn remove_characters(&mut self, ids: &BTreeSet<Id>) {
        let mut left = ids.clone();
        let mut stories = vec![StoryId::Body];
        for (table, entry) in &self.tables {
            stories.extend(
                entry
                    .stories
                    .keys()
                    .map(|key| super::story_id(*table, *key)),
            );
        }
        stories.extend(
            self.footnotes
                .keys()
                .map(|id| StoryId::Footnote { id: *id }),
        );
        stories.extend(self.endnotes.keys().map(|id| StoryId::Endnote { id: *id }));
        stories.extend(
            self.text_boxes
                .keys()
                .map(|id| StoryId::TextBox { id: *id }),
        );
        for story in stories {
            if left.is_empty() {
                break;
            }
            let flow = &mut self.story_mut(story).expect("listed").flow;
            left.retain(|id| flow.delete(*id).is_err());
        }
    }

    /// Accepts (`accept`) or rejects whole revisions, one after the other.
    pub(super) fn decide(
        &mut self,
        revisions: &[Id],
        accept: bool,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if revisions.is_empty() {
            return Err(reject("name the revisions to decide"));
        }
        if let Some(unknown) = revisions.iter().find(|r| !self.revisions.contains_key(r)) {
            return Err(reject(format!("revision {unknown} does not exist")));
        }
        let mut effects = Vec::new();
        let mut done = BTreeSet::new();
        for revision in revisions {
            if !done.insert(*revision) {
                continue;
            }
            match (self.revisions[revision].kind, accept) {
                (RevisionKind::Insertion, true) => {
                    let ids = self.inserted_by(*revision);
                    let paragraphs = self.paragraphs();
                    effects.extend(self.ranges(&ids).into_iter().map(|(p, start, end, _)| {
                        DocxEffect::AcceptInsertionRange {
                            paragraph: paragraphs[p].at.clone(),
                            start,
                            end,
                        }
                    }));
                    self.inserted.retain(|_, r| r != revision);
                }
                (RevisionKind::Insertion, false) => {
                    let ids = self.inserted_by(*revision);
                    effects.extend(self.removal(&ids));
                    self.remove_characters(&ids);
                }
                (RevisionKind::Deletion, true) => {
                    let ids = self.deleted_by(*revision);
                    effects.extend(self.removal(&ids));
                    self.remove_characters(&ids);
                }
                (RevisionKind::Deletion, false) => {
                    let ids = self.deleted_by(*revision);
                    let shown: BTreeSet<Id> = ids
                        .iter()
                        .filter(|id| self.tracked(**id).deleted == Some(*revision))
                        .copied()
                        .collect();
                    for id in &ids {
                        if let Some(set) = self.deleted.get_mut(id) {
                            set.remove(revision);
                            if set.is_empty() {
                                self.deleted.remove(id);
                            }
                        }
                    }
                    // Where it was shown, jade gives the characters back and
                    // the next deletion in line, if any, takes them.
                    effects.extend(self.restoring(&shown));
                    let mut next: BTreeMap<Id, BTreeSet<Id>> = BTreeMap::new();
                    for id in shown {
                        if let Some(by) = self.tracked(id).deleted {
                            next.entry(by).or_default().insert(id);
                        }
                    }
                    for (by, ids) in next {
                        effects.extend(self.deleting(&ids, by));
                    }
                }
            }
        }
        Ok(effects)
    }

    fn inserted_by(&self, revision: Id) -> BTreeSet<Id> {
        self.inserted
            .iter()
            .filter(|(_, r)| **r == revision)
            .map(|(id, _)| *id)
            .collect()
    }

    fn deleted_by(&self, revision: Id) -> BTreeSet<Id> {
        self.deleted
            .iter()
            .filter(|(_, set)| set.contains(&revision))
            .map(|(id, _)| *id)
            .collect()
    }
}
