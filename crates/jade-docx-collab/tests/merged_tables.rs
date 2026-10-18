//! Merged table cells in docx: merging shows the covered cells' content in
//! the top-left cell (as jade concatenates it), unmerging gives the freed
//! cells new empty stories, and text edits into any cell commute with both.
//! Rows and columns go through merged cells as in Word and jade: a line
//! inserted strictly inside a merge joins it, a deleted edge moves inward.

mod common;

use common::{assert_tables_agree, jade_core, model_tables, open, TableLayout};
use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_docx_collab::{Anchor, DocxModel, DocxOp, JadeDocx, StoryId};
use proptest::prelude::*;

struct Session {
    model: DocxModel,
    jade: JadeDocx,
    seq: u64,
    next: u64,
}

impl Session {
    fn open(name: &str) -> Self {
        let (model, jade) = open(name);
        Self {
            model,
            jade,
            seq: 0,
            next: 5_000_000,
        }
    }

    fn id(&mut self) -> Id {
        self.next += 1;
        Id::new(self.next, ReplicaId(1))
    }

    fn reserve(&mut self, n: u64) -> Id {
        let first = Id::new(self.next + 1, ReplicaId(1));
        self.next += n;
        first
    }

    fn step(&mut self, op: DocxOp) {
        self.seq += 1;
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: self.seq - 1,
            seq: Some(self.seq),
        };
        if DocxModel::lane(&op) == Lane::Sequenced {
            self.jade
                .admit(&self.model, &op)
                .unwrap_or_else(|e| panic!("jade refused {op:?}: {e}"));
        }
        let effect = self
            .model
            .apply(&op, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        self.jade
            .materialize(&self.model, effect, &ctx)
            .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        assert_eq!(
            self.model.paragraph_texts(),
            self.jade.body_paragraph_texts(),
            "after {op:?}"
        );
        assert_tables_agree(&self.model, &self.jade, &format!("after {op:?}"));
    }

    fn grid(&mut self, table: Id, op: GridOp) {
        self.step(DocxOp::Table { table, op });
    }

    /// The first table as the model shows it (jade agrees, `step` checks).
    fn layout(&self) -> TableLayout {
        model_tables(&self.model).remove(0).expect("unlocked")
    }

    /// The file jade writes reads back as the same tables.
    fn assert_round_trips(&self) {
        let reloaded = JadeDocx::load(&self.jade.export().unwrap()).unwrap();
        let model = DocxModel::from_word_document(reloaded.word()).unwrap();
        assert_eq!(
            reloaded.body_paragraph_texts(),
            self.model.paragraph_texts()
        );
        assert_tables_agree(&model, &reloaded, "reloaded");
        assert_eq!(model_tables(&model), model_tables(&self.model));
    }

    /// A 3 × 3 table after the first paragraph, with "r{row}c{col}" typed in
    /// every cell; returns its id, rows and columns.
    fn table(&mut self) -> (Id, Vec<Id>, Vec<Id>) {
        let end = self.model.paragraphs()[0].chars.last().copied();
        let mark = self.id();
        self.step(DocxOp::SplitParagraph {
            story: StoryId::Body,
            parent: end,
            id: mark,
        });
        let first = self.reserve(7);
        let rows: Vec<Id> = (1..=3).map(|k| first.offset(k)).collect();
        let columns: Vec<Id> = (4..=6).map(|k| first.offset(k)).collect();
        self.step(DocxOp::InsertTable {
            anchor: Anchor::After(mark),
            id: first,
            rows: rows.clone(),
            columns: columns.clone(),
        });
        for (r, row) in rows.iter().enumerate() {
            for (c, column) in columns.iter().enumerate() {
                let text = format!("r{r}c{c}");
                let first = self.reserve(text.chars().count() as u64);
                self.step(DocxOp::InsertText {
                    story: StoryId::Cell {
                        table: first_table(&self.model),
                        row: *row,
                        column: *column,
                    },
                    parent: None,
                    first,
                    text,
                });
            }
        }
        (first, rows, columns)
    }
}

fn first_table(model: &DocxModel) -> Id {
    model.tables()[0].id
}

fn region(
    rows: &[Id],
    columns: &[Id],
    top: usize,
    left: usize,
    bottom: usize,
    right: usize,
) -> Merge {
    Merge {
        top: rows[top],
        left: columns[left],
        bottom: rows[bottom],
        right: columns[right],
    }
}

