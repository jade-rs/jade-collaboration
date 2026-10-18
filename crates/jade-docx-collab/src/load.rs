//! Building the model from a jade `WordDocument`.

use std::collections::BTreeMap;

use jade_collab_core::grid::{Grid, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::rga::Rga;
use jade_docx_document_core::document::{BodyElement, Table as JadeTable};
use jade_docx_document_core::headless_api::execute_headless_queries_on_document;
use jade_docx_document_core::word_document::WordDocument;
use jade_docx_headless_api::{HeadlessQuery, HeadlessQueryRequest, HeadlessQueryResult};

use crate::model::{Anchor, Atom, Block, Comment, CommentContent, DocxModel, Placed, Story, Table};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("the document has no parsed body")]
    NoBody,
}

struct Loader {
    next: u64,
    tables: BTreeMap<Id, Table>,
}

impl Loader {
    fn id(&mut self) -> Id {
        let id = Id::new(self.next, ReplicaId::ORIGIN);
        self.next += 1;
        id
    }

    /// Mirrors jade's body surface enumeration: paragraphs in order, a
    /// table's cells row by row, recursing into cell blocks.
    fn story(&mut self, blocks: &[BodyElement], top_level: bool) -> Story {
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
                    let chars: Vec<Atom> = paragraph
                        .runs
                        .iter()
                        .flat_map(|run| run.text.chars())
                        .map(Atom::Char)
                        .collect();
                    if !chars.is_empty() {
                        let count = chars.len() as u64;
                        let first = Id::new(self.next, ReplicaId::ORIGIN);
                        self.next += count;
                        flow.insert_run(first, previous, chars).expect("fresh ids");
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
                    let id = self.table(table, top_level);
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

    fn table(&mut self, table: &JadeTable, top_level: bool) -> Id {
        let id = self.id();
        match regular_layout(table).filter(|_| top_level) {
            Some((width, merges)) => {
                let rows: Vec<Id> = table.rows.iter().map(|_| self.id()).collect();
                let columns: Vec<Id> = (0..width).map(|_| self.id()).collect();
                let mut model = Table::new(&[], &[], false, top_level);
                for (r, row) in table.rows.iter().enumerate() {
                    let mut c = 0;
                    for cell in &row.cells {
                        let story = self.story(&cell.blocks, false);
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
                        let story = self.story(&cell.blocks, false);
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
    /// The model of `word`'s body, with ids owned by [`ReplicaId::ORIGIN`].
    pub fn from_word_document(word: &WordDocument) -> Result<Self, LoadError> {
        let body = word
            .document
            .as_ref()
            .and_then(|doc| doc.body.as_ref())
            .ok_or(LoadError::NoBody)?;
        let mut loader = Loader {
            next: 1,
            tables: BTreeMap::new(),
        };
        let body = loader.story(&body.elements, true);
        let comments = loader.comments(word);
        Ok(DocxModel {
            body,
            tables: loader.tables,
            comments,
            max_lamport: loader.next - 1,
        })
    }
}
