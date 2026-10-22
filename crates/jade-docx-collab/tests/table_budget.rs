use jade_collab_core::grid::{GridOp, Merge};
use jade_collab_core::id::{Id, ReplicaId};
use jade_collab_sync::{ApplyContext, Document};
use jade_docx_collab::{Anchor, DocxModel, DocxOp};

fn id(time: u64) -> Id {
    Id::new(time, ReplicaId(1))
}
fn ctx() -> ApplyContext {
    ApplyContext {
        origin: ReplicaId(1),
        base_seq: 0,
        seq: None,
    }
}
fn insert(rows: usize, columns: usize) -> DocxOp {
    DocxOp::InsertTable {
        anchor: Anchor::Start,
        id: id(1_000_000),
        rows: (0..rows).map(|n| id(1_000_001 + n as u64)).collect(),
        columns: (0..columns)
            .map(|n| id(1_000_001 + rows as u64 + n as u64))
            .collect(),
    }
}

#[test]
fn a_compact_rectangular_request_cannot_expand_past_the_cell_budget() {
    let mut model = DocxModel::from_paragraphs(&["body"]);
    let before = model.clone();
    let outcome = model.apply(&insert(101, 100), &ctx());
    assert!(
        outcome.is_err(),
        "101 small row/column ids multiplied into 10,100 cell stories"
    );
    assert_eq!(
        model, before,
        "refusal is atomic, including the observed Lamport clock"
    );
    model.apply(&insert(2, 3), &ctx()).unwrap();
    assert_eq!(model.tables()[0].rows.len(), 2);
}

#[test]
fn older_committed_large_tables_remain_replayable_but_new_submissions_are_refused() {
    use jade_collab_sync::protocol::{Committed, Downstream, Submission};
    use jade_collab_sync::{NoMaterializer, Record, Replica, Sequencer, SubmitOutcome};
    let mut live = Sequencer::new(DocxModel::from_paragraphs(&["body"]));
    let welcome = live.join();
    let checkpoint = live.checkpoint();
    let outcome = live.submit(
        welcome.replica,
        Submission {
            client_seq: 1,
            base_seq: 0,
            op: insert(101, 100),
        },
    );
    assert!(matches!(outcome, SubmitOutcome::Rejected(_)));
    let fact = Committed {
        seq: 1,
        origin: welcome.replica,
        client_seq: 1,
        base_seq: 0,
        op: insert(101, 100),
    };
    let mut restored = Sequencer::restore(checkpoint, NoMaterializer);
    restored
        .replay(Record::Committed(fact.clone()))
        .expect("resource admission must not invalidate historical facts");
    // A different subscriber receives the same historical fact without a
    // pending optimistic operation from the committing replica.
    let mut observer = Replica::new(ReplicaId(2), 0, welcome.snapshot);
    observer.receive(Downstream::Committed(fact)).unwrap();
    assert_eq!(observer.view(), restored.document());
    let before = restored.checkpoint();
    assert!(
        restored
            .replay(Record::Committed(Committed {
                seq: 2,
                origin: welcome.replica,
                client_seq: 2,
                base_seq: 1,
                op: insert(101, 100)
            }))
            .is_err(),
        "trusted replay still enforces duplicate-id validity"
    );
    assert_eq!(restored.checkpoint(), before);
}

