mod common;
#[path = "../../jade-collab-server/tests/common/tls.rs"]
mod support;
use jade_collab_server::{Server, Tokens};
use proptest::prelude::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;
use support::{fixture, Proxy};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Process {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
}
impl Process {
    fn new(ca: &std::path::Path) -> Self {
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_jade-collab-mcp"))
            .env("SSL_CERT_FILE", ca)
            .env("JADE_COLLAB_TOKEN", "editor")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            input,
            output,
            id: 0,
        }
    }
    async fn call(&mut self, name: &str, args: Value) -> Value {
        self.id += 1;
        let line=json!({"jsonrpc":"2.0","id":self.id,"method":"tools/call","params":{"name":name,"arguments":args}}).to_string();
        self.input.write_all(line.as_bytes()).await.unwrap();
        self.input.write_all(b"\n").await.unwrap();
        self.input.flush().await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(15), self.output.next_line())
            .await
            .expect("bounded MCP request")
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
    async fn tool(&mut self, name: &str, args: Value) -> Value {
        let answer = self.call(name, args).await;
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        answer["result"]["structuredContent"].clone()
    }
    async fn close(mut self) {
        drop(self.input);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), self.child.wait())
                .await
                .expect("MCP shutdown")
                .unwrap()
                .success()
        );
    }
}

