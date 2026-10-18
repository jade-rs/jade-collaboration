//! Merged table cells in docx: merging shows the covered cells' content in
//! the top-left cell (as jade concatenates it), unmerging gives the freed
//! cells new empty stories, and text edits into any cell commute with both.

mod common;

use common::{jade_core, open};
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
    let reloaded = JadeDocx::load(&s.jade.export(&s.model).unwrap()).unwrap();
    assert_eq!(reloaded.body_paragraph_texts(), s.model.paragraph_texts());
}

#[test]
fn rows_and_columns_through_a_merge_are_refused() {
    let mut s = Session::open("jade-core/hello.docx");
    let (table, rows, columns) = s.table();
    s.step(DocxOp::MergeCells {
        table,
        merge: region(&rows, &columns, 0, 0, 1, 1),
        first: Id::new(7_000_000, ReplicaId(3)),
    });
    let refused = |s: &mut Session, op: GridOp| {
        let before = s.model.clone();
        let op = DocxOp::Table { table, op };
        let err = s.jade.admit(&s.model, &op).err().or_else(|| {
            s.model
                .apply(
                    &op,
                    &ApplyContext {
                        origin: ReplicaId(1),
                        base_seq: 0,
                        seq: None,
                    },
                )
                .err()
        });
        assert!(err.is_some(), "{op:?} was accepted");
        assert_eq!(s.model, before);
    };
    let id = s.id();
    refused(
        &mut s,
        GridOp::InsertRow {
            id,
            after: Some(rows[0]),
        },
    );
    refused(&mut s, GridOp::DeleteRow { id: rows[1] });
    let id = s.id();
    refused(
        &mut s,
        GridOp::InsertColumn {
            id,
            after: Some(columns[0]),
        },
    );
    refused(&mut s, GridOp::DeleteColumn { id: columns[0] });
    // Outside the merge they still work.
    let id = s.id();
    s.step(DocxOp::Table {
        table,
        op: GridOp::InsertRow {
            id,
            after: Some(rows[2]),
        },
    });
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
