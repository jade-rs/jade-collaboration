//! Agents joining, reading and editing hosted documents through the MCP
//! tools, next to replicas that do not use MCP.

mod common;

use common::{statuses, texts, Agent, Hosted, DOCX, XLSX};
use jade_collab_mcp::{read_docx, read_xlsx};
use jade_docx_collab::{Anchor, DocxModel, DocxOp, JadeDocx, StoryId};
use jade_xlsx_collab::{CellInput, XlsxModel};
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread")]
async fn list_documents_names_every_hosted_document_and_its_socket() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    for server in [hosted.http.clone(), format!("{}/", hosted.http)] {
        let listed = agent
            .tool("list_documents", json!({ "server": server }))
            .await;
        assert_eq!(
            listed["documents"],
            json!([
                { "name": XLSX, "format": "xlsx", "url": hosted.url(XLSX) },
                { "name": DOCX, "format": "docx", "url": hosted.url(DOCX) },
            ])
        );
    }
    // Nothing listens on port 1: the tool ran and failed.
    let message = agent
        .tool_failure("list_documents", json!({ "server": "http://127.0.0.1:1" }))
        .await;
    assert!(!message.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_joins_reads_edits_and_leaves_a_docx() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let joined = agent
        .tool("join_document", json!({ "url": hosted.url(DOCX) }))
        .await;
    assert_eq!(joined["format"], json!("docx"));
    assert_eq!(joined["document"], json!(DOCX));
    assert!(joined["replica"].as_u64().unwrap() > 0);
    assert_eq!(joined["seq"], json!(0));
    let session = joined["session_id"].as_str().unwrap().to_string();

    let read = agent.read_all(&session).await;
    assert_eq!(read["format"], json!("docx"));
    assert_eq!(read["seq"], json!(0));
    assert_eq!(read["pending"], json!(0));
    assert_eq!(read["paragraph_count"], json!(2));
    assert_eq!(read["start"], json!(0));
    assert_eq!(read["next"], Value::Null);
    assert_eq!(read["tables"], json!([]));
    let first = read["paragraphs"][0].clone();
    assert_eq!(first["index"], json!(0));
    assert_eq!(first["story"], json!({ "kind": "body" }));
    let hello = first["text"].as_str().unwrap().to_string();
    assert!(hello.starts_with("Hello"), "{hello}");
    assert_eq!(read["paragraphs"][1]["text"], json!("World"));

    // Each edit is resolved against the view the previous ones left: the
    // break makes a new paragraph 1, which the third edit then fills.
    let applied = agent
        .apply(
            &session,
            json!([
                { "type": "insert_text", "paragraph": 0, "offset": 0, "text": "AI: " },
                { "type": "insert_paragraph_break", "paragraph": 0, "offset": hello.len() + 4 },
                { "type": "insert_text", "paragraph": 1, "offset": 0, "text": "new" },
                { "type": "insert_text", "paragraph": 42, "offset": 0, "text": "lost" },
                { "type": "set_cell", "sheet": "Operations", "reference": "A1", "value": { "kind": "blank" } },
                { "type": "delete_text", "paragraph": 2, "offset": 5, "direction": "backward", "count": 2 },
            ]),
        )
        .await;
    assert_eq!(
        statuses(&applied),
        ["applied", "applied", "applied", "invalid", "invalid", "applied"],
        "{applied}"
    );
    let results = applied["results"].as_array().unwrap();
    for (i, result) in results.iter().enumerate() {
        assert_eq!(result["index"], json!(i));
    }
    assert!(results[3]["error"]
        .as_str()
        .unwrap()
        .contains("paragraph 42"));
    assert!(!results[4]["error"].as_str().unwrap().is_empty());
    assert_eq!(applied["pending"], json!(0));
    let seq = applied["seq"].as_u64().unwrap();
    assert!(seq >= 4, "{applied}");

    let read = agent.read_all(&session).await;
    let expected = [format!("AI: {hello}"), "new".into(), "Wor".into()];
    assert_eq!(texts(&read), expected);
    assert_eq!(read["seq"], json!(seq));

    // Everyone else sees the same document, and so does the file.
    let direct = hosted.direct::<DocxModel>(DOCX).await;
    assert_eq!(direct.view().paragraph_texts(), expected);
    let exported = JadeDocx::load(&hosted.server.export(DOCX).await.unwrap()).unwrap();
    assert_eq!(exported.body_paragraph_texts(), expected);
    assert!(hosted.server.faults(DOCX).await.unwrap().is_empty());

    let left = agent
        .tool("leave_document", json!({ "session_id": session }))
        .await;
    assert_eq!(left["session_id"], json!(session));
    assert_eq!(left["left"], json!(true));
    let (code, _) = agent
        .rpc_error("read_document", json!({ "session_id": session }))
        .await;
    assert_eq!(code, -32602);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_joins_reads_edits_and_leaves_an_xlsx() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let joined = agent
        .tool("join_document", json!({ "url": hosted.url(XLSX) }))
        .await;
    assert_eq!(joined["format"], json!("xlsx"));
    let session = joined["session_id"].as_str().unwrap().to_string();

    let read = agent.read_all(&session).await;
    assert_eq!(read["format"], json!("xlsx"));
    assert_eq!(read["pending"], json!(0));
    let sheet = &read["sheets"][0];
    assert_eq!(sheet["name"], json!("Operations"));
    assert!(sheet["rows"].as_u64().unwrap() >= 4);
    assert!(sheet["columns"].as_u64().unwrap() >= 6);
    assert_eq!(
        sheet["cells"]["A1"],
        json!({ "kind": "text", "value": "Name" })
    );
    assert_eq!(
        sheet["cells"]["B2"],
        json!({ "kind": "number", "value": 10.0 })
    );
    assert_eq!(
        sheet["cells"]["C2"],
        json!({ "kind": "formula", "value": "B2*2" })
    );
    assert_eq!(
        read["cell_count"],
        json!(sheet["cells"].as_object().unwrap().len())
    );

    // The row inserted by the second edit moves the first edit's value
    // down; the third edit addresses the grid as it is then.
    let applied = agent
        .apply(
            &session,
            json!([
                { "type": "set_cell", "sheet": "Operations", "reference": "E6", "value": { "kind": "number", "value": 42 } },
                { "type": "insert_row", "sheet": "Operations", "before": 6 },
                { "type": "set_cell", "sheet": "Operations", "reference": "E6", "value": { "kind": "text", "value": "x" } },
                { "type": "set_cell", "sheet": "Nope", "reference": "A1", "value": { "kind": "blank" } },
                { "type": "insert_text", "paragraph": 0, "offset": 0, "text": "docx only" },
            ]),
        )
        .await;
    assert_eq!(
        statuses(&applied),
        ["applied", "applied", "applied", "invalid", "invalid"],
        "{applied}"
    );
    assert!(applied["results"][3]["error"]
        .as_str()
        .unwrap()
        .contains("Nope"));

    let read = agent.read_all(&session).await;
    let cells = &read["sheets"][0]["cells"];
    assert_eq!(cells["E6"], json!({ "kind": "text", "value": "x" }));
    assert_eq!(cells["E7"], json!({ "kind": "number", "value": 42.0 }));

    let direct = hosted.direct::<XlsxModel>(XLSX).await;
    let mut expected = read_xlsx(&direct.view(), 0, usize::MAX);
    expected["seq"] = read["seq"].clone();
    expected["pending"] = json!(0);
    assert_eq!(common::without_snapshot(read), expected);
    assert!(hosted.server.faults(XLSX).await.unwrap().is_empty());

    agent
        .tool("leave_document", json!({ "session_id": session }))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_the_document_refuses_are_reported_as_rejected() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();

    // Writing a table's header renames its column; its header row cannot be
    // deleted.
    let book = agent.join(&hosted.url(XLSX)).await;
    let applied = agent
        .apply(
            &book,
            json!([
                { "type": "set_cell", "sheet": "Operations", "reference": "A1", "value": { "kind": "text", "value": "Who" } },
                { "type": "delete_row", "sheet": "Operations", "row": 1 },
                { "type": "set_cell", "sheet": "Operations", "reference": "B3", "value": { "kind": "number", "value": 7 } },
            ]),
        )
        .await;
    assert_eq!(
        statuses(&applied),
        ["applied", "rejected", "applied"],
        "{applied}"
    );
    assert!(applied["results"][1]["reason"]
        .as_str()
        .unwrap()
        .contains("header"));
    let read = agent.read_all(&book).await;
    assert_eq!(
        read["sheets"][0]["cells"]["A1"],
        json!({ "kind": "text", "value": "Who" })
    );

    // Someone else puts a table after the first paragraph; joining the
    // paragraphs around it is refused.
    let person = hosted.direct::<DocxModel>(DOCX).await;
    let end = person.view().paragraphs()[0].chars.last().copied();
    let mark = person.reserve(1);
    person
        .submit(DocxOp::SplitParagraph {
            story: StoryId::Body,
            parent: end,
            id: mark,
        })
        .unwrap();
    let first = person.reserve(3);
    person
        .submit(DocxOp::InsertTable {
            anchor: Anchor::After(mark),
            id: first,
            rows: vec![first.offset(1)],
            columns: vec![first.offset(2)],
        })
        .unwrap();
    person.settle().await.unwrap();

    let doc = agent.join(&hosted.url(DOCX)).await;
    let read = agent.read_all(&doc).await;
    assert_eq!(
        read["tables"],
        json!([{ "index": 0, "rows": 1, "columns": 1, "locked": false, "merges": [] }])
    );
    let stories: Vec<Value> = read["paragraphs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["story"].clone())
        .collect();
    assert_eq!(
        stories,
        [
            json!({ "kind": "body" }),
            json!({ "kind": "table_cell", "table": 0, "row": 0, "column": 0 }),
            json!({ "kind": "body" }),
            json!({ "kind": "body" }),
        ]
    );
    let applied = agent
        .apply(
            &doc,
            json!([
                { "type": "delete_text", "paragraph": 2, "offset": 0, "direction": "backward" },
                { "type": "insert_text", "paragraph": 1, "offset": 0, "text": "cell" },
            ]),
        )
        .await;
    assert_eq!(statuses(&applied), ["rejected", "applied"], "{applied}");
    assert!(applied["results"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("table"));
    let read = agent.read_all(&doc).await;
    assert_eq!(read["paragraph_count"], json!(4));
    assert_eq!(read["paragraphs"][1]["text"], json!("cell"));
}

#[tokio::test(flavor = "multi_thread")]
async fn read_document_pages_through_large_documents() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let doc = agent.join(&hosted.url(DOCX)).await;
    let applied = agent
        .apply(
            &doc,
            json!([{ "type": "insert_paragraph_break", "paragraph": 1, "offset": 0, "count": 10 }]),
        )
        .await;
    assert_eq!(statuses(&applied), ["applied"]);
    let all = agent.read_all(&doc).await;
    assert_eq!(all["paragraph_count"], json!(12));

    let mut paged = Vec::new();
    let mut start = Some(0u64);
    while let Some(at) = start {
        let page = agent
            .tool(
                "read_document",
                json!({ "session_id": doc, "start": at, "limit": 5, "snapshot": all["snapshot"] }),
            )
            .await;
        assert_eq!(page["start"], json!(at));
        assert_eq!(page["paragraph_count"], json!(12));
        assert_eq!(page["tables"], all["tables"]);
        let paragraphs = page["paragraphs"].as_array().unwrap();
        assert!(paragraphs.len() <= 5);
        paged.extend(paragraphs.iter().cloned());
        start = page["next"].as_u64();
    }
    assert_eq!(Value::Array(paged), all["paragraphs"]);
    let beyond = agent
        .tool("read_document", json!({ "session_id": doc, "start": 50 }))
        .await;
    assert_eq!(beyond["paragraphs"], json!([]));
    assert_eq!(beyond["next"], Value::Null);

    let book = agent.join(&hosted.url(XLSX)).await;
    let applied = agent.apply(&book, json!([
        { "type":"set_format", "sheet":"Operations", "range":"A20:A30", "change":{"bold":true} },
        { "type":"set_format", "sheet":"Operations", "range":"35:40", "change":{"italic":true} },
        { "type":"set_format", "sheet":"Operations", "range":"G:H", "change":{"wrap_text":true} },
        { "type":"merge_cells", "sheet":"Operations", "range":"J25:K27" },
        { "type":"set_note", "sheet":"Operations", "reference":"L31", "text":"Check 日本🙂" },
    ])).await;
    assert_eq!(statuses(&applied), ["applied"; 5]);
    let all = agent.read_all(&book).await;
    let total = all["cell_count"].as_u64().unwrap();
    assert!(total > 4);
    let mut cells = serde_json::Map::new();
    let mut formats = serde_json::Map::new();
    let mut notes = serde_json::Map::new();
    let mut row_formats = serde_json::Map::new();
    let mut column_formats = serde_json::Map::new();
    let mut merges = Vec::new();
    let mut start = Some(0u64);
    while let Some(at) = start {
        let page = agent
            .tool(
                "read_document",
                json!({ "session_id": book, "start": at, "limit": 3, "snapshot": all["snapshot"] }),
            )
            .await;
        assert_eq!(page["cell_count"], json!(total));
        assert_eq!(page["seq"], all["seq"], "this walk has no concurrent edits");
        let listed = page["sheets"][0]["cells"].as_object().unwrap();
        let sheet = &page["sheets"][0];
        let cell_items: std::collections::BTreeSet<_> = listed
            .keys()
            .chain(sheet["formats"].as_object().unwrap().keys())
            .collect();
        assert!(
            cell_items.len()
                + sheet["row_formats"].as_object().unwrap().len()
                + sheet["column_formats"].as_object().unwrap().len()
                + sheet["merges"].as_array().unwrap().len()
                <= 3
        );
        cells.extend(listed.clone());
        formats.extend(sheet["formats"].as_object().unwrap().clone());
        notes.extend(sheet["notes"].as_object().unwrap().clone());
        row_formats.extend(sheet["row_formats"].as_object().unwrap().clone());
        column_formats.extend(sheet["column_formats"].as_object().unwrap().clone());
        merges.extend(sheet["merges"].as_array().unwrap().iter().cloned());
        start = page["next"].as_u64();
    }
    assert_eq!(Value::Object(cells), all["sheets"][0]["cells"]);
    assert_eq!(Value::Object(formats), all["sheets"][0]["formats"]);
    assert_eq!(Value::Object(notes), all["sheets"][0]["notes"]);
    assert_eq!(Value::Object(row_formats), all["sheets"][0]["row_formats"]);
    assert_eq!(
        Value::Object(column_formats),
        all["sheets"][0]["column_formats"]
    );
    assert_eq!(Value::Array(merges), all["sheets"][0]["merges"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_for_changes_sees_other_replicas_edits() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let book = agent.join(&hosted.url(XLSX)).await;

    let started = std::time::Instant::now();
    let quiet = agent
        .tool(
            "wait_for_changes",
            json!({ "session_id": book, "after_seq": 0, "timeout_ms": 50 }),
        )
        .await;
    assert_eq!(quiet, json!({ "changed": false, "seq": 0 }));
    assert!(started.elapsed() >= std::time::Duration::from_millis(50));

    let person = hosted.direct::<XlsxModel>(XLSX).await;
    let (sheet, row, column) = person.view().locate("Operations", "F8").unwrap();
    person
        .submit(jade_xlsx_collab::XlsxOp::SetCell {
            sheet,
            row,
            column,
            stamp: person.reserve(1),
            value: CellInput::Text("from a person".into()),
        })
        .unwrap();
    let waited = agent
        .tool(
            "wait_for_changes",
            json!({ "session_id": book, "after_seq": 0, "timeout_ms": 10_000 }),
        )
        .await;
    assert_eq!(waited, json!({ "changed": true, "seq": 1 }));
    let read = agent.read_all(&book).await;
    assert_eq!(
        read["sheets"][0]["cells"]["F8"],
        json!({ "kind": "text", "value": "from a person" })
    );
}

/// Two agents — two MCP servers, as two processes would be — edit the same
/// documents at the same time and end up reading the same thing, which is
/// also what a replica outside MCP reads.
#[tokio::test(flavor = "multi_thread")]
async fn two_agents_editing_concurrently_converge() {
    let hosted = Hosted::start().await;
    let mut alice = Agent::new();
    let mut bob = Agent::new();

    let a = alice.join(&hosted.url(DOCX)).await;
    let b = bob.join(&hosted.url(DOCX)).await;
    let (from_alice, from_bob) = tokio::join!(
        alice.apply(
            &a,
            json!([
                { "type": "insert_text", "paragraph": 0, "offset": 0, "text": "Alice: " },
                { "type": "insert_text", "paragraph": 1, "offset": 5, "text": "!" },
                { "type": "insert_paragraph_break", "paragraph": 1, "offset": 0 },
            ]),
        ),
        bob.apply(
            &b,
            json!([
                { "type": "insert_text", "paragraph": 0, "offset": 0, "text": "Bob: " },
                { "type": "delete_text", "paragraph": 1, "offset": 0, "direction": "forward", "count": 1 },
                { "type": "insert_text", "paragraph": 1, "offset": 0, "text": "w" },
            ]),
        ),
    );
    assert_eq!(statuses(&from_alice), ["applied"; 3]);
    assert_eq!(statuses(&from_bob), ["applied"; 3]);
    let seq = hosted.server.seq(DOCX).await.unwrap();
    alice.catch_up(&a, seq).await;
    bob.catch_up(&b, seq).await;
    let read = alice.read_all(&a).await;
    assert_eq!(
        common::without_snapshot(read.clone()),
        common::without_snapshot(bob.read_all(&b).await)
    );
    let joined = texts(&read).concat();
    for piece in ["Alice: ", "Bob: ", "world!"] {
        assert!(joined.contains(piece), "{piece} in {joined:?}");
    }
    let direct = hosted.direct::<DocxModel>(DOCX).await;
    let mut expected = read_docx(&direct.view(), 0, usize::MAX);
    expected["seq"] = json!(seq);
    expected["pending"] = json!(0);
    assert_eq!(common::without_snapshot(read), expected);

    let a = alice.join(&hosted.url(XLSX)).await;
    let b = bob.join(&hosted.url(XLSX)).await;
    let set = |reference: &str, value: Value| json!({ "type": "set_cell", "sheet": "Operations", "reference": reference, "value": value });
    let (from_alice, from_bob) = tokio::join!(
        alice.apply(
            &a,
            json!([
                set("B2", json!({ "kind": "number", "value": 1 })),
                set("E5", json!({ "kind": "text", "value": "a" }))
            ]),
        ),
        bob.apply(
            &b,
            json!([
                set("B2", json!({ "kind": "number", "value": 2 })),
                set("F5", json!({ "kind": "text", "value": "b" }))
            ]),
        ),
    );
    assert_eq!(statuses(&from_alice), ["applied"; 2]);
    assert_eq!(statuses(&from_bob), ["applied"; 2]);
    let seq = hosted.server.seq(XLSX).await.unwrap();
    alice.catch_up(&a, seq).await;
    bob.catch_up(&b, seq).await;
    let read = alice.read_all(&a).await;
    assert_eq!(
        common::without_snapshot(read.clone()),
        common::without_snapshot(bob.read_all(&b).await)
    );
    let cells = &read["sheets"][0]["cells"];
    assert_eq!(cells["E5"], json!({ "kind": "text", "value": "a" }));
    assert_eq!(cells["F5"], json!({ "kind": "text", "value": "b" }));
    assert!(
        [json!(1.0), json!(2.0)].contains(&cells["B2"]["value"]),
        "{cells}"
    );
    assert!(hosted.server.faults(DOCX).await.unwrap().is_empty());
    assert!(hosted.server.faults(XLSX).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn joining_a_missing_document_fails_as_a_tool_error() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let message = agent
        .tool_failure(
            "join_document",
            json!({ "url": hosted.url("missing.docx") }),
        )
        .await;
    assert!(!message.is_empty());
    // A failed join leaves no session behind.
    let session = agent.join(&hosted.url(DOCX)).await;
    assert_eq!(agent.read_all(&session).await["format"], json!("docx"));
}

/// An agent reviews with Track Changes: it switches tracking on, its edits
/// become revisions everyone sees, a person rejects one and the agent
/// accepts the rest.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_edits_with_track_changes_and_revisions_are_decided() {
    let hosted = Hosted::start().await;
    let mut agent = Agent::new();
    let doc = agent.join(&hosted.url(DOCX)).await;
    let applied = agent
        .apply(
            &doc,
            json!([
                { "type": "set_tracking", "on": true },
                { "type": "insert_text", "paragraph": 1, "offset": 5, "text": "!" },
                { "type": "delete_text", "paragraph": 1, "offset": 0, "direction": "forward", "count": 1 },
            ]),
        )
        .await;
    assert_eq!(
        statuses(&applied),
        ["applied", "applied", "applied"],
        "{applied}"
    );
    let read = agent.read_all(&doc).await;
    assert_eq!(read["tracking"], json!(true));
    // The deleted "W" is still in the text, marked.
    assert_eq!(read["paragraphs"][1]["text"], json!("World!"));
    let revisions = read["revisions"].as_array().unwrap().clone();
    let kinds: Vec<&str> = revisions
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["insertion", "deletion"]);

    // A person (outside MCP) rejects the deletion.
    let person = hosted.direct::<DocxModel>(DOCX).await;
    let deletion = person.view().revisions()[1].id;
    person
        .submit(DocxOp::RejectRevisions {
            revisions: vec![deletion],
        })
        .unwrap();
    person.settle().await.unwrap();
    hosted.catch_up(DOCX, &person).await;

    let applied = agent
        .apply(&doc, json!([{ "type": "accept_revisions" }]))
        .await;
    assert_eq!(statuses(&applied), ["applied"], "{applied}");
    let read = agent.read_all(&doc).await;
    assert_eq!(read["revisions"], json!([]));
    assert_eq!(read["paragraphs"][1]["text"], json!("World!"));
    let exported = JadeDocx::load(&hosted.server.export(DOCX).await.unwrap()).unwrap();
    assert_eq!(exported.body_paragraph_texts()[1], "World!");
    let reloaded = DocxModel::from_word_document(exported.word()).unwrap();
    assert!(reloaded.revisions().is_empty());
    assert!(hosted.server.faults(DOCX).await.unwrap().is_empty());
}