const NAMES: [&str; 3] = ["日本 #?.docx", "Budget +税.xlsx", "Deck %.pptx"];
async fn hosted() -> (
    Server,
    Proxy,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
) {
    let server = Server::new()
        .with_tokens(Tokens::from_json(r#"{"editor":{"name":"Ada","role":"editor"}}"#).unwrap());
    for (name, source) in NAMES.iter().zip([
        "hello.docx",
        "public/fixtures/grid-operations-workbook.xlsx",
        "public/fixtures/sample-deck.pptx",
    ]) {
        server.open(name, fixture(source)).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(server.clone().serve(listener));
    let proxy = Proxy::start(address).await;
    (server, proxy, serving)
}

#[tokio::test(flavor = "multi_thread")]
async fn https_discovery_returns_joinable_wss_urls_and_bad_certificates_fail_closed() {
    let (server, proxy, serving) = hosted().await;
    let mut mcp = Process::new(&proxy.ca);
    let listed = mcp
        .tool("list_documents", json!({"server":proxy.base(true)}))
        .await;
    assert_eq!(listed["documents"].as_array().unwrap().len(), 3);
    for document in listed["documents"].as_array().unwrap() {
        let name = document["name"].as_str().unwrap();
        let url = document["url"].as_str().unwrap();
        assert_eq!(url, proxy.socket(name, true));
        let joined = mcp.tool("join_document", json!({"url":url})).await;
        assert_eq!(joined["document"], name);
        mcp.tool("read_document",json!({"session_id":joined["session_id"],"include_base_formatting":!name.ends_with("xlsx")})).await;
        mcp.tool("leave_document", json!({"session_id":joined["session_id"]}))
            .await;
    }
    mcp.close().await;
    proxy.idle().await;
    for (host_ok, ca) in [(false, &proxy.ca), (true, &proxy.wrong_ca)] {
        let mut mcp = Process::new(ca);
        for (tool, args) in [
            ("list_documents", json!({"server":proxy.base(host_ok)})),
            (
                "join_document",
                json!({"url":proxy.socket(NAMES[0],host_ok)}),
            ),
        ] {
            let answer = mcp.call(tool, args).await;
            assert_eq!(answer["result"]["isError"], true, "{answer}");
        }
        mcp.close().await;
        proxy.idle().await;
    }
    for name in NAMES {
        assert_eq!(server.seq(name).await.unwrap(), 0);
    }
    serving.abort();
    let _ = serving.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_connections_and_invalid_urls_never_echo_query_credentials() {
    let (_, proxy, serving) = hosted().await;
    let mut mcp = Process::new(&proxy.ca);
    let secret = "query-credential-must-stay-private";
    for url in [
        format!("{}?token={secret}", proxy.socket(NAMES[0], false)),
        format!("ftp://localhost/a.docx?token={secret}"),
        format!("wss://localhost/a.txt?token={secret}"),
        format!("wss://user:{secret}@localhost/a.docx"),
    ] {
        let response = mcp.call("join_document", json!({"url":url})).await;
        assert!(
            !response.to_string().contains(secret),
            "diagnostics exposed URL credentials: {response}"
        );
    }
    mcp.close().await;
    proxy.idle().await;
    serving.abort();
    let _ = serving.await;
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]
    #[test]
    fn secure_discovery_session_and_trust_histories_preserve_native_edits(
        steps in prop::collection::vec((0u8..6,0usize..3),1..12),
    ) {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (server,proxy,serving)=hosted().await;
            let mut mcp=Process::new(&proxy.ca);let mut sessions:BTreeMap<usize,String>=BTreeMap::new();let mut counts=[0u64;3];
            for (kind,target) in steps.into_iter().chain([(0,0),(0,1),(0,2)]) {
                let name=NAMES[target];
                match kind {
                    0|1=>{
                        let session=if let Some(session)=sessions.get(&target) {session.clone()} else {
                            let joined=mcp.tool("join_document",json!({"url":proxy.socket(name,true)})).await;
                            let id=joined["session_id"].as_str().unwrap().to_owned();sessions.insert(target,id.clone());id
                        };
                        if kind==1 {
                            let next=counts[target]+1;
                            let edit=match target {
                                0=>json!({"type":"insert_text","paragraph":1,"offset":0,"text":"x"}),
                                1=>json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"number","value":next}}),
                                _=>json!({"type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"x"}),
                            };
                            let applied=mcp.tool("apply_edits",json!({"session_id":session,"edits":[edit]})).await;
                            assert_eq!(applied["results"][0]["status"],"applied");counts[target]=next;
                        }
                        let read=mcp.tool("read_document",json!({"session_id":session,"limit":100000})).await;
                        assert_eq!(read["seq"],counts[target]);
                        match target {
                            0=>assert_eq!(read["paragraphs"][1]["text"],format!("{}World","x".repeat(counts[target] as usize))),
                            1 if counts[target]>0=>{
                                let sheet=read["sheets"].as_array().unwrap().iter().find(|s|s["name"]=="Operations").unwrap();
                                assert_eq!(sheet["cells"]["E6"]["value"].as_f64(),Some(counts[target] as f64));
                            }
                            2=>assert!(read["slides"][0]["notes"].as_str().unwrap().starts_with(&"x".repeat(counts[target] as usize))),
                            _=>{}
                        }
                    }
                    2=>if let Some(session)=sessions.remove(&target) {mcp.tool("leave_document",json!({"session_id":session})).await;},
                    3=>{
                        let listed=mcp.tool("list_documents",json!({"server":proxy.base(true)})).await;
                        for document in listed["documents"].as_array().unwrap() {assert!(document["url"].as_str().unwrap().starts_with("wss://"));}
                    }
                    4=>{mcp.close().await;proxy.idle().await;sessions.clear();mcp=Process::new(&proxy.ca);}
                    _=>{
                        let (host_ok,ca)=if target==0 {(false,&proxy.ca)} else {(true,&proxy.wrong_ca)};
                        let mut bad=Process::new(ca);
                        let response=bad.call("join_document",json!({"url":proxy.socket(name,host_ok)})).await;
                        assert_eq!(response["result"]["isError"],true);bad.close().await;
                    }
                }
                for (name,count) in NAMES.iter().zip(counts) {assert_eq!(server.seq(name).await.unwrap(),count);assert!(server.faults(name).await.unwrap().is_empty());}
            }
            mcp.close().await;proxy.idle().await;serving.abort();let _=serving.await;
        });
    }
}
