//! Real sockets: a server with a .docx and a .xlsx, and clients editing
//! them concurrently — one typing like a person, one sending index-addressed
//! edits like an agent.

use std::path::PathBuf;
use std::time::Duration;

use jade_collab_core::grid::GridOp;
use jade_collab_server::{Client, Server};
use jade_collab_sync::ReplicaEvent;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::{Anchor, DocxModel, DocxOp, JadeDocx, StoryId};
use jade_xlsx_collab::{CellInput, JadeXlsx, XlsxModel, XlsxOp};

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}

async fn start() -> (Server, String) {
    let server = Server::new();
    server.open("hello.docx", fixture("hello.docx")).unwrap();
    server
        .open(
            "book.xlsx",
            fixture("public/fixtures/grid-operations-workbook.xlsx"),
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.clone().serve(listener));
    (server, format!("ws://{addr}/documents"))
}

/// Waits until `client` has caught up with the server's log.
async fn catch_up<D>(server: &Server, name: &str, client: &Client<D>)
where
    D: jade_collab_sync::Document + serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    D::Op: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let seq = server.seq(name).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), client.wait_for_seq(seq))
        .await
        .expect("caught up in time")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_and_an_agent_edit_a_docx_together() {
    let (server, base) = start().await;
    let url = format!("{base}/hello.docx/ws");
    let person: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let agent: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    assert_ne!(person.id(), agent.id());
    assert_eq!(person.view(), agent.view());
    let original = person.view().paragraphs()[0].text.clone();

    // The person types at the end of the first paragraph, one key at a time.
    for ch in [" ", "w", "o", "r", "l", "d"] {
        let view = person.view();
        let paragraph = &view.paragraphs()[0];
        let op = DocxOp::InsertText {
            story: StoryId::Body,
            parent: paragraph.chars.last().copied(),
            first: person.reserve(1),
            text: ch.to_string(),
        };
        person.submit(op).unwrap();
    }
    // Meanwhile the agent, from its own (older) view, prefixes the paragraph
    // and adds a paragraph with a table after it.
    let view = agent.view();
    for op in resolve(
        &view,
        &IntentOp::InsertText {
            paragraph: 0,
            offset: 0,
            text: "[AI] ".into(),
        },
        &mut |n| agent.reserve(n),
    )
    .unwrap()
    {
        agent.submit(op).unwrap();
    }
    let end = view.paragraphs()[0].chars.last().copied();
    let mark = agent.reserve(1);
    agent
        .submit(DocxOp::SplitParagraph {
            story: StoryId::Body,
            parent: end,
            id: mark,
        })
        .unwrap();
    let first = agent.reserve(3);
    agent
        .submit(DocxOp::InsertTable {
            anchor: Anchor::After(mark),
            id: first,
            rows: vec![first.offset(1)],
            columns: vec![first.offset(2)],
        })
        .unwrap();

    person.settle().await.unwrap();
    agent.settle().await.unwrap();
    catch_up(&server, "hello.docx", &person).await;
    catch_up(&server, "hello.docx", &agent).await;

    let view = person.view();
    assert_eq!(view, agent.view());
    // Both edits survive. The person's " world" and the agent's paragraph
    // break went in at the same place concurrently; which comes first is
    // decided by their ids, identically everywhere.
    let texts = view.paragraph_texts();
    assert!(
        texts[0].starts_with(&format!("[AI] {original}")),
        "{texts:?}"
    );
    assert!(
        texts.concat().contains(&format!("{original} world")),
        "{texts:?}"
    );
    assert_eq!(view.tables().len(), 1);

    // The file the server writes is the document both of them see.
    let exported = JadeDocx::load(&server.export("hello.docx").await.unwrap()).unwrap();
    assert_eq!(exported.body_paragraph_texts(), view.paragraph_texts());
    assert!(server.faults("hello.docx").await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_structural_edit_comes_back_to_its_author() {
    let (server, base) = start().await;
    let url = format!("{base}/hello.docx/ws");
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&url, None).await.unwrap();

    // A splits the first paragraph and the split reaches everyone.
    let end = a.view().paragraphs()[0].chars.last().copied();
    let mark = a.reserve(1);
    a.submit(DocxOp::SplitParagraph {
        story: StoryId::Body,
        parent: end,
        id: mark,
    })
    .unwrap();
    a.settle().await.unwrap();
    catch_up(&server, "hello.docx", &b).await;

    // B goes offline and joins the paragraph with the next one…
    let mut offline = b.disconnect().await;
    offline
        .submit(DocxOp::JoinParagraph {
            story: StoryId::Body,
            mark,
        })
        .unwrap();
    // …while A puts a table right after that paragraph.
    let first = a.reserve(3);
    a.submit(DocxOp::InsertTable {
        anchor: Anchor::After(mark),
        id: first,
        rows: vec![first.offset(1)],
        columns: vec![first.offset(2)],
    })
    .unwrap();
    a.settle().await.unwrap();

    // Back online, B's join reaches the server after the table: refused.
    let b: Client<DocxModel> = Client::resume(&url, offline, None).await.unwrap();
    b.settle().await.unwrap();
    let events = b.take_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ReplicaEvent::Rejected { .. })),
        "{events:?}"
    );
    catch_up(&server, "hello.docx", &a).await;
    catch_up(&server, "hello.docx", &b).await;
    assert_eq!(a.view(), b.view());
    assert_eq!(b.view().tables().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn spreadsheet_cells_and_rows_converge() {
    let (server, base) = start().await;
    let url = format!("{base}/book.xlsx/ws");
    let a: Client<XlsxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<XlsxModel> = Client::connect(&url, None).await.unwrap();

    let view = a.view();
    let (sheet, row6, e) = view.locate("Operations", "E6").unwrap();
    let (_, row5, _) = view.locate("Operations", "E5").unwrap();
    // Both write E6 at once: the later stamp wins everywhere.
    a.submit(XlsxOp::SetCell {
        sheet,
        row: row6,
        column: e,
        stamp: a.reserve(1),
        value: CellInput::Number(1.0),
    })
    .unwrap();
    b.submit(XlsxOp::SetCell {
        sheet,
        row: row6,
        column: e,
        stamp: b.reserve(1),
        value: CellInput::Text("b".into()),
    })
    .unwrap();
    // A inserts a row above; B writes a formula against the old grid.
    a.submit(XlsxOp::Structure {
        sheet,
        op: GridOp::InsertRow {
            id: a.reserve(1),
            after: Some(row5),
        },
    })
    .unwrap();
    let (_, row7, d) = view.locate("Operations", "D7").unwrap();
    b.submit(XlsxOp::SetCell {
        sheet,
        row: row7,
        column: d,
        stamp: b.reserve(1),
        value: CellInput::Formula("E6*2".into()),
    })
    .unwrap();

    a.settle().await.unwrap();
    b.settle().await.unwrap();
    catch_up(&server, "book.xlsx", &a).await;
    catch_up(&server, "book.xlsx", &b).await;
    let view = a.view();
    assert_eq!(view, b.view());
    // Whether or not B had seen the inserted row when it typed, the formula
    // points at the cell both of them wrote (now E7), and the file agrees.
    // (Which interleaving happened depends on the network; the model and
    // simulation tests pin each one down.)
    let formula = view.value(sheet, row7, d).cloned();
    assert!(
        matches!(&formula, Some(CellInput::Formula(f)) if f == "E7*2" || f == "E6*2"),
        "{formula:?}"
    );
    let sheet_view = &view.sheets()[0];
    let r = sheet_view.rows.iter().position(|r| *r == row7).unwrap();
    let exported = JadeXlsx::open(&server.export("book.xlsx").await.unwrap()).unwrap();
    let cell = exported
        .workbook()
        .cell("Operations", &format!("D{}", r + 1))
        .unwrap();
    assert_eq!(cell.formula.map(CellInput::Formula), formula);
    assert!(server.faults("book.xlsx").await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_drops_resumes_where_it_left_off() {
    let (server, base) = start().await;
    let url = format!("{base}/hello.docx/ws");
    let a: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let b: Client<DocxModel> = Client::connect(&url, None).await.unwrap();
    let replica = a.disconnect().await;

    // While A is away, B edits.
    let parent = b.view().paragraphs()[0].chars.first().copied();
    b.submit(DocxOp::InsertText {
        story: StoryId::Body,
        parent,
        first: b.reserve(2),
        text: "zz".into(),
    })
    .unwrap();
    b.settle().await.unwrap();

    let a = Client::resume(&url, replica, None).await.unwrap();
    catch_up(&server, "hello.docx", &a).await;
    assert_eq!(a.view(), b.view());
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_documents_are_refused() {
    let (_server, base) = start().await;
    let result: Result<Client<DocxModel>, _> =
        Client::connect(&format!("{base}/missing.docx/ws"), None).await;
    assert!(result.is_err());
}
