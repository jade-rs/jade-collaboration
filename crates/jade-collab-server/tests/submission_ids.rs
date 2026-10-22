//! Allocation namespaces are admission, not ownership of referenced content.
use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_core::marks::Boundary;
use jade_collab_sync::protocol::{Committed, Downstream, Submission};
use jade_collab_sync::{
    ApplyContext, Document, NoMaterializer, Record, Replica, Sequencer, SubmitOutcome,
};
use jade_docx_collab::{
    Anchor, CommentContent, DocxModel, DocxOp, FormatChange, NoteKind, StoryId,
};
use jade_pptx_collab::{CharFormat, Geometry, GeometryEdit, Image, PptxModel, PptxOp};
use jade_xlsx_collab::{CellInput, LineAxis, RangeBorders, XlsxModel, XlsxOp};
use proptest::prelude::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

fn id(n: u64) -> Id {
    Id::new(n, ReplicaId::ORIGIN)
}
fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId::ORIGIN,
        base_seq: 0,
        seq: None,
    }
}
fn check_fields<D: Document>(model: &D, op: D::Op, fields: &[&str])
where
    D::Op: Serialize + DeserializeOwned,
{
    let caller = ReplicaId(7);
    let mut wire = serde_json::to_value(op).unwrap();
    for field in fields {
        wire.pointer_mut(&format!("{field}/replica"))
            .unwrap()
            .clone_from(&Value::from(caller.0));
    }
    let owned: D::Op = serde_json::from_value(wire.clone()).unwrap();
    model.validate_submission(&owned, caller).unwrap();
    for field in fields {
        for foreign in [ReplicaId::ORIGIN, ReplicaId(8), ReplicaId(999)] {
            let mut forged = wire.clone();
            *forged.pointer_mut(&format!("{field}/replica")).unwrap() = Value::from(foreign.0);
            let forged: D::Op = serde_json::from_value(forged).unwrap();
            assert!(
                model.validate_submission(&forged, caller).is_err(),
                "unchecked new ID {field} in {owned:?}"
            );
        }
    }
}

#[test]
fn docx_checks_every_explicit_minted_field_but_allows_foreign_references() {
    let model = DocxModel::from_paragraphs(&["body"]);
    let story = StoryId::Body;
    let cases = vec![
        (
            DocxOp::InsertText {
                story,
                parent: Some(id(1)),
                first: id(100),
                text: "x".into(),
            },
            vec!["/first"],
        ),
        (
            DocxOp::TrackInsertText {
                story,
                parent: Some(id(1)),
                first: id(100),
                text: "x".into(),
                date: None,
            },
            vec!["/first"],
        ),
        (
            DocxOp::TrackDeleteText {
                story,
                ids: vec![id(1)],
                revision: id(100),
                date: None,
            },
            vec!["/revision"],
        ),
        (
            DocxOp::SplitParagraph {
                story,
                parent: Some(id(1)),
                id: id(100),
            },
            vec!["/id"],
        ),
        (
            DocxOp::InsertTable {
                anchor: Anchor::Start,
                id: id(100),
                rows: vec![id(101), id(102)],
                columns: vec![id(103), id(104)],
            },
            vec!["/id", "/rows/0", "/rows/1", "/columns/0", "/columns/1"],
        ),
        (
            DocxOp::Table {
                table: id(1),
                op: GridOp::InsertRow {
                    id: id(100),
                    after: Some(id(2)),
                },
            },
            vec!["/op/id"],
        ),
        (
            DocxOp::Table {
                table: id(1),
                op: GridOp::InsertColumn {
                    id: id(100),
                    after: Some(id(2)),
                },
            },
            vec!["/op/id"],
        ),
        (
            DocxOp::MergeCells {
                table: id(1),
                merge: Merge {
                    top: id(2),
                    bottom: id(3),
                    left: id(4),
                    right: id(5),
                },
                first: id(100),
            },
            vec!["/first"],
        ),
        (
            DocxOp::Mark {
                story,
                id: id(100),
                start: Boundary::Before(id(1)),
                end: Boundary::After(id(2)),
                change: FormatChange::Bold { value: true },
            },
            vec!["/id"],
        ),
        (
            DocxOp::AddComment {
                story,
                start: id(1),
                end: id(2),
                id: id(100),
                content: CommentContent::new("comment"),
            },
            vec!["/id"],
        ),
        (
            DocxOp::ReplyToComment {
                comment: id(1),
                id: id(100),
                content: CommentContent::new("reply"),
            },
            vec!["/id"],
        ),
        (
            DocxOp::InsertNote {
                kind: NoteKind::Footnote,
                story,
                parent: Some(id(1)),
                id: id(100),
                text: "note".into(),
            },
            vec!["/id"],
        ),
        (
            DocxOp::InsertTextBox {
                story,
                parent: Some(id(1)),
                id: id(100),
                text: "box".into(),
                width_px: 100,
                height_px: 100,
            },
            vec!["/id"],
        ),
        (
            DocxOp::DeleteText {
                story,
                ids: vec![id(1)],
            },
            vec![],
        ),
        (DocxOp::JoinParagraph { story, mark: id(1) }, vec![]),
        (
            DocxOp::Table {
                table: id(1),
                op: GridOp::DeleteRow { id: id(2) },
            },
            vec![],
        ),
        (
            DocxOp::Format {
                story,
                start: id(1),
                end: id(2),
                change: FormatChange::Bold { value: true },
            },
            vec![],
        ),
        (
            DocxOp::AcceptRevisions {
                revisions: vec![id(1)],
            },
            vec![],
        ),
    ];
    for (op, fields) in cases {
        check_fields(&model, op, &fields);
    }
}