#[test]
fn a_wide_sparse_import_remains_editable_without_expanding_its_absent_cells() {
    use jade_collab_sync::protocol::Submission;
    use jade_collab_sync::{Sequencer, SubmitOutcome};
    use jade_docx_collab::JadeDocx;
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx"),
    )
    .unwrap();
    let mut package = jade_opc::OpcPackage::open(&bytes).unwrap();
    let xml = format!(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>body</w:t></w:r></w:p><w:tbl><w:tblGrid>{}</w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val="10001"/></w:tcPr><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>end</w:t></w:r></w:p></w:body></w:document>"#,
        r#"<w:gridCol w:w="1"/>"#.repeat(10001)
    );
    package.replace("word/document.xml", xml.into_bytes());
    let jade = JadeDocx::load(&package.write().unwrap()).unwrap();
    let model = DocxModel::from_word_document(jade.word()).unwrap();
    assert_eq!(model.tables()[0].columns.len(), 10001);
    assert_eq!(model.paragraph_texts(), vec!["body", "cell", "end"]);
    let mut sequencer = Sequencer::with_materializer(model, jade);
    let replica = sequencer.join().replica;
    let paragraph = sequencer.document().paragraphs().remove(1);
    let operation = DocxOp::InsertText {
        story: paragraph.story,
        parent: paragraph.chars.last().copied(),
        first: Id::new(sequencer.document().max_lamport() + 1, replica),
        text: "!".into(),
    };
    assert!(matches!(
        sequencer.submit(
            replica,
            Submission {
                client_seq: 1,
                base_seq: 0,
                op: operation
            }
        ),
        SubmitOutcome::Committed(_)
    ));
    assert_eq!(
        sequencer.document().paragraph_texts(),
        vec!["body", "cell!", "end"]
    );
    let reloaded = JadeDocx::load(
        &sequencer
            .materializer()
            .export(sequencer.document())
            .unwrap(),
    )
    .unwrap();
    let reread = DocxModel::from_word_document(reloaded.word()).unwrap();
    assert_eq!(reread.paragraph_texts(), vec!["body", "cell!", "end"]);
    assert_eq!(reread.tables()[0].columns.len(), 10001);
    assert!(sequencer.faults().is_empty());
    let table = sequencer.document().tables().remove(0);
    assert!(matches!(
        sequencer.submit(
            replica,
            Submission {
                client_seq: 2,
                base_seq: 1,
                op: DocxOp::Table {
                    table: table.id,
                    op: GridOp::DeleteColumn {
                        id: table.columns[0]
                    }
                }
            }
        ),
        SubmitOutcome::Committed(_)
    ));
    assert_eq!(sequencer.document().tables()[0].columns.len(), 10000);
    let before = sequencer.checkpoint();
    let file = sequencer.materializer().export_base().unwrap();
    let op = DocxOp::Table {
        table: table.id,
        op: GridOp::InsertRow {
            id: Id::new(sequencer.document().max_lamport() + 1, replica),
            after: None,
        },
    };
    assert!(
        matches!(
            sequencer.submit(
                replica,
                Submission {
                    client_seq: 3,
                    base_seq: 2,
                    op
                }
            ),
            SubmitOutcome::Rejected(_)
        ),
        "a new row creates stories for tombstoned columns too"
    );
    assert_eq!(sequencer.document(), &before.document);
    assert_eq!(sequencer.seq(), before.seq);
    assert_eq!(sequencer.materializer().export_base().unwrap(), file);
}