#[test]
fn merging_shows_the_covered_cells_in_the_top_left_one() {
    let mut s = Session::open("jade-core/hello.docx");
    let (table, rows, columns) = s.table();
    s.step(DocxOp::MergeCells {
        table,
        merge: region(&rows, &columns, 0, 0, 1, 1),
        first: Id::new(7_000_000, ReplicaId(3)),
    });
    let texts = s.model.paragraph_texts();
    // Row-major: r0c0, r0c1, r1c0, r1c1 all in the merged cell, then r0c2,
    // then row 1's follower (one empty paragraph, as Word keeps it) and its
    // remaining cell r1c2, then row 2.
    let at = texts.iter().position(|t| t == "r0c0").unwrap();
    assert_eq!(
        texts[at..at + 10],
        ["r0c0", "r0c1", "r1c0", "r1c1", "r0c2", "", "r1c2", "r2c0", "r2c1", "r2c2"]
    );
    let view = &s.model.tables()[0];
    assert!(!view.locked);
    assert_eq!(view.merges, vec![(0, 0, 1, 1)]);
}

#[test]
fn text_typed_into_a_covered_cell_lands_in_the_merged_cell() {
    let mut s = Session::open("jade-core/hello.docx");
    let (table, rows, columns) = s.table();
    s.step(DocxOp::MergeCells {
        table,
        merge: region(&rows, &columns, 0, 0, 0, 1),
        first: Id::new(7_000_000, ReplicaId(3)),
    });
    // A replica that had not seen the merge types into r0c1.
    let story = StoryId::Cell {
        table,
        row: rows[0],
        column: columns[1],
    };
    let end = s
        .model
        .paragraphs()
        .iter()
        .find(|p| p.story == story)
        .unwrap()
        .chars
        .last()
        .copied();
    let first = s.reserve(1);
    s.step(DocxOp::InsertText {
        story,
        parent: end,
        first,
        text: "!".into(),
    });
    assert!(s.model.paragraph_texts().contains(&"r0c1!".to_string()));
}

#[test]
fn unmerging_keeps_the_content_and_frees_new_empty_cells() {
    let mut s = Session::open("jade-core/hello.docx");
    let (table, rows, columns) = s.table();
    s.step(DocxOp::MergeCells {
        table,
        merge: region(&rows, &columns, 1, 1, 2, 2),
        first: Id::new(7_000_000, ReplicaId(3)),
    });
    let first = s.reserve(3);
    s.step(DocxOp::UnmergeCells {
        table,
        top: rows[1],
        left: columns[1],
        first,
    });
    assert!(s.model.tables()[0].merges.is_empty());
    // The freed cells are new stories and take text.
    let fresh = s
        .model
        .paragraphs()
        .into_iter()
        .find(|p| matches!(p.story, StoryId::Split { .. }))
        .expect("a freed cell");
    let typed = s.reserve(3);
    s.step(DocxOp::InsertText {
        story: fresh.story,
        parent: None,
        first: typed,
        text: "new".into(),
    });
    let reloaded = JadeDocx::load(&s.jade.export().unwrap()).unwrap();
    assert_eq!(reloaded.body_paragraph_texts(), s.model.paragraph_texts());
}

fn texts(layout: &TableLayout, row: usize, column: usize) -> Vec<&str> {
    layout
        .cells
        .get(&(row, column))
        .map(|t| t.iter().map(String::as_str).collect())
        .unwrap_or_default()
}

/// A 3 × 3 table with its top-left 2 × 2 merged.
fn merged_session() -> (Session, Id, Vec<Id>, Vec<Id>) {
    let mut s = Session::open("jade-core/hello.docx");
    let (table, rows, columns) = s.table();
    s.step(DocxOp::MergeCells {
        table,
        merge: region(&rows, &columns, 0, 0, 1, 1),
        first: Id::new(7_000_000, ReplicaId(3)),
    });
    (s, table, rows, columns)
}