#[test]
fn xlsx_checks_every_write_stamp_and_inserted_line_but_allows_shared_targets() {
    let model = XlsxModel::blank(&[("Sheet1", 2, 2)]);
    let (sheet, row, column) = (id(1), id(2), id(3));
    let stamp = id(100);
    let format = jade_xlsx_collab::FormatChange::default();
    let cases = vec![
        (
            XlsxOp::SetCell {
                sheet,
                row,
                column,
                stamp,
                value: CellInput::Blank,
            },
            vec!["/stamp"],
        ),
        (
            XlsxOp::SetNote {
                sheet,
                row,
                column,
                stamp,
                note: Some("note".into()),
            },
            vec!["/stamp"],
        ),
        (
            XlsxOp::SetFormat {
                sheet,
                rows: vec![row],
                columns: vec![column],
                stamp,
                change: format.clone(),
            },
            vec!["/stamp"],
        ),
        (
            XlsxOp::SetBorders {
                sheet,
                rows: vec![row],
                columns: vec![column],
                stamp,
                borders: RangeBorders::default(),
            },
            vec!["/stamp"],
        ),
        (
            XlsxOp::SetLineFormat {
                sheet,
                axis: LineAxis::Rows,
                lines: vec![row],
                stamp,
                change: format,
            },
            vec!["/stamp"],
        ),
        (
            XlsxOp::MergeCells {
                sheet,
                merge: Merge {
                    top: row,
                    bottom: row,
                    left: column,
                    right: column,
                },
                stamp,
            },
            vec!["/stamp"],
        ),
        (
            XlsxOp::SetHeader {
                sheet,
                row,
                column,
                stamp,
                text: "header".into(),
            },
            vec!["/stamp"],
        ),
        (
            XlsxOp::AddSheet {
                id: stamp,
                name: "New".into(),
                after: Some(sheet),
                rows: 1,
                columns: 1,
            },
            vec!["/id"],
        ),
        (
            XlsxOp::Structure {
                sheet,
                op: GridOp::InsertRow {
                    id: stamp,
                    after: Some(row),
                },
            },
            vec!["/op/id"],
        ),
        (
            XlsxOp::Structure {
                sheet,
                op: GridOp::InsertColumn {
                    id: stamp,
                    after: Some(column),
                },
            },
            vec!["/op/id"],
        ),
        (
            XlsxOp::Structure {
                sheet,
                op: GridOp::DeleteColumn { id: column },
            },
            vec![],
        ),
        (
            XlsxOp::UnmergeCells {
                sheet,
                top: row,
                left: column,
            },
            vec![],
        ),
        (
            XlsxOp::RenameSheet {
                sheet,
                name: "Renamed".into(),
            },
            vec![],
        ),
    ];
    for (op, fields) in cases {
        check_fields(&model, op, &fields);
    }
}

