//! The collaborative DOCX model.
//!
//! A *story* — the body or one table cell — is an RGA of characters and
//! paragraph marks; a paragraph is the run of characters ending at a visible
//! mark, and a story's last paragraph has no mark (Word's final paragraph
//! mark cannot be deleted). Tables are placed *after* a mark (or at the start
//! or end of their story), so a concurrent split can never put a table inside
//! a paragraph. Walking the model in order gives the same paragraphs, in the
//! same order, as jade's body paragraph surfaces — which is how a model
//! position becomes a jade paragraph index.

use std::collections::{BTreeMap, BTreeSet};

use jade_collab_core::grid::{Grid, GridOp};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::rga::Rga;
use jade_collab_sync::{ApplyContext, Document, Lane, Rejection};
use serde::{Deserialize, Serialize};

/// Which story an operation edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoryId {
    Body,
    Cell { table: Id, row: Id, column: Id },
}

/// Where a block sits in its story.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "mark", rename_all = "snake_case")]
pub enum Anchor {
    /// Before the story's first paragraph.
    Start,
    /// Right after the paragraph that ends with this mark.
    After(Id),
    /// After the story's last paragraph (only found in loaded files).
    End,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Atom {
    Char(char),
    Mark,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Story {
    pub(crate) flow: Rga<Atom>,
    pub(crate) blocks: Vec<Placed>,
    /// False for a story jade shows no paragraph for (a cell holding only a
    /// table); such a story is read-only.
    pub(crate) has_paragraph: bool,
}

impl Story {
    pub(crate) fn empty_paragraph() -> Self {
        Self {
            flow: Rga::new(),
            blocks: Vec::new(),
            has_paragraph: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Placed {
    pub(crate) anchor: Anchor,
    pub(crate) block: Block,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Block {
    Table(Id),
    /// Content the model does not edit (equations): it only blocks joins.
    Opaque,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Table {
    pub(crate) grid: Grid,
    #[serde(with = "pairs")]
    pub(crate) cells: BTreeMap<(Id, Id), Story>,
    /// Merged, ragged or nested tables: cell text is editable, structure is not.
    pub(crate) locked: bool,
    pub(crate) deleted: bool,
    pub(crate) top_level: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocxModel {
    pub(crate) body: Story,
    #[serde(with = "pairs")]
    pub(crate) tables: BTreeMap<Id, Table>,
    pub(crate) max_lamport: u64,
}

/// One paragraph as the model walks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    pub story: StoryId,
    /// The mark that ends it; `None` for its story's last paragraph.
    pub mark: Option<Id>,
    pub text: String,
    /// The visible characters, in order (one per `char` of `text`).
    pub chars: Vec<Id>,
}

/// A top-level table as the model walks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableView {
    pub id: Id,
    pub rows: Vec<Id>,
    pub columns: Vec<Id>,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocxOp {
    /// Convergent. Inserts `text` as a run after `parent` (`None`: story
    /// start); its characters get ids `first`, `first.offset(1)`, ….
    InsertText {
        story: StoryId,
        parent: Option<Id>,
        first: Id,
        text: String,
    },
    /// Convergent. Deletes characters (never marks); idempotent.
    DeleteText { story: StoryId, ids: Vec<Id> },
    /// Convergent. Inserts a paragraph mark after `parent`.
    SplitParagraph {
        story: StoryId,
        parent: Option<Id>,
        id: Id,
    },
    /// Sequenced. Deletes `mark`, joining its paragraph with the next one.
    /// Refused while a table follows the paragraph.
    JoinParagraph { story: StoryId, mark: Id },
    /// Sequenced. A new empty table in the body.
    InsertTable {
        anchor: Anchor,
        id: Id,
        rows: Vec<Id>,
        columns: Vec<Id>,
    },
    /// Sequenced. Row and column structure of a top-level table.
    Table { table: Id, op: GridOp },
    /// Sequenced.
    DeleteTable { table: Id },
}

/// An operation's consequence in jade terms: paragraph ordinals in model walk
/// order, UTF-8 byte offsets in model text, and tables by their pre-order
/// index in the body's table structure. The materializer maps these to jade
/// paragraph surfaces and table order. Emitted in the order to apply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocxEffect {
    InsertText {
        paragraph: usize,
        offset: usize,
        text: String,
    },
    DeleteRange {
        paragraph: usize,
        start: usize,
        end: usize,
    },
    SplitParagraph {
        paragraph: usize,
        offset: usize,
    },
    /// Join `paragraph` (whose text is `offset` bytes long) with the next.
    JoinParagraph {
        paragraph: usize,
        offset: usize,
    },
    /// A table after `paragraph`, or above it when `above`.
    InsertTable {
        paragraph: usize,
        above: bool,
        rows: usize,
        columns: usize,
    },
    InsertTableRow {
        table: usize,
        row: usize,
        template: Option<usize>,
    },
    DeleteTableRow {
        table: usize,
        row: usize,
    },
    InsertTableColumn {
        table: usize,
        column: usize,
        template: Option<usize>,
    },
    DeleteTableColumn {
        table: usize,
        column: usize,
    },
    DeleteTable {
        table: usize,
    },
}

#[derive(Default)]
struct Walk {
    paragraphs: Vec<Paragraph>,
    tables: Vec<TableView>,
    /// Every visible table, nested ones included, in pre-order.
    all_tables: Vec<Id>,
}

fn reject(message: impl Into<String>) -> Rejection {
    Rejection::new(message)
}

impl DocxModel {
    /// A body-only document, one paragraph per entry; ids belong to
    /// [`ReplicaId::ORIGIN`].
    pub fn from_paragraphs(paragraphs: &[&str]) -> Self {
        let mut next = 1u64;
        let mut flow = Rga::new();
        let mut previous = None;
        for (i, text) in paragraphs.iter().enumerate() {
            let chars: Vec<Atom> = text.chars().map(Atom::Char).collect();
            if !chars.is_empty() {
                let first = Id::new(next, ReplicaId::ORIGIN);
                let count = chars.len() as u64;
                flow.insert_run(first, previous, chars).expect("fresh ids");
                next += count;
                previous = Some(first.offset(count - 1));
            }
            if i + 1 < paragraphs.len() {
                let mark = Id::new(next, ReplicaId::ORIGIN);
                flow.insert(mark, previous, Atom::Mark).expect("fresh id");
                next += 1;
                previous = Some(mark);
            }
        }
        Self {
            body: Story {
                flow,
                blocks: Vec::new(),
                has_paragraph: true,
            },
            tables: BTreeMap::new(),
            max_lamport: next - 1,
        }
    }

    fn walk(&self) -> Walk {
        let mut out = Walk::default();
        self.walk_story(StoryId::Body, &self.body, &mut out);
        out
    }

    fn walk_story(&self, id: StoryId, story: &Story, out: &mut Walk) {
        self.walk_blocks(id, story, Anchor::Start, out);
        let mut current = Paragraph {
            story: id,
            mark: None,
            text: String::new(),
            chars: Vec::new(),
        };
        for node in story.flow.nodes().iter().filter(|n| !n.deleted) {
            match node.value {
                Atom::Char(c) => {
                    current.text.push(c);
                    current.chars.push(node.id);
                }
                Atom::Mark => {
                    current.mark = Some(node.id);
                    let next = Paragraph {
                        story: id,
                        mark: None,
                        text: String::new(),
                        chars: Vec::new(),
                    };
                    out.paragraphs.push(std::mem::replace(&mut current, next));
                    self.walk_blocks(id, story, Anchor::After(node.id), out);
                }
            }
        }
        if story.has_paragraph {
            out.paragraphs.push(current);
        }
        self.walk_blocks(id, story, Anchor::End, out);
    }

    fn walk_blocks(&self, id: StoryId, story: &Story, anchor: Anchor, out: &mut Walk) {
        for placed in story.blocks.iter().filter(|p| p.anchor == anchor) {
            let Block::Table(table_id) = placed.block else {
                continue;
            };
            let table = &self.tables[&table_id];
            if table.deleted {
                continue;
            }
            let rows = table.grid.rows();
            let columns = table.grid.columns();
            out.all_tables.push(table_id);
            if id == StoryId::Body {
                out.tables.push(TableView {
                    id: table_id,
                    rows: rows.clone(),
                    columns: columns.clone(),
                    locked: table.locked,
                });
            }
            for &row in &rows {
                for &column in &columns {
                    if let Some(cell) = table.cells.get(&(row, column)) {
                        let cell_id = StoryId::Cell {
                            table: table_id,
                            row,
                            column,
                        };
                        self.walk_story(cell_id, cell, out);
                    }
                }
            }
        }
    }

    /// Every visible paragraph, in jade's body paragraph-surface order.
    pub fn paragraphs(&self) -> Vec<Paragraph> {
        self.walk().paragraphs
    }

    pub fn paragraph_texts(&self) -> Vec<String> {
        self.paragraphs().into_iter().map(|p| p.text).collect()
    }

    /// Top-level tables in order; the index is jade's `table_order_index`.
    pub fn tables(&self) -> Vec<TableView> {
        self.walk().tables
    }

    pub(crate) fn story(&self, id: StoryId) -> Option<&Story> {
        match id {
            StoryId::Body => Some(&self.body),
            StoryId::Cell { table, row, column } => {
                self.tables.get(&table)?.cells.get(&(row, column))
            }
        }
    }

    fn story_mut(&mut self, id: StoryId) -> Option<&mut Story> {
        match id {
            StoryId::Body => Some(&mut self.body),
            StoryId::Cell { table, row, column } => {
                self.tables.get_mut(&table)?.cells.get_mut(&(row, column))
            }
        }
    }

    fn editable_story(&self, id: StoryId) -> Result<&Story, Rejection> {
        let story = self
            .story(id)
            .ok_or_else(|| reject(format!("story {id:?} does not exist")))?;
        if !story.has_paragraph {
            return Err(reject(format!("story {id:?} has no paragraph to edit")));
        }
        Ok(story)
    }

    /// (ordinal, byte offset, byte length) of each requested visible atom; a
    /// mark reports the offset of its paragraph's end.
    fn locate(&self, ids: &BTreeSet<Id>) -> BTreeMap<Id, (usize, usize, usize)> {
        let mut found = BTreeMap::new();
        for (ordinal, paragraph) in self.paragraphs().into_iter().enumerate() {
            let mut offset = 0;
            for (c, id) in paragraph.text.chars().zip(&paragraph.chars) {
                if ids.contains(id) {
                    found.insert(*id, (ordinal, offset, c.len_utf8()));
                }
                offset += c.len_utf8();
            }
            if let Some(mark) = paragraph.mark.filter(|m| ids.contains(m)) {
                found.insert(mark, (ordinal, offset, 0));
            }
        }
        found
    }

    fn locate_one(&self, id: Id) -> Option<(usize, usize)> {
        self.locate(&BTreeSet::from([id]))
            .get(&id)
            .map(|&(p, offset, _)| (p, offset))
    }

    /// The table's index among all visible tables, nested ones included, in
    /// document pre-order — the order of the body's table structure. The
    /// materializer maps it to jade's `table_order_index` (which also counts
    /// tables inside text boxes).
    /// The ordinal of the visible paragraph `mark` ends.
    pub(crate) fn paragraph_ending_with(&self, story: StoryId, mark: Id) -> Option<usize> {
        self.paragraphs()
            .iter()
            .position(|p| p.story == story && p.mark == Some(mark))
    }

    fn table_order(&self, table: Id) -> Option<usize> {
        self.walk().all_tables.iter().position(|t| *t == table)
    }

    fn observe(&mut self, ids: impl IntoIterator<Item = Id>) {
        for id in ids {
            self.max_lamport = self.max_lamport.max(id.lamport);
        }
    }

    fn insert_text(
        &mut self,
        story: StoryId,
        parent: Option<Id>,
        first: Id,
        text: &str,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if text.is_empty() {
            return Err(reject("nothing to insert"));
        }
        self.editable_story(story)?;
        let count = text.chars().count() as u64;
        self.story_mut(story).expect("checked").flow.insert_run(
            first,
            parent,
            text.chars().map(Atom::Char),
        )?;
        self.observe([first.offset(count - 1)]);
        Ok(self
            .locate_one(first)
            .map(|(paragraph, offset)| DocxEffect::InsertText {
                paragraph,
                offset,
                text: text.to_string(),
            })
            .into_iter()
            .collect())
    }

    fn delete_text(&mut self, story: StoryId, ids: &[Id]) -> Result<Vec<DocxEffect>, Rejection> {
        let flow = &self.editable_story(story)?.flow;
        for id in ids {
            match flow.get(*id) {
                None => return Err(reject(format!("character {id} does not exist"))),
                Some(node) if node.value == Atom::Mark => {
                    return Err(reject("paragraph marks are joined, not deleted"))
                }
                Some(_) => {}
            }
        }
        let positions = self.locate(&ids.iter().copied().collect());
        let flow = &mut self.story_mut(story).expect("checked").flow;
        for id in ids {
            flow.delete(*id)?;
        }
        // Merge adjacent characters into ranges; apply from the end so earlier
        // offsets stay valid.
        let mut spans: Vec<(usize, usize, usize)> = positions
            .values()
            .map(|&(p, offset, len)| (p, offset, offset + len))
            .collect();
        spans.sort();
        let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
        for (p, start, end) in spans {
            match ranges.last_mut() {
                Some(last) if last.0 == p && last.2 == start => last.2 = end,
                _ => ranges.push((p, start, end)),
            }
        }
        Ok(ranges
            .into_iter()
            .rev()
            .map(|(paragraph, start, end)| DocxEffect::DeleteRange {
                paragraph,
                start,
                end,
            })
            .collect())
    }

    fn split(
        &mut self,
        story: StoryId,
        parent: Option<Id>,
        id: Id,
    ) -> Result<Vec<DocxEffect>, Rejection> {
        self.editable_story(story)?;
        self.story_mut(story)
            .expect("checked")
            .flow
            .insert(id, parent, Atom::Mark)?;
        self.observe([id]);
        Ok(self
            .locate_one(id)
            .map(|(paragraph, offset)| DocxEffect::SplitParagraph { paragraph, offset })
            .into_iter()
            .collect())
    }

    fn join(&mut self, story_id: StoryId, mark: Id) -> Result<Vec<DocxEffect>, Rejection> {
        let story = self.editable_story(story_id)?;
        match story.flow.get(mark) {
            Some(node) if node.value == Atom::Mark && !node.deleted => {}
            Some(node) if node.value == Atom::Mark => {
                return Err(reject("the paragraphs are already joined"))
            }
            _ => return Err(reject(format!("{mark} is not a paragraph mark"))),
        }
        let blocked = story.blocks.iter().any(|placed| {
            placed.anchor == Anchor::After(mark)
                && match placed.block {
                    Block::Table(table) => !self.tables[&table].deleted,
                    Block::Opaque => true,
                }
        });
        if blocked {
            return Err(reject(
                "a table separates the paragraphs; they cannot be joined",
            ));
        }
        let effect = self
            .locate_one(mark)
            .map(|(paragraph, offset)| DocxEffect::JoinParagraph { paragraph, offset });
        self.story_mut(story_id)
            .expect("checked")
            .flow
            .delete(mark)?;
        Ok(effect.into_iter().collect())
    }

    fn insert_table(
        &mut self,
        anchor: Anchor,
        id: Id,
        rows: &[Id],
        columns: &[Id],
    ) -> Result<Vec<DocxEffect>, Rejection> {
        if rows.is_empty() || columns.is_empty() {
            return Err(reject("a table needs at least one row and one column"));
        }
        if self.tables.contains_key(&id) {
            return Err(reject(format!("table {id} already exists")));
        }
        let mut all: BTreeSet<Id> = BTreeSet::from([id]);
        if !rows.iter().chain(columns).all(|i| all.insert(*i)) {
            return Err(reject("table, row and column ids must be distinct"));
        }
        let paragraphs = self.paragraphs();
        let effect = match anchor {
            Anchor::Start => {
                let first = paragraphs
                    .iter()
                    .position(|p| p.story == StoryId::Body)
                    .ok_or_else(|| reject("the body has no paragraph"))?;
                DocxEffect::InsertTable {
                    paragraph: first,
                    above: true,
                    rows: rows.len(),
                    columns: columns.len(),
                }
            }
            Anchor::After(mark) => {
                let paragraph = paragraphs
                    .iter()
                    .position(|p| p.story == StoryId::Body && p.mark == Some(mark))
                    .ok_or_else(|| {
                        reject(format!(
                            "{mark} is not a visible paragraph mark in the body"
                        ))
                    })?;
                DocxEffect::InsertTable {
                    paragraph,
                    above: false,
                    rows: rows.len(),
                    columns: columns.len(),
                }
            }
            Anchor::End => {
                return Err(reject("split the last paragraph to place a table after it"))
            }
        };
        let cells = rows
            .iter()
            .flat_map(|r| {
                columns
                    .iter()
                    .map(move |c| ((*r, *c), Story::empty_paragraph()))
            })
            .collect();
        self.tables.insert(
            id,
            Table {
                grid: Grid::new(rows.iter().copied(), columns.iter().copied()),
                cells,
                locked: false,
                deleted: false,
                top_level: true,
            },
        );
        let placed = Placed {
            anchor,
            block: Block::Table(id),
        };
        // jade puts a table straight after its paragraph (before older ones
        // there), or straight above the first paragraph (after older ones).
        match anchor {
            Anchor::After(_) => {
                let at = self
                    .body
                    .blocks
                    .iter()
                    .position(|p| p.anchor == anchor)
                    .unwrap_or(self.body.blocks.len());
                self.body.blocks.insert(at, placed);
            }
            _ => self.body.blocks.push(placed),
        }
        self.observe(all);
        Ok(vec![effect])
    }

    fn structural_table(&self, table: Id) -> Result<(&Table, usize), Rejection> {
        let entry = self
            .tables
            .get(&table)
            .ok_or_else(|| reject(format!("table {table} does not exist")))?;
        if entry.deleted {
            return Err(reject(format!("table {table} was deleted")));
        }
        if !entry.top_level {
            return Err(reject("only top-level tables can change structure"));
        }
        let order = self.table_order(table).expect("a visible table is walked");
        Ok((entry, order))
    }

    fn table_op(&mut self, table_id: Id, op: &GridOp) -> Result<Vec<DocxEffect>, Rejection> {
        let (table, order) = self.structural_table(table_id)?;
        if table.locked {
            return Err(reject(
                "this table has merged, ragged or nested cells; its structure is read-only",
            ));
        }
        let before = (
            match op {
                GridOp::DeleteRow { id } => table.grid.row_index(*id),
                GridOp::DeleteColumn { id } => table.grid.column_index(*id),
                _ => None,
            },
            table.grid.all_rows(),
            table.grid.all_columns(),
        );
        let table = self.tables.get_mut(&table_id).expect("checked");
        let effect = match op {
            GridOp::Merge(_) | GridOp::Unmerge { .. } => {
                return Err(reject("merged cells are not supported for docx yet"))
            }
            GridOp::InsertRow { id, .. } => {
                table.grid.apply(op)?;
                for column in &before.2 {
                    table.cells.insert((*id, *column), Story::empty_paragraph());
                }
                let row = table.grid.row_index(*id).expect("just inserted");
                DocxEffect::InsertTableRow {
                    table: order,
                    row,
                    template: row.checked_sub(1),
                }
            }
            GridOp::InsertColumn { id, .. } => {
                table.grid.apply(op)?;
                for row in &before.1 {
                    table.cells.insert((*row, *id), Story::empty_paragraph());
                }
                let column = table.grid.column_index(*id).expect("just inserted");
                DocxEffect::InsertTableColumn {
                    table: order,
                    column,
                    template: column.checked_sub(1),
                }
            }
            GridOp::DeleteRow { .. } => {
                table.grid.apply(op)?;
                DocxEffect::DeleteTableRow {
                    table: order,
                    row: before.0.expect("validated by the grid"),
                }
            }
            GridOp::DeleteColumn { .. } => {
                table.grid.apply(op)?;
                DocxEffect::DeleteTableColumn {
                    table: order,
                    column: before.0.expect("validated by the grid"),
                }
            }
        };
        if let GridOp::InsertRow { id, .. } | GridOp::InsertColumn { id, .. } = op {
            self.observe([*id]);
        }
        Ok(vec![effect])
    }

    fn delete_table(&mut self, table: Id) -> Result<Vec<DocxEffect>, Rejection> {
        let (_, order) = self.structural_table(table)?;
        self.tables.get_mut(&table).expect("checked").deleted = true;
        Ok(vec![DocxEffect::DeleteTable { table: order }])
    }
}

impl Document for DocxModel {
    type Op = DocxOp;
    type Effect = Vec<DocxEffect>;

    fn lane(op: &DocxOp) -> Lane {
        match op {
            DocxOp::InsertText { .. }
            | DocxOp::DeleteText { .. }
            | DocxOp::SplitParagraph { .. } => Lane::Convergent,
            DocxOp::JoinParagraph { .. }
            | DocxOp::InsertTable { .. }
            | DocxOp::Table { .. }
            | DocxOp::DeleteTable { .. } => Lane::Sequenced,
        }
    }

    fn apply(&mut self, op: &DocxOp, _ctx: &ApplyContext) -> Result<Vec<DocxEffect>, Rejection> {
        match op {
            DocxOp::InsertText {
                story,
                parent,
                first,
                text,
            } => self.insert_text(*story, *parent, *first, text),
            DocxOp::DeleteText { story, ids } => self.delete_text(*story, ids),
            DocxOp::SplitParagraph { story, parent, id } => self.split(*story, *parent, *id),
            DocxOp::JoinParagraph { story, mark } => self.join(*story, *mark),
            DocxOp::InsertTable {
                anchor,
                id,
                rows,
                columns,
            } => self.insert_table(*anchor, *id, rows, columns),
            DocxOp::Table { table, op } => self.table_op(*table, op),
            DocxOp::DeleteTable { table } => self.delete_table(*table),
        }
    }

    fn max_lamport(&self) -> u64 {
        self.max_lamport
    }
}

/// Serializes a map with non-string keys as a list of pairs.
mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        map: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K, V, D>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}
