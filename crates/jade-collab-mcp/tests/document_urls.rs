mod common;

use common::{fixture, statuses, Agent, Hosted};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn advertised_urls_join_the_original_names_and_edit_all_office_formats() {
    let hosted = Hosted::start().await;
    let cases = [
        (
            "日本 #100%?+.docx",
            "%E6%97%A5%E6%9C%AC%20%23100%25%3F%2B.docx",
            "hello.docx",
            json!({"type":"insert_text","paragraph":1,"offset":0,"text":"x"}),
        ),
        (
            "Budget [2026]&税.xlsx",
            "Budget%20%5B2026%5D%26%E7%A8%8E.xlsx",
            "public/fixtures/grid-operations-workbook.xlsx",
            json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"number","value":7}}),
        ),
        (
            "Deck #1%.pptx",
            "Deck%20%231%25.pptx",
            "public/fixtures/sample-deck.pptx",
            json!({"type":"set_slide_hidden","slide":1,"hidden":true}),
        ),
        (
            "literal%2F.docx",
            "literal%252F.docx",
            "hello.docx",
            json!({"type":"insert_text","paragraph":1,"offset":0,"text":"x"}),
        ),
    ];
    for (name, _, source, _) in &cases {
        hosted.server.open(name, fixture(source)).unwrap();
    }
    let mut agent = Agent::new();
    let listed = agent
        .tool("list_documents", json!({"server":hosted.http}))
        .await;
    for (name, encoded, _, edit) in cases {
        let url = listed["documents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == name)
            .unwrap()["url"]
            .as_str()
            .unwrap();
        let expected = format!(
            "{}/documents/{encoded}/ws",
            hosted.http.replacen("http://", "ws://", 1)
        );
        assert_eq!(url, expected);
        assert!(url.is_ascii());
        let joined = agent.tool("join_document", json!({"url":url})).await;
        assert_eq!(
            joined["document"], name,
            "decode one path segment exactly once"
        );
        let session = joined["session_id"].as_str().unwrap();
        assert_eq!(
            statuses(&agent.apply(session, json!([edit])).await),
            ["applied"]
        );
        assert_eq!(hosted.server.seq(name).await.unwrap(), 1);
        assert!(hosted.server.faults(name).await.unwrap().is_empty());
        agent
            .tool("leave_document", json!({"session_id":session}))
            .await;
    }
}

