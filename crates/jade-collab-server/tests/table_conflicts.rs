//! Two valid offline merges overlap: exactly the first commit survives.
use jade_collab_core::id::Id;
use jade_collab_server::{Client, Server};
use jade_collab_sync::{Document, ReplicaEvent};
use jade_docx_collab::{intent::IntentOp, Anchor, DocxModel, DocxOp, JadeDocx, StoryId};
use jade_xlsx_collab::{intent::XlsxIntent, JadeXlsx, XlsxModel};
use serde::{de::DeserializeOwned, Serialize};
use std::{fmt::Debug, path::Path, time::Duration};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(name),
    )
    .unwrap()
}
async fn listen(
    server: &Server,
    name: &str,
) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/{name}/ws",
        listener.local_addr().unwrap()
    );
    (url, tokio::spawn(server.clone().serve(listener)))
}

type Plan<D> = fn(&D, &mut dyn FnMut(u64) -> Id, bool) -> <D as Document>::Op;
async fn overlap<D>(server: &Server, name: &str, url: &str, first: bool, plan: Plan<D>) -> D
where
    D: Document + Serialize + DeserializeOwned + Send + PartialEq + Debug + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let a = Client::<D>::connect(url, None).await.unwrap();
    let b = Client::<D>::connect(url, None).await.unwrap();
    let mut offline = b.disconnect().await;
    let old = offline.view().clone();
    let op = plan(&old, &mut |n| offline.reserve(n), !first);
    offline.submit(op).unwrap(); // valid in the same old state
    assert_eq!(offline.pending_len(), 1);
    let before = server.seq(name).await.unwrap();
    a.submit(plan(&a.view(), &mut |n| a.reserve(n), first))
        .unwrap();
    a.settle().await.unwrap();
    let accepted = server.export(name).await.unwrap();
    assert_eq!(server.seq(name).await.unwrap(), before + 1);
    let b = Client::<D>::resume(url, &offline, None).await.unwrap();
    b.settle().await.unwrap();
    let events = b.take_events();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ReplicaEvent::Rejected { client_seq: 1, .. }))
            .count(),
        1
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, ReplicaEvent::Acknowledged { .. })));
    assert_eq!(b.pending_len(), 0);
    assert_eq!(
        b.view(),
        a.view(),
        "the refused optimistic merge must be rolled back"
    );
    assert_eq!(server.seq(name).await.unwrap(), before + 1);
    assert_eq!(
        server.export(name).await.unwrap(),
        accepted,
        "refusal cannot change the native file"
    );
    assert!(server.faults(name).await.unwrap().is_empty());
    let refused = b.disconnect().await;
    let b = Client::<D>::resume(url, &refused, None).await.unwrap();
    b.settle().await.unwrap();
    assert_eq!(
        b.view(),
        a.view(),
        "reconnecting cannot revive the refused merge"
    );
    assert_eq!(server.seq(name).await.unwrap(), before + 1);
    b.disconnect().await;
    a.disconnect().await.view().clone()
}

fn word_merge(model: &DocxModel, ids: &mut dyn FnMut(u64) -> Id, second: bool) -> DocxOp {
    let i = usize::from(second);
    let mut ops = jade_docx_collab::intent::resolve(
        model,
        &IntentOp::MergeTableCells {
            table: 0,
            top_row: i,
            left_column: i,
            bottom_row: i + 1,
            right_column: i + 1,
        },
        ids,
    )
    .unwrap();
    assert_eq!(ops.len(), 1);
    ops.remove(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn docx_overlapping_offline_merges_keep_only_the_first_in_both_orders() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for first in [false, true] {
            let server = Server::new();
            server.open("table.docx", fixture("hello.docx")).unwrap();
            let (url, task) = listen(&server, "table.docx").await;
            let seed = Client::<DocxModel>::connect(&url, None).await.unwrap();
            let mark = seed.reserve(1);
            seed.submit(DocxOp::SplitParagraph {
                story: StoryId::Body,
                parent: seed.view().paragraphs()[0].chars.last().copied(),
                id: mark,
            })
            .unwrap();
            let id = seed.reserve(7);
            seed.submit(DocxOp::InsertTable {
                anchor: Anchor::After(mark),
                id,
                rows: (1..=3).map(|i| id.offset(i)).collect(),
                columns: (4..=6).map(|i| id.offset(i)).collect(),
            })
            .unwrap();
            seed.settle().await.unwrap();
            seed.disconnect().await;
            let model = overlap(&server, "table.docx", &url, first, word_merge).await;
            let i = usize::from(first);
            assert_eq!(model.tables()[0].merges, vec![(i, i, i + 1, i + 1)]);
            let native = JadeDocx::load(&server.export("table.docx").await.unwrap()).unwrap();
            assert_eq!(
                DocxModel::from_word_document(native.word())
                    .unwrap()
                    .tables()[0]
                    .merges,
                model.tables()[0].merges
            );
            task.abort();
            let _ = task.await;
        }
    })
    .await
    .expect("table conflict deadline");
}

fn sheet_merge(
    model: &XlsxModel,
    ids: &mut dyn FnMut(u64) -> Id,
    second: bool,
) -> jade_xlsx_collab::XlsxOp {
    jade_xlsx_collab::intent::resolve(
        model,
        &XlsxIntent::MergeCells {
            sheet: "Operations".into(),
            // Outside the fixture's structured Excel table, where merging is forbidden.
            range: if second { "I21:J22" } else { "H20:I21" }.into(),
        },
        ids,
    )
    .unwrap()
}
fn regions(model: &XlsxModel) -> Vec<(usize, usize, usize, usize)> {
    let sheet = model
        .sheets()
        .into_iter()
        .find(|s| s.name == "Operations")
        .unwrap();
    model
        .merges(sheet.id)
        .iter()
        .map(|m| {
            (
                sheet.rows.iter().position(|r| *r == m.top).unwrap(),
                sheet.columns.iter().position(|c| *c == m.left).unwrap(),
                sheet.rows.iter().position(|r| *r == m.bottom).unwrap(),
                sheet.columns.iter().position(|c| *c == m.right).unwrap(),
            )
        })
        .collect()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xlsx_overlapping_offline_merges_keep_only_the_first_in_both_orders() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for first in [false, true] {
            let server = Server::new();
            let bytes = fixture("public/fixtures/grid-operations-workbook.xlsx");
            let initial = JadeXlsx::open(&bytes).unwrap();
            let mut expected =
                regions(&XlsxModel::from_workbook(initial.workbook(), Default::default()).unwrap());
            server.open("table.xlsx", bytes).unwrap();
            let (url, task) = listen(&server, "table.xlsx").await;
            let model = overlap(&server, "table.xlsx", &url, first, sheet_merge).await;
            let i = usize::from(first);
            expected.push((i + 19, i + 7, i + 20, i + 8));
            assert_eq!(
                regions(&model),
                expected,
                "original merges and exactly the winning new merge survive"
            );
            let native = JadeXlsx::open(&server.export("table.xlsx").await.unwrap()).unwrap();
            let imported = XlsxModel::from_workbook(native.workbook(), Default::default()).unwrap();
            assert_eq!(regions(&imported), regions(&model));
            task.abort();
            let _ = task.await;
        }
    })
    .await
    .expect("table conflict deadline");
}