#[test]
fn pptx_checks_every_minted_id_and_geometry_stamp_but_allows_shared_shapes() {
    let model = PptxModel::from_texts(&[&["body"]]);
    let (slide, shape) = (id(1), id(2));
    let story = jade_pptx_collab::StoryId::Shape { slide, shape };
    let geometry = Geometry {
        x: 0,
        y: 0,
        width: 100,
        height: 100,
        rotation: 0,
    };
    let cases = vec![
        (
            PptxOp::InsertText {
                story,
                parent: Some(id(3)),
                first: id(100),
                text: "x".into(),
            },
            vec!["/first"],
        ),
        (
            PptxOp::SplitParagraph {
                story,
                parent: Some(id(3)),
                id: id(100),
            },
            vec!["/id"],
        ),
        (
            PptxOp::SetGeometry {
                slide,
                shape,
                stamp: id(100),
                edit: GeometryEdit {
                    x: Some(10),
                    ..Default::default()
                },
            },
            vec!["/stamp"],
        ),
        (
            PptxOp::InsertSlide {
                id: id(100),
                after: Some(slide),
            },
            vec!["/id"],
        ),
        (PptxOp::DuplicateSlide { slide, id: id(100) }, vec!["/id"]),
        (
            PptxOp::InsertTextBox {
                slide,
                id: id(100),
                geometry,
            },
            vec!["/id"],
        ),
        (
            PptxOp::InsertPicture {
                slide,
                id: id(100),
                geometry,
                image: Image(vec![]),
            },
            vec!["/id"],
        ),
        (
            PptxOp::SetSlideHidden {
                slide,
                stamp: id(100),
                hidden: true,
            },
            vec!["/stamp"],
        ),
        (
            PptxOp::Mark {
                story,
                id: id(100),
                start: Boundary::Before(id(3)),
                end: Boundary::End,
                format: CharFormat::Bold { value: true },
            },
            vec!["/id"],
        ),
        (
            PptxOp::ResizeShape {
                slide,
                shape,
                stamp: id(100),
                width: 100,
                height: 100,
            },
            vec!["/stamp"],
        ),
        (
            PptxOp::DeleteText {
                story,
                ids: vec![id(3)],
            },
            vec![],
        ),
        (PptxOp::DeleteShape { slide, shape }, vec![]),
        (
            PptxOp::MoveSlide {
                slide,
                after: Some(id(9)),
            },
            vec![],
        ),
    ];
    for (op, fields) in cases {
        check_fields(&model, op, &fields);
    }
}

proptest! {
    #[test]
    fn docx_merge_allocations_distinguish_unused_sentinels_from_new_stories(rows in 1usize..5, columns in 1usize..5) {
        let mut model=DocxModel::from_paragraphs(&["body"]);
        let table=id(100);let rs:Vec<_>=(0..rows).map(|n|id(101+n as u64)).collect();
        let cs:Vec<_>=(0..columns).map(|n|id(201+n as u64)).collect();
        model.apply(&DocxOp::InsertTable {anchor:Anchor::Start,id:table,rows:rs.clone(),columns:cs.clone()},&ctx()).unwrap();
        let merge=Merge {top:rs[0],bottom:*rs.last().unwrap(),left:cs[0],right:*cs.last().unwrap()};
        let merge_op=DocxOp::MergeCells {table,merge,first:id(1000)};
        prop_assert_eq!(model.validate_submission(&merge_op,ReplicaId(7)).is_ok(),rows==1);
        if rows*columns==1 {return Ok(());}
        model.apply(&merge_op,&ctx()).unwrap();
        let unmerge=DocxOp::UnmergeCells {table,top:rs[0],left:cs[0],first:id(2000)};
        prop_assert_eq!(model.validate_submission(&unmerge,ReplicaId(7)).is_ok(),columns==1);
        // Owned allocation always passes; only actual unused fields bypass it.
        let owned=Id::new(3000,ReplicaId(7));
        prop_assert!(model.validate_submission(&DocxOp::UnmergeCells {table,top:rs[0],left:cs[0],first:owned},ReplicaId(7)).is_ok(),"owned allocation passes");
    }
}

