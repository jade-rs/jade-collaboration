//! Excel tables (ListObjects): structure inside a table, header renames, and
//! what Excel does on its own (naming a new column, normalising a header),
//! which the server learns from jade and commits as derived operations.

mod common;

use common::{assert_agree, open};
use jade_collab_core::grid::GridOp;
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document, Lane, Materializer};
use jade_xlsx_collab::{CellInput, JadeXlsx, XlsxModel, XlsxOp};

struct Session {
    model: XlsxModel,
    jade: JadeXlsx,
    seq: u64,
    next: u64,
}

impl Session {
    fn open() -> Self {
        let (model, jade) = open("grid-operations-workbook.xlsx");
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

    /// Applies `op` as the sequencer does, derived operations included.
    fn step(&mut self, op: XlsxOp) -> Result<(), String> {
        let ctx = ApplyContext {
            origin: ReplicaId(1),
            base_seq: self.seq,
            seq: Some(self.seq + 1),
        };
        if XlsxModel::lane(&op) == Lane::Sequenced {
            self.jade
                .admit(&self.model, &op)
                .map_err(|e| e.to_string())?;
        }
        let effect = self.model.apply(&op, &ctx).map_err(|e| e.to_string())?;
        self.seq += 1;
        self.jade.materialize(&self.model, effect, &ctx)?;
        for derived in self.jade.derived(&self.model, &op).unwrap() {
            self.seq += 1;
            let ctx = ApplyContext {
                origin: ReplicaId::ORIGIN,
                base_seq: self.seq - 1,
                seq: Some(self.seq),
            };
            let effect = self
                .model
                .apply(&derived, &ctx)
                .map_err(|e| e.to_string())?;
            self.jade.materialize(&self.model, effect, &ctx)?;
        }
        assert_agree(&self.model, &self.jade, &format!("after {op:?}"));
        Ok(())
    }

    fn cell(&self, reference: &str) -> (Id, Id, Id) {
        self.model.locate("Operations", reference).unwrap()
    }

    fn value(&self, reference: &str) -> Option<CellInput> {
        let (sheet, row, column) = self.cell(reference);
        self.model.value(sheet, row, column).cloned()
    }
}

#[test]
fn a_row_inserted_inside_a_table_extends_it() {
    let mut s = Session::open();
    let (sheet, row2, _) = s.cell("A2");
    let id = s.id();
    s.step(XlsxOp::Structure {
        sheet,
        op: GridOp::InsertRow {
            id,
            after: Some(row2),
        },
    })
    .unwrap();
    // The table now ends a row lower: its last data row moved to row 5.
    assert_eq!(s.value("A5"), Some(CellInput::Text("Gamma".into())));
    let reopened = JadeXlsx::open(&s.jade.export().unwrap()).unwrap();
    assert_agree(&s.model, &reopened, "after export");
}

#[test]
fn a_column_inserted_inside_a_table_is_named_by_excel_and_learnt() {
    let mut s = Session::open();
    let (sheet, _, column_a) = s.cell("A1");
    let id = s.id();
    s.step(XlsxOp::Structure {
        sheet,
        op: GridOp::InsertColumn {
            id,
            after: Some(column_a),
        },
    })
    .unwrap();
    // jade named the new header; the model learnt it from a derived op.
    assert_eq!(s.value("B1"), Some(CellInput::Text("Column1".into())));
    // And the new header cell is protected like the others.
    let (sheet, row, column) = s.cell("B1");
    let refused = s.model.clone().apply(
        &XlsxOp::SetCell {
            sheet,
            row,
            column,
            stamp: Id::new(9_000_000, ReplicaId(1)),
            value: CellInput::Blank,
        },
        &ApplyContext {
            origin: ReplicaId(1),
            base_seq: 0,
            seq: None,
        },
    );
    assert!(refused.is_err());
}

#[test]
fn headers_are_renamed_with_set_header_and_normalised_by_excel() {
    let mut s = Session::open();
    let (sheet, row, column) = s.cell("A1");
    let stamp = s.id();
    s.step(XlsxOp::SetHeader {
        sheet,
        row,
        column,
        stamp,
        text: "Label".into(),
    })
    .unwrap();
    assert_eq!(s.value("A1"), Some(CellInput::Text("Label".into())));
    // A blank header is what Excel never keeps: jade names it, and the
    // server tells everyone.
    let stamp = s.id();
    s.step(XlsxOp::SetHeader {
        sheet,
        row,
        column,
        stamp,
        text: String::new(),
    })
    .unwrap();
    let named = s.value("A1");
    assert!(
        matches!(&named, Some(CellInput::Text(t)) if !t.is_empty()),
        "{named:?}"
    );
}

#[test]
fn a_data_row_inside_a_table_can_be_deleted_but_not_the_header() {
    let mut s = Session::open();
    let (sheet, row3, _) = s.cell("A3");
    s.step(XlsxOp::Structure {
        sheet,
        op: GridOp::DeleteRow { id: row3 },
    })
    .unwrap();
    assert_eq!(s.value("A3"), Some(CellInput::Text("Gamma".into())));
    let (_, header, _) = s.cell("A1");
    assert!(s
        .step(XlsxOp::Structure {
            sheet,
            op: GridOp::DeleteRow { id: header },
        })
        .is_err());
}

#[test]
fn deleting_the_last_data_row_moves_the_table_end() {
    let mut s = Session::open();
    let (sheet, row4, _) = s.cell("A4");
    s.step(XlsxOp::Structure {
        sheet,
        op: GridOp::DeleteRow { id: row4 },
    })
    .unwrap();
    // A row inserted below the table now does not join it.
    let (_, row3, _) = s.cell("A3");
    let id = s.id();
    s.step(XlsxOp::Structure {
        sheet,
        op: GridOp::InsertRow {
            id,
            after: Some(row3),
        },
    })
    .unwrap();
}

#[test]
fn only_the_server_can_commit_derived_operations() {
    let (mut model, _) = open("grid-operations-workbook.xlsx");
    let (sheet, row, column) = model.locate("Operations", "A1").unwrap();
    let derived = XlsxOp::Derived {
        sheet,
        row,
        column,
        stamp: Id::new(9_000_000, ReplicaId::ORIGIN),
        value: CellInput::Text("x".into()),
    };
    let from = |origin| ApplyContext {
        origin,
        base_seq: 0,
        seq: Some(1),
    };
    assert!(model.clone().apply(&derived, &from(ReplicaId(3))).is_err());
    let effect = model.apply(&derived, &from(ReplicaId::ORIGIN)).unwrap();
    assert!(effect.is_empty(), "jade already holds what it derived");
}
