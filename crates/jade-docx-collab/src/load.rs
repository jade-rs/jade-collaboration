//! Building the model from a jade `WordDocument`.

use std::collections::BTreeMap;

use jade_collab_core::grid::Grid;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::rga::Rga;
use jade_docx_document_core::document::{BodyElement, Table as JadeTable};
use jade_docx_document_core::word_document::WordDocument;

use crate::model::{Anchor, Atom, Block, DocxModel, Placed, Story, Table};

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
        let width = table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        let rows: Vec<Id> = table.rows.iter().map(|_| self.id()).collect();
        let columns: Vec<Id> = (0..width.max(1)).map(|_| self.id()).collect();
        let simple = top_level
            && !table.rows.is_empty()
            && table.columns.len() == width
            && table.rows.iter().all(|row| {
                row.cells.len() == width
                    && row.grid_before.unwrap_or(0) == 0
                    && row.grid_after.unwrap_or(0) == 0
                    && row.cells.iter().all(|cell| {
                        cell.span.unwrap_or(1) == 1
                            && cell.vertical_merge.is_none()
                            && !cell.blocks.is_empty()
                            && cell
                                .blocks
                                .iter()
                                .all(|b| matches!(b, BodyElement::Paragraph(_)))
                    })
            });
        let mut cells = BTreeMap::new();
        for (row, jade_row) in rows.iter().zip(&table.rows) {
            for (column, cell) in columns.iter().zip(&jade_row.cells) {
                let story = self.story(&cell.blocks, false);
                cells.insert((*row, *column), story);
            }
        }
        self.tables.insert(
            id,
            Table {
                grid: Grid::new(rows, columns),
                cells,
                locked: !simple,
                deleted: false,
                top_level,
            },
        );
        id
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
        Ok(DocxModel {
            body,
            tables: loader.tables,
            max_lamport: loader.next - 1,
        })
    }
}