#[test]
fn a_row_inserted_inside_a_merge_joins_it() {
    let (mut s, table, rows, _) = merged_session();
    let id = s.id();
    s.grid(
        table,
        GridOp::InsertRow {
            id,
            after: Some(rows[0]),
        },
    );
    let layout = s.layout();
    assert_eq!(layout.merges, vec![(0, 0, 2, 1)]);
    // The new row's merged part is one empty follower, as Word makes it; the
    // cell outside the merge is a new empty cell.
    assert_eq!(texts(&layout, 1, 0), [""]);
    assert!(!layout.cells.contains_key(&(1, 1)));
    assert_eq!(texts(&layout, 1, 2), [""]);
    assert_eq!(texts(&layout, 0, 0), ["r0c0", "r0c1", "r1c0", "r1c1"]);
    s.assert_round_trips();
}

#[test]
fn a_row_inserted_at_a_merge_edge_stays_out_of_it() {
    let (mut s, table, rows, _) = merged_session();
    let below = s.id();
    s.grid(
        table,
        GridOp::InsertRow {
            id: below,
            after: Some(rows[1]),
        },
    );
    let above = s.id();
    s.grid(
        table,
        GridOp::InsertRow {
            id: above,
            after: None,
        },
    );
    let layout = s.layout();
    assert_eq!(layout.merges, vec![(1, 0, 2, 1)]);
    assert_eq!(texts(&layout, 3, 0), [""]);
    assert_eq!(texts(&layout, 3, 1), [""]);
    s.assert_round_trips();
}

#[test]
fn a_column_inserted_inside_a_merge_joins_it() {
    let (mut s, table, _, columns) = merged_session();
    let id = s.id();
    s.grid(
        table,
        GridOp::InsertColumn {
            id,
            after: Some(columns[0]),
        },
    );
    let at_edge = s.id();
    s.grid(
        table,
        GridOp::InsertColumn {
            id: at_edge,
            after: Some(columns[1]),
        },
    );
    let layout = s.layout();
    assert_eq!(layout.merges, vec![(0, 0, 1, 2)]);
    assert!(!layout.cells.contains_key(&(0, 1)));
    assert!(!layout.cells.contains_key(&(1, 1)));
    // At the merge's right edge: new cells in every row.
    assert_eq!(texts(&layout, 0, 3), [""]);
    assert_eq!(texts(&layout, 1, 3), [""]);
    assert_eq!(texts(&layout, 0, 4), ["r0c2"]);
    s.assert_round_trips();
}

/// Deleting a merge's top row deletes what the merged cell shows (it lives
/// in that row's `w:tc`, in Word as in jade); the follower below becomes the
/// merged cell.
#[test]
fn deleting_a_merge_top_row_moves_it_down() {
    let mut s = Session::open("jade-core/hello.docx");
    let (table, rows, columns) = s.table();
    s.step(DocxOp::MergeCells {
        table,
        merge: region(&rows, &columns, 0, 1, 2, 2),
        first: Id::new(7_000_000, ReplicaId(3)),
    });
    s.grid(table, GridOp::DeleteRow { id: rows[0] });
    let layout = s.layout();
    assert_eq!(layout.merges, vec![(0, 1, 1, 2)]);
    assert_eq!(texts(&layout, 0, 1), [""]);
    assert_eq!(texts(&layout, 0, 0), ["r1c0"]);
    // Deleting the next one leaves a 1 × 2 merge, then none.
    s.grid(table, GridOp::DeleteRow { id: rows[1] });
    assert_eq!(s.layout().merges, vec![(0, 1, 0, 2)]);
    s.grid(table, GridOp::DeleteColumn { id: columns[1] });
    assert!(s.layout().merges.is_empty());
    s.assert_round_trips();
}

/// Deleting a merge's left column keeps what the merged cell shows: its
/// `w:tc` only spans one column fewer.
#[test]
fn deleting_a_merge_left_column_keeps_its_content() {
    let (mut s, table, rows, columns) = merged_session();
    s.grid(table, GridOp::DeleteColumn { id: columns[0] });
    let layout = s.layout();
    assert_eq!(layout.merges, vec![(0, 0, 1, 0)]);
    assert_eq!(texts(&layout, 0, 0), ["r0c0", "r0c1", "r1c0", "r1c1"]);
    assert_eq!(texts(&layout, 1, 0), [""]);
    // Its last column on that axis... and then its last row: no merge.
    s.grid(table, GridOp::DeleteRow { id: rows[1] });
    let layout = s.layout();
    assert!(layout.merges.is_empty());
    assert_eq!(texts(&layout, 0, 0), ["r0c0", "r0c1", "r1c0", "r1c1"]);
    s.assert_round_trips();
}

