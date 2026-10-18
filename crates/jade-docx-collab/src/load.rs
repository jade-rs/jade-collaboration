//! Building the model from a jade `WordDocument`.

use std::collections::BTreeMap;

use jade_collab_core::grid::{Grid, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::rga::Rga;
use jade_docx_document_core::document::{BodyElement, Table as JadeTable};
use jade_docx_document_core::headless_api::execute_headless_queries_on_document;
use jade_docx_document_core::word_document::WordDocument;
use jade_docx_headless_api::{HeadlessQuery, HeadlessQueryRequest, HeadlessQueryResult};

use crate::model::{
    Anchor, Atom, Block, Comment, CommentContent, DocxModel, Note, Placed, Placement, Story, Table,
};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("the document has no parsed body")]
    NoBody,
}

struct Loader {
    next: u64,
    tables: BTreeMap<Id, Table>,
    footnotes: BTreeMap<Id, Note>,
    endnotes: BTreeMap<Id, Note>,
    text_boxes: BTreeMap<Id, Story>,
    /// Model ids of the notes by `w:id`, footnotes and endnotes apart.
    note_ids: BTreeMap<(bool, String), Id>,
}

impl Loader {
    fn id(&mut self) -> Id {
        let id = Id::new(self.next, ReplicaId::ORIGIN);
        self.next += 1;
        id
    }

    /// The model id of footnote (`footnote`) or endnote `file_id`; a
    /// reference to a note the file does not have gets an empty one.
    fn note(&mut self, footnote: bool, file_id: &str) -> Id {
        if let Some(id) = self.note_ids.get(&(footnote, file_id.to_string())) {
            return *id;
        }
        let id = self.id();
        self.note_ids.insert((footnote, file_id.to_string()), id);
        let note = Note {
            file_id: file_id.to_string(),
            story: Story {
                has_paragraph: false,
                ..Story::empty_paragraph()
            },
        };
        if footnote {
            self.footnotes.insert(id, note);
        } else {
            self.endnotes.insert(id, note);
        }
        id
    }