// Independent append/count references exercise real format models through
// forged/valid submissions, duplicate tickets, resume, compaction and replay.
fn history<D>(
    model: D,
    steps: &[(u8, u8)],
    make: fn(&D, Id, char) -> D::Op,
    read: fn(&D) -> String,
    append: bool,
) where
    D: Document + std::fmt::Debug + PartialEq,
    D::Op: Serialize + DeserializeOwned,
{
    let mut seq = Sequencer::new(model);
    let a = seq.join();
    let b = seq.join();
    let mut replica = Replica::new(ReplicaId(3), 0, a.snapshot);
    let mut ticket = 0;
    let mut accepted = 0;
    let mut expected = read(seq.document());
    let mut last = None;
    for (index, &(action, value)) in steps.iter().enumerate() {
        let ch = char::from(b'a' + value % 26);
        let fresh = Id::new(100_000 + index as u64, a.replica);
        match action {
            0..=2 => {
                ticket += 1;
                let owner = if action == 0 {
                    a.replica
                } else if action == 1 {
                    ReplicaId::ORIGIN
                } else {
                    b.replica
                };
                let op = make(
                    seq.document(),
                    Id {
                        replica: owner,
                        ..fresh
                    },
                    ch,
                );
                let before = seq.checkpoint();
                if action != 0 {
                    let local_before = replica.clone();
                    assert!(replica.submit(op.clone()).is_err());
                    assert_eq!(replica.view(), local_before.view());
                    assert_eq!(replica.next_client_seq(), local_before.next_client_seq());
                    assert_eq!(
                        replica.pending_submissions(),
                        local_before.pending_submissions()
                    );
                }
                let submission = Submission {
                    client_seq: ticket,
                    base_seq: seq.seq(),
                    op,
                };
                let result = seq.submit(a.replica, submission.clone());
                last = Some(submission);
                if action == 0 {
                    let SubmitOutcome::Committed(committed) = result else {
                        panic!("owned operation refused")
                    };
                    accepted += 1;
                    if append {
                        expected.insert(0, ch);
                    } else {
                        expected = ch.to_string();
                    }
                    replica.receive(Downstream::Committed(committed)).unwrap();
                } else {
                    assert!(matches!(result, SubmitOutcome::Rejected(_)));
                    assert_eq!(seq.document(), &before.document);
                    assert_eq!(seq.seq(), before.seq);
                }
                assert_eq!(seq.processed_client_seq(a.replica), ticket);
            }
            3 => {
                if let Some(submission) = last.clone() {
                    let before = seq.checkpoint();
                    assert!(matches!(
                        seq.submit(a.replica, submission),
                        SubmitOutcome::Duplicate
                    ));
                    assert_eq!(seq.checkpoint(), before);
                }
            }
            4 => {
                let resumed = seq.resume(a.replica, seq.seq()).unwrap();
                assert_eq!(resumed.processed_client_seq, ticket);
                assert!(resumed.missed.is_empty());
            }
            _ => {
                let checkpoint = seq.checkpoint();
                seq.compact_keeping(2);
                seq = Sequencer::restore_with_tail(checkpoint, seq.log().to_vec(), NoMaterializer)
                    .unwrap();
            }
        }
        assert_eq!(seq.seq(), accepted);
        assert_eq!(read(seq.document()), expected);
        assert_eq!(read(replica.view()), expected);
    }
}
fn docx_op(_: &DocxModel, first: Id, ch: char) -> DocxOp {
    DocxOp::InsertText {
        story: StoryId::Body,
        parent: None,
        first,
        text: ch.to_string(),
    }
}
fn docx_read(m: &DocxModel) -> String {
    m.paragraph_texts()[0].clone()
}
fn xlsx_op(m: &XlsxModel, stamp: Id, ch: char) -> XlsxOp {
    let s = &m.sheets()[0];
    XlsxOp::SetNote {
        sheet: s.id,
        row: s.rows[0],
        column: s.columns[0],
        stamp,
        note: Some(ch.to_string()),
    }
}
fn xlsx_read(m: &XlsxModel) -> String {
    let s = &m.sheets()[0];
    m.note(s.id, s.rows[0], s.columns[0]).unwrap_or("").into()
}
fn pptx_op(m: &PptxModel, first: Id, ch: char) -> PptxOp {
    let s = &m.slides()[0];
    PptxOp::InsertText {
        story: jade_pptx_collab::StoryId::Shape {
            slide: s.id,
            shape: s.shapes[0].id,
        },
        parent: None,
        first,
        text: ch.to_string(),
    }
}
fn pptx_read(m: &PptxModel) -> String {
    m.slides()[0].shapes[0].text.clone().unwrap()
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn stateful_namespace_admission_matches_native_text_and_register_models(steps in prop::collection::vec((0u8..6,any::<u8>()),1..90)) {
        history(DocxModel::from_paragraphs(&["base"]),&steps,docx_op,docx_read,true);
        history(XlsxModel::blank(&[("S",2,2)]),&steps,xlsx_op,xlsx_read,false);
        history(PptxModel::from_texts(&[&["base"]]),&steps,pptx_op,pptx_read,true);
    }
}