#[test]
fn exact_creation_fits_and_incremental_growth_remains_available() {
    let mut model = DocxModel::from_paragraphs(&["body"]);
    model.apply(&insert(100, 100), &ctx()).unwrap();
    let table = model.tables().remove(0);
    model
        .apply(
            &DocxOp::Table {
                table: table.id,
                op: GridOp::InsertRow {
                    id: id(8_000_000),
                    after: table.rows.last().copied(),
                },
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(
        model.tables()[0].rows.len(),
        101,
        "the budget is per operation, not a total-document cap"
    );
}

#[test]
fn line_insertion_counts_every_new_story_before_changing_the_grid() {
    for rows_axis in [false, true] {
        let mut model = DocxModel::from_paragraphs(&["body"]);
        model
            .apply(
                &insert(
                    if rows_axis { 1 } else { 10000 },
                    if rows_axis { 10000 } else { 1 },
                ),
                &ctx(),
            )
            .unwrap();
        let table = model.tables().remove(0);
        let grow = if rows_axis {
            GridOp::InsertColumn {
                id: id(8_000_000),
                after: table.columns.last().copied(),
            }
        } else {
            GridOp::InsertRow {
                id: id(8_000_000),
                after: table.rows.last().copied(),
            }
        };
        model
            .apply(
                &DocxOp::Table {
                    table: table.id,
                    op: grow,
                },
                &ctx(),
            )
            .unwrap();
        let before = model.clone();
        let too_many = if rows_axis {
            GridOp::InsertRow {
                id: id(8_000_001),
                after: None,
            }
        } else {
            GridOp::InsertColumn {
                id: id(8_000_001),
                after: None,
            }
        };
        assert!(model
            .apply(
                &DocxOp::Table {
                    table: table.id,
                    op: too_many
                },
                &ctx()
            )
            .is_err());
        assert_eq!(model, before);
    }
}

#[test]
fn merge_checks_rectangle_size_before_collecting_positions() {
    let mut model = DocxModel::from_paragraphs(&["body"]);
    model.apply(&insert(100, 100), &ctx()).unwrap();
    let table = model.tables().remove(0);
    model
        .apply(
            &DocxOp::Table {
                table: table.id,
                op: GridOp::InsertRow {
                    id: id(8_000_000),
                    after: table.rows.last().copied(),
                },
            },
            &ctx(),
        )
        .unwrap();
    let table = model.tables().remove(0);
    let before = model.clone();
    let merge = Merge {
        top: table.rows[0],
        bottom: *table.rows.last().unwrap(),
        left: table.columns[0],
        right: *table.columns.last().unwrap(),
    };
    assert!(model
        .apply(
            &DocxOp::MergeCells {
                table: table.id,
                merge,
                first: id(9_000_000)
            },
            &ctx()
        )
        .is_err());
    assert_eq!(model, before);
}

#[test]
fn oversized_intents_refuse_before_reserving_any_story_ids() {
    use jade_docx_collab::intent::{try_resolve, IntentOp};
    for unmerge in [false, true] {
        let mut model = DocxModel::from_paragraphs(&["body"]);
        model.apply(&insert(100, 100), &ctx()).unwrap();
        let table = model.tables().remove(0);
        if unmerge {
            model
                .apply(
                    &DocxOp::MergeCells {
                        table: table.id,
                        merge: Merge {
                            top: table.rows[0],
                            bottom: table.rows[99],
                            left: table.columns[0],
                            right: table.columns[99],
                        },
                        first: id(9_000_000),
                    },
                    &ctx(),
                )
                .unwrap();
        }
        model
            .apply(
                &DocxOp::Table {
                    table: table.id,
                    op: GridOp::InsertRow {
                        id: id(8_000_000),
                        after: Some(table.rows[0]),
                    },
                },
                &ctx(),
            )
            .unwrap();
        let before = model.clone();
        let intent = if unmerge {
            IntentOp::UnmergeTableCells {
                table: 0,
                row: 0,
                column: 0,
            }
        } else {
            IntentOp::MergeTableCells {
                table: 0,
                top_row: 0,
                left_column: 0,
                bottom_row: 100,
                right_column: 99,
            }
        };
        let mut reservations = 0;
        let result = try_resolve(&model, &intent, &mut |_| {
            reservations += 1;
            Ok(id(10_000_000))
        });
        assert!(
            result.is_err(),
            "a large intent must be refused while planning"
        );
        assert_eq!(reservations, 0);
        assert_eq!(model, before);
        if unmerge {
            assert!(model
                .apply(
                    &DocxOp::UnmergeCells {
                        table: table.id,
                        top: table.rows[0],
                        left: table.columns[0],
                        first: id(10_000_000)
                    },
                    &ctx()
                )
                .is_err());
            assert_eq!(model, before);
        }
    }
}

#[test]
fn zero_new_story_merges_and_unmerges_work_with_an_exhausted_clock() {
    use jade_docx_collab::intent::{try_resolve, IntentOp};
    let mut model = DocxModel::from_paragraphs(&["body"]);
    model.apply(&insert(1, 2), &ctx()).unwrap();
    let intent = IntentOp::MergeTableCells {
        table: 0,
        top_row: 0,
        left_column: 0,
        bottom_row: 0,
        right_column: 1,
    };
    let operations = try_resolve(&model, &intent, &mut |_| {
        Err(jade_collab_core::id::IdError::Exhausted)
    })
    .expect("horizontal merge needs no follower ids");
    for operation in operations {
        model.apply(&operation, &ctx()).unwrap();
    }
    let mut vertical = DocxModel::from_paragraphs(&["body"]);
    vertical.apply(&insert(2, 1), &ctx()).unwrap();
    let table = vertical.tables().remove(0);
    vertical
        .apply(
            &DocxOp::MergeCells {
                table: table.id,
                merge: Merge {
                    top: table.rows[0],
                    bottom: table.rows[1],
                    left: table.columns[0],
                    right: table.columns[0],
                },
                first: id(9_000_000),
            },
            &ctx(),
        )
        .unwrap();
    let operations = try_resolve(
        &vertical,
        &IntentOp::UnmergeTableCells {
            table: 0,
            row: 0,
            column: 0,
        },
        &mut |_| Err(jade_collab_core::id::IdError::Exhausted),
    )
    .expect("vertical unmerge frees no removed columns");
    for operation in operations {
        vertical.apply(&operation, &ctx()).unwrap();
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(96))]
    #[test]
    fn table_budgets_follow_atomic_rejection_and_incremental_growth_histories(
        history in proptest::collection::vec(0u8..7,1..70),
    ) {
        use proptest::prelude::*;
        let mut model=DocxModel::from_paragraphs(&["body"]);
        model.apply(&insert(2,3),&ctx()).unwrap();
        let table=model.tables().remove(0);
        let (mut rows,mut columns)=(table.rows,table.columns);
        let mut clock=model.max_lamport();
        for (step,kind) in history.into_iter().enumerate() {
            let fresh=id(10_000_000+step as u64*1000);
            let before=model.clone();
            match kind {
                0 => {model.apply(&DocxOp::Table {table:table.id,op:GridOp::InsertRow {id:fresh,after:rows.last().copied()}},&ctx()).unwrap();rows.push(fresh);clock=fresh.lamport;}
                1 => {model.apply(&DocxOp::Table {table:table.id,op:GridOp::InsertColumn {id:fresh,after:columns.last().copied()}},&ctx()).unwrap();columns.push(fresh);clock=fresh.lamport;}
                2 if rows.len()>1 => {model.apply(&DocxOp::Table {table:table.id,op:GridOp::DeleteRow {id:rows.remove(0)}},&ctx()).unwrap();}
                3 if columns.len()>1 => {model.apply(&DocxOp::Table {table:table.id,op:GridOp::DeleteColumn {id:columns.remove(0)}},&ctx()).unwrap();}
                4 => {
                    let operation=DocxOp::InsertTable {anchor:Anchor::Start,id:fresh,
                        rows:(1..=101).map(|i|fresh.offset(i)).collect(),columns:(102..202).map(|i|fresh.offset(i)).collect()};
                    prop_assert!(model.apply(&operation,&ctx()).is_err());prop_assert_eq!(&model,&before);
                }
                5 => {model=serde_json::from_slice(&serde_json::to_vec(&model).unwrap()).unwrap();prop_assert_eq!(&model,&before);}
                _ => {}
            }
            let view=model.tables().remove(0);
            prop_assert_eq!(view.rows.as_slice(),rows.as_slice());prop_assert_eq!(view.columns.as_slice(),columns.as_slice());
            prop_assert_eq!(model.max_lamport(),clock);
            prop_assert_eq!(model.paragraphs().len(),1+rows.len()*columns.len());
        }
    }
}