#[test]
fn deleting_inner_lines_shrinks_a_merge() {
    let mut s = Session::open("jade-core/hello.docx");
    let (table, rows, columns) = s.table();
    s.step(DocxOp::MergeCells {
        table,
        merge: region(&rows, &columns, 0, 0, 2, 2),
        first: Id::new(7_000_000, ReplicaId(3)),
    });
    s.grid(table, GridOp::DeleteRow { id: rows[1] });
    s.grid(table, GridOp::DeleteColumn { id: columns[1] });
    let layout = s.layout();
    assert_eq!(layout.merges, vec![(0, 0, 1, 1)]);
    assert_eq!(layout.cells.len(), 2, "the merged cell and its follower");
    s.assert_round_trips();
}

/// After rows and columns went through a merge, unmerging frees every cell
/// it covers — the inserted ones too — and they take text.
#[test]
fn a_grown_merge_unmerges_into_fresh_cells() {
    let (mut s, table, rows, columns) = merged_session();
    let row = s.id();
    s.grid(
        table,
        GridOp::InsertRow {
            id: row,
            after: Some(rows[0]),
        },
    );
    let column = s.id();
    s.grid(
        table,
        GridOp::InsertColumn {
            id: column,
            after: Some(columns[0]),
        },
    );
    assert_eq!(s.layout().merges, vec![(0, 0, 2, 2)]);
    let first = s.reserve(9);
    s.step(DocxOp::UnmergeCells {
        table,
        top: rows[0],
        left: columns[0],
        first,
    });
    let layout = s.layout();
    assert!(layout.merges.is_empty());
    assert_eq!(layout.cells.len(), 16);
    let freed = s
        .model
        .paragraphs()
        .into_iter()
        .filter(|p| matches!(p.story, StoryId::Split { .. }))
        .last()
        .expect("a freed cell");
    let typed = s.reserve(2);
    s.step(DocxOp::InsertText {
        story: freed.story,
        parent: None,
        first: typed,
        text: "ok".into(),
    });
    s.assert_round_trips();
}

#[test]
fn tables_with_merges_in_the_file_load_unlocked() {
    let dir = jade_core().join("public/fixtures");
    let mut unlocked_with_merges = 0;
    for name in [
        "table-complex-merged-continuations.docx",
        "merged-table-over-page.docx",
    ] {
        if !dir.join(name).exists() {
            continue;
        }
        let (model, jade) = open(name);
        assert_eq!(
            model.paragraph_texts(),
            jade.body_paragraph_texts(),
            "{name}"
        );
        unlocked_with_merges += model
            .tables()
            .iter()
            .filter(|t| !t.locked && !t.merges.is_empty())
            .count();
    }
    assert!(unlocked_with_merges > 0);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// A merge commutes with text typed into any of its cells: whichever
    /// order a replica applies them in, the model is the same.
    #[test]
    fn merges_commute_with_cell_text(
        cells in prop::collection::vec((0usize..3, 0usize..3), 1..5),
        top in 0usize..2, left in 0usize..2,
    ) {
        let mut s = Session::open("jade-core/hello.docx");
        let (table, rows, columns) = s.table();
        let merge = DocxOp::MergeCells { table, merge: region(&rows, &columns, top, left, top + 1, left + 1), first: Id::new(7_000_000, ReplicaId(3)) };
        let edits: Vec<DocxOp> = cells
            .iter()
            .enumerate()
            .map(|(k, (r, c))| DocxOp::InsertText {
                story: StoryId::Cell { table, row: rows[*r], column: columns[*c] },
                parent: None,
                first: Id::new(9_000_000 + k as u64 * 10, ReplicaId(2)),
                text: format!("e{k}"),
            })
            .collect();
        let ctx = ApplyContext { origin: ReplicaId(2), base_seq: 0, seq: None };
        let mut merged_first = s.model.clone();
        merged_first.apply(&merge, &ctx).unwrap();
        for e in &edits {
            merged_first.apply(e, &ctx).unwrap();
        }
        let mut merged_last = s.model.clone();
        for e in &edits {
            merged_last.apply(e, &ctx).unwrap();
        }
        merged_last.apply(&merge, &ctx).unwrap();
        prop_assert_eq!(merged_first.paragraph_texts(), merged_last.paragraph_texts());
        prop_assert_eq!(merged_first, merged_last);
    }
}