fn old_facts<D>(model: D, make: fn(&D, Id, char) -> D::Op)
where
    D: Document + std::fmt::Debug + PartialEq,
{
    let mut seq = Sequencer::new(model);
    let a = seq.join();
    let b = seq.join();
    let fact = Committed {
        seq: 1,
        origin: a.replica,
        client_seq: 1,
        base_seq: 0,
        op: make(seq.document(), Id::new(1000, b.replica), 'x'),
    };
    seq.replay(Record::Committed(fact.clone())).unwrap();
    let mut observer = Replica::new(ReplicaId(3), 0, a.snapshot);
    observer.receive(Downstream::Committed(fact)).unwrap();
    assert_eq!(observer.view(), seq.document());
    let before = seq.checkpoint();
    let op = make(seq.document(), Id::new(1001, b.replica), 'y');
    assert!(matches!(
        seq.submit(
            a.replica,
            Submission {
                client_seq: 2,
                base_seq: 1,
                op
            }
        ),
        SubmitOutcome::Rejected(_)
    ));
    assert_eq!(seq.document(), &before.document);
    let op = make(seq.document(), id(1002), 'z');
    let origin = seq.commit_origin(op).unwrap();
    observer.receive(Downstream::Committed(origin)).unwrap();
    assert_eq!(observer.view(), seq.document());
}
#[test]
fn historical_foreign_ids_and_server_origin_facts_keep_their_original_meaning() {
    old_facts(DocxModel::from_paragraphs(&["base"]), docx_op);
    old_facts(XlsxModel::blank(&[("S", 2, 2)]), xlsx_op);
    old_facts(PptxModel::from_texts(&[&["base"]]), pptx_op);
}

#[test]
fn a_bad_namespace_is_refused_before_native_admission_staging_or_application() {
    use jade_collab_sync::{Lane, Materializer, Rejection};
    #[derive(Clone, Debug, PartialEq)]
    struct Guarded;
    impl Document for Guarded {
        type Op = Id;
        type Effect = ();
        fn lane(_: &Id) -> Lane {
            Lane::Sequenced
        }
        fn max_lamport(&self) -> u64 {
            0
        }
        fn validate_submission(&self, op: &Id, from: ReplicaId) -> Result<(), Rejection> {
            jade_collab_sync::check_id_owner(*op, from)
        }
        fn apply(&mut self, _: &Id, _: &ApplyContext) -> Result<(), Rejection> {
            panic!("refusal must precede model application")
        }
    }
    struct Native;
    impl Materializer<Guarded> for Native {
        fn may_derive(&self, _: &Guarded, _: &Id) -> bool {
            panic!("refusal must precede native work")
        }
        fn fork(&self, _: &Guarded) -> Result<Self, String> {
            panic!("refusal must precede native staging")
        }
        fn materialize(&mut self, _: &Guarded, _: (), _: &ApplyContext) -> Result<(), String> {
            panic!("refusal must precede materialization")
        }
    }
    let mut seq = Sequencer::with_materializer(Guarded, Native);
    let a = seq.join();
    assert!(matches!(
        seq.submit(
            a.replica,
            Submission {
                client_seq: 1,
                base_seq: 0,
                op: id(100)
            }
        ),
        SubmitOutcome::Rejected(_)
    ));
    assert_eq!(seq.seq(), 0);
    assert_eq!(seq.processed_client_seq(a.replica), 1);
    assert!(seq.faults().is_empty());
}