#[tokio::test]
async fn query_authentication_does_not_become_part_of_the_name_or_format() {
    use jade_collab_server::{Server, Tokens};
    let server = Server::new().with_tokens(
        Tokens::from_json(r#"{"query-secret":{"name":"editor","role":"editor"}}"#).unwrap(),
    );
    let name = "日本 #?.docx";
    server.open(name, fixture("hello.docx")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/%E6%97%A5%E6%9C%AC%20%23%3F.docx/ws?token=query-secret&next=/x.txt",
        listener.local_addr().unwrap()
    );
    let serving = tokio::spawn(server.clone().serve(listener));
    let mut agent = Agent::new();
    let joined = agent.tool("join_document", json!({"url":url})).await;
    assert_eq!(joined["document"], name);
    assert_eq!(joined["format"], "docx");
    let session = joined["session_id"].as_str().unwrap();
    assert_eq!(
        statuses(
            &agent
                .apply(
                    session,
                    json!([{"type":"insert_text","paragraph":1,"offset":0,"text":"x"}])
                )
                .await
        ),
        ["applied"]
    );
    agent
        .tool("leave_document", json!({"session_id":session}))
        .await;
    serving.abort();
    let _ = serving.await;
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn opaque_names_keep_independent_edits_through_join_leave_and_durable_restart(
        source in 0usize..3,
        steps in proptest::collection::vec((0u8..5,0usize..4),1..18),
    ) {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use jade_collab_server::{Server,StorageOptions,document_name_from_url};
            use std::collections::BTreeMap;
            let dir = tempfile::tempdir().unwrap();
            let (ext,path) = [("docx","hello.docx"),("xlsx","public/fixtures/grid-operations-workbook.xlsx"),("pptx","public/fixtures/sample-deck.pptx")][source];
            let bytes = fixture(path);
            let names: Vec<_> = ["日本 #?", "same+name", "same name", "literal%2F%25"].into_iter().map(|name|format!("{name}.{ext}")).collect();
            let open = || Server::with_storage_options(dir.path(),StorageOptions {checkpoint_every:2,keep_tail:2,..StorageOptions::default()}).unwrap();
            let mut server = open();
            server.open(&names[0],bytes.clone()).unwrap();
            let mut edits = BTreeMap::from([(names[0].clone(),0u64)]);
            let mut sessions: BTreeMap<String,String> = BTreeMap::new();
            let mut agent = Agent::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut http = format!("http://{}",listener.local_addr().unwrap());
            let mut serving = tokio::spawn(server.clone().serve(listener));
            for (kind,slot) in steps.into_iter().chain([(4,0)]) {
                let name = &names[slot];
                if kind == 4 {
                    for session in sessions.values() {agent.tool("leave_document",json!({"session_id":session})).await;}
                    sessions.clear();
                    drop(agent);
                    serving.abort(); let _ = serving.await;
                    drop(server);
                    server = open();
                    agent = Agent::new();
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    http = format!("http://{}",listener.local_addr().unwrap());
                    serving = tokio::spawn(server.clone().serve(listener));
                } else if kind == 0 {
                    let opened = server.open(name,bytes.clone());
                    if edits.contains_key(name) {assert!(opened.is_err());}
                    else {opened.unwrap();edits.insert(name.clone(),0);}
                } else if edits.contains_key(name) {
                    let listed = agent.tool("list_documents",json!({"server":http})).await;
                    let url = listed["documents"].as_array().unwrap().iter().find(|d|d["name"]==*name).unwrap()["url"].as_str().unwrap();
                    if !sessions.contains_key(name) {
                        let joined = agent.tool("join_document",json!({"url":url})).await;
                        assert_eq!(joined["document"],*name);
                        sessions.insert(name.clone(),joined["session_id"].as_str().unwrap().to_string());
                    }
                    let session = &sessions[name];
                    if kind == 1 {
                        let count = edits[name]+1;
                        let edit = match source {
                            0 => json!({"type":"insert_text","paragraph":1,"offset":0,"text":"x"}),
                            1 => json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"number","value":count}}),
                            _ => json!({"type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"x"}),
                        };
                        assert_eq!(statuses(&agent.apply(session,json!([edit])).await),["applied"]);
                        edits.insert(name.clone(),count);
                    } else if kind == 3 {
                        agent.tool("leave_document",json!({"session_id":session})).await;
                        sessions.remove(name);
                    }
                }
                let listed = agent.tool("list_documents",json!({"server":http})).await;
                let documents = listed["documents"].as_array().unwrap();
                assert_eq!(documents.len(),edits.len());
                for document in documents {
                    let name = document["name"].as_str().unwrap();
                    let url = document["url"].as_str().unwrap();
                    assert!(url.is_ascii());
                    assert_eq!(document_name_from_url(url).as_deref(),Some(name));
                    assert_eq!(server.seq(name).await.unwrap(),edits[name]);
                    assert!(server.faults(name).await.unwrap().is_empty());
                    // An independent fresh session verifies persisted content,
                    // including documents with no previously retained socket.
                    let joined = agent.join(url).await;
                    let read = agent.read_all(&joined).await;
                    let count = edits[name];
                    assert_eq!(read["seq"],count);
                    match source {
                        0 => assert_eq!(read["paragraphs"][1]["text"],format!("{}World","x".repeat(count as usize))),
                        1 if count>0 => {
                            let sheet = read["sheets"].as_array().unwrap().iter().find(|s|s["name"]=="Operations").unwrap();
                            assert_eq!(sheet["cells"]["E6"],json!({"kind":"number","value":count as f64}));
                        }
                        2 => assert!(read["slides"][0]["notes"].as_str().unwrap().starts_with(&"x".repeat(count as usize))),
                        _ => {}
                    }
                    agent.tool("leave_document",json!({"session_id":joined})).await;
                }
            }
            for session in sessions.values() {agent.tool("leave_document",json!({"session_id":session})).await;}
            serving.abort(); let _ = serving.await;
        });
    }
}