    /// Mirrors jade's surface enumeration: paragraphs in order, a table's
    /// cells row by row, recursing into cell blocks. A run's text box and
    /// note references become anchors before its text; `listed` says
    /// whether jade lists this story's text boxes (the body's and its
    /// cells' — not a text box's or a note's), which then get stories of
    /// their own.
    fn story(&mut self, blocks: &[BodyElement], top_level: bool, listed: bool) -> Story {
        let last_paragraph = blocks
            .iter()
            .rposition(|b| matches!(b, BodyElement::Paragraph(_)));
        let mut flow = Rga::new();
        let mut previous = None;
        let mut anchor = Anchor::Start;
        let mut placed = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            match block {
                BodyElement::Paragraph(paragraph) => {
                    let mut atoms: Vec<Atom> = Vec::new();
                    for run in &paragraph.runs {
                        if let Some(text_box) = &run.text_box {
                            let id = self.id();
                            let story = if listed {
                                self.story(&text_box.blocks, false, false)
                            } else {
                                Story {
                                    has_paragraph: false,
                                    ..Story::empty_paragraph()
                                }
                            };
                            self.text_boxes.insert(id, story);
                            atoms.push(Atom::Anchor(Placement::TextBox(id)));
                        }
                        for note in &run.footnote_reference_ids {
                            atoms.push(Atom::Anchor(Placement::Footnote(self.note(true, note))));
                        }
                        for note in &run.endnote_reference_ids {
                            atoms.push(Atom::Anchor(Placement::Endnote(self.note(false, note))));
                        }
                        atoms.extend(run.text.chars().map(Atom::Char));
                    }
                    if !atoms.is_empty() {
                        let count = atoms.len() as u64;
                        let first = Id::new(self.next, ReplicaId::ORIGIN);
                        self.next += count;
                        flow.insert_run(first, previous, atoms).expect("fresh ids");
                        previous = Some(first.offset(count - 1));
                    }
                    if Some(index) == last_paragraph {
                        anchor = Anchor::End;
                    } else {
                        let mark = self.id();
                        flow.insert(mark, previous, Atom::Mark).expect("fresh id");
                        previous = Some(mark);
                        anchor = Anchor::After(mark);
                    }
                }
                BodyElement::Table(table) => {
                    let id = self.table(table, top_level, listed);
                    placed.push(Placed {
                        anchor,
                        block: Block::Table(id),
                    });
                }
                BodyElement::Math(_) => placed.push(Placed {
                    anchor,
                    block: Block::Opaque,
                }),
            }
        }
        Story {
            flow,
            blocks: placed,
            has_paragraph: last_paragraph.is_some(),
        }
    }

    fn table(&mut self, table: &JadeTable, top_level: bool, listed: bool) -> Id {
        let id = self.id();
        match regular_layout(table).filter(|_| top_level) {
            Some((width, merges)) => {
                let rows: Vec<Id> = table.rows.iter().map(|_| self.id()).collect();
                let columns: Vec<Id> = (0..width).map(|_| self.id()).collect();
                let mut model = Table::new(&[], &[], false, top_level);
                for (r, row) in table.rows.iter().enumerate() {
                    let mut c = 0;
                    for cell in &row.cells {
                        let story = self.story(&cell.blocks, false, listed);
                        model.add_cell(rows[r], columns[c], story);
                        c += span(cell);
                    }
                }
                let merges = merges
                    .into_iter()
                    .map(|(top, left, bottom, right)| Merge {
                        top: rows[top],
                        left: columns[left],
                        bottom: rows[bottom],
                        right: columns[right],
                    })
                    .collect();
                model.grid = Grid::with_merges_unchecked(rows, columns, merges);
                // Merges the file got wrong (overlapping) keep the table's
                // structure read-only.
                model.locked = model.grid.check_invariants().is_err();
                self.tables.insert(id, model);
            }
            None => {
                // Ragged, nested or not at the top level: cell text is
                // editable, structure is not. Cells are placed by their
                // index in the row, which is the order jade lists them in.
                let width = table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
                let rows: Vec<Id> = table.rows.iter().map(|_| self.id()).collect();
                let columns: Vec<Id> = (0..width.max(1)).map(|_| self.id()).collect();
                let mut model = Table::new(&[], &[], true, top_level);
                for (row, jade_row) in rows.iter().zip(&table.rows) {
                    for (column, cell) in columns.iter().zip(&jade_row.cells) {
                        let story = self.story(&cell.blocks, false, listed);
                        model.add_cell(*row, *column, story);
                    }
                }
                model.grid = Grid::new(rows, columns);
                self.tables.insert(id, model);
            }
        }
        id
    }
}

fn span(cell: &jade_docx_document_core::document::TableCell) -> usize {
    cell.span.unwrap_or(1).max(1) as usize
}

/// (top, left, bottom, right) indices of a merged region.
type Region = (usize, usize, usize, usize);

/// For a table whose rows all fill the same grid with cells holding only
/// paragraphs: the grid width and its merged regions as (top, left, bottom,
/// right) indices — `w:gridSpan` across, `w:vMerge` restart/continue down.
/// `None` for anything else.
fn regular_layout(table: &JadeTable) -> Option<(usize, Vec<Region>)> {
    let width = table.columns.len();
    if table.rows.is_empty() || width == 0 {
        return None;
    }
    let mut merges: Vec<Region> = Vec::new();
    for (r, row) in table.rows.iter().enumerate() {
        if row.grid_before.unwrap_or(0) != 0 || row.grid_after.unwrap_or(0) != 0 {
            return None;
        }
        let mut c = 0;
        for cell in &row.cells {
            if !cell
                .blocks
                .iter()
                .all(|b| matches!(b, BodyElement::Paragraph(_)))
            {
                return None;
            }
            let right = c + span(cell) - 1;
            match cell.vertical_merge.as_deref() {
                Some("continue") => {
                    let above = merges
                        .iter_mut()
                        .find(|m| m.1 == c && m.3 == right && m.2 + 1 == r)?;
                    above.2 = r;
                }
                Some(_) => merges.push((r, c, r, right)),
                None if right > c => merges.push((r, c, r, right)),
                None => {}
            }
            c = right + 1;
        }
        if c != width {
            return None;
        }
    }
    merges.retain(|(top, left, bottom, right)| (bottom - top + 1) * (right - left + 1) >= 2);
    Some((width, merges))
}

impl Loader {
    /// The file's comments, as jade lists them, each with a model id and its
    /// jade `w:id` so operations can name it.
    fn comments(&mut self, word: &WordDocument) -> BTreeMap<Id, Comment> {
        let request = HeadlessQueryRequest {
            queries: vec![HeadlessQuery::Comments {
                start_comment: 0,
                max_comments: usize::MAX,
                author: None,
                resolved: None,
            }],
        };
        let listed = match execute_headless_queries_on_document(word, &request)
            .map(|mut response| response.results.remove(0))
        {
            Ok(HeadlessQueryResult::Comments { comments, .. }) => comments,
            _ => Vec::new(),
        };
        let mut ids: BTreeMap<String, Id> = BTreeMap::new();
        let mut unique = Vec::new();
        for comment in listed {
            // A malformed file may reuse an id; jade addresses only one.
            if !ids.contains_key(&comment.id) {
                ids.insert(comment.id.clone(), self.id());
                unique.push(comment);
            }
        }
        unique
            .into_iter()
            .map(|comment| {
                let parent = comment
                    .reply_to_comment_id
                    .as_ref()
                    .and_then(|p| ids.get(p))
                    .copied();
                (
                    ids[&comment.id],
                    Comment {
                        parent,
                        anchor: None,
                        content: CommentContent {
                            text: comment.text,
                            author: comment.author,
                            initials: comment.initials,
                            date: comment.date,
                        },
                        resolved: comment.resolved.unwrap_or(false),
                        deleted: false,
                        file_id: Some(comment.id),
                    },
                )
            })
            .collect()
    }
}

impl DocxModel {
    /// The model of `word`'s body, notes and text boxes, with ids owned by
    /// [`ReplicaId::ORIGIN`]. Note separators are not modelled.
    pub fn from_word_document(word: &WordDocument) -> Result<Self, LoadError> {
        let document = word.document.as_ref().ok_or(LoadError::NoBody)?;
        let body = document.body.as_ref().ok_or(LoadError::NoBody)?;
        let mut loader = Loader {
            next: 1,
            tables: BTreeMap::new(),
            footnotes: BTreeMap::new(),
            endnotes: BTreeMap::new(),
            text_boxes: BTreeMap::new(),
            note_ids: BTreeMap::new(),
        };
        let notes = |footnote: bool| {
            let notes = if footnote {
                &document.footnotes
            } else {
                &document.endnotes
            };
            notes
                .iter()
                .filter(|(_, note)| note.note_type.is_none())
                .map(move |(id, note)| (footnote, id, &note.body))
        };
        let notes: Vec<_> = notes(true).chain(notes(false)).collect();
        for (footnote, file_id, _) in &notes {
            loader.note(*footnote, file_id);
        }
        let body = loader.story(&body.elements, true, true);
        for (footnote, file_id, blocks) in notes {
            let story = loader.story(blocks, false, false);
            let id = loader.note(footnote, file_id);
            let notes = if footnote {
                &mut loader.footnotes
            } else {
                &mut loader.endnotes
            };
            notes.get_mut(&id).expect("listed above").story = story;
        }
        let comments = loader.comments(word);
        Ok(DocxModel {
            body,
            tables: loader.tables,
            footnotes: loader.footnotes,
            endnotes: loader.endnotes,
            text_boxes: loader.text_boxes,
            comments,
            max_lamport: loader.next - 1,
        })
    }
}
