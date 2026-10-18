#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use jade_collab_mcp::McpServer;
use jade_collab_server::{Client, Server};
use jade_collab_sync::Document;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

pub const DOCX: &str = "hello.docx";
pub const XLSX: &str = "book.xlsx";

pub fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(path),
    )
    .unwrap()
}

/// A collaboration server on a free port hosting `hello.docx` and
/// `book.xlsx` (the grid-operations workbook).
pub struct Hosted {
    pub server: Server,
    /// `http://127.0.0.1:port`
    pub http: String,
    addr: String,
}

impl Hosted {
    pub async fn start() -> Self {
        let server = Server::new();
        server.open(DOCX, fixture("hello.docx")).unwrap();
        server
            .open(
                XLSX,
                fixture("public/fixtures/grid-operations-workbook.xlsx"),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(server.clone().serve(listener));
        Self {
            server,
            http: format!("http://{addr}"),
            addr,
        }
    }

    pub fn url(&self, name: &str) -> String {
        format!("ws://{}/documents/{name}/ws", self.addr)
    }

    /// A replica outside MCP, caught up with the server's log.
    pub async fn direct<D>(&self, name: &str) -> Client<D>
    where
        D: Document + DeserializeOwned + Send + 'static,
        D::Op: Serialize + DeserializeOwned + Send + 'static,
    {
        let client = Client::<D>::connect(&self.url(name)).await.unwrap();
        self.catch_up(name, &client).await;
        client
    }

    pub async fn catch_up<D>(&self, name: &str, client: &Client<D>)
    where
        D: Document + DeserializeOwned + Send + 'static,
        D::Op: Serialize + DeserializeOwned + Send + 'static,
    {
        let seq = self.server.seq(name).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), client.wait_for_seq(seq))
            .await
            .expect("caught up in time")
            .unwrap();
    }
}

/// An MCP client driving a [`McpServer`] in process, one JSON-RPC line at a
/// time.
pub struct Agent {
    pub mcp: McpServer,
    next_id: u64,
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent {
    pub fn new() -> Self {
        Self {
            mcp: McpServer::new(),
            next_id: 0,
        }
    }

    /// Sends one raw line and parses the response, if any.
    pub async fn send(&mut self, line: &str) -> Option<Value> {
        self.mcp
            .handle_message(line)
            .await
            .map(|response| serde_json::from_str(&response).expect("responses are JSON"))
    }

    /// Sends a request (a fresh `id` is added) and checks the response is a
    /// JSON-RPC 2.0 response to it.
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let response = self
            .send(
                &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string(),
            )
            .await
            .expect("a request is answered");
        assert_eq!(response["jsonrpc"], json!("2.0"), "{response}");
        assert_eq!(response["id"], json!(id), "{response}");
        assert!(
            response.get("result").is_some() != response.get("error").is_some(),
            "exactly one of result and error: {response}"
        );
        response
    }

    pub async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// A successful tool call's structured result.
    pub async fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.call(name, arguments.clone()).await;
        let result = &response["result"];
        assert_eq!(
            result["isError"],
            json!(false),
            "{name} {arguments}: {response}"
        );
        let text = result["content"][0]["text"].as_str().expect("text content");
        let structured = result["structuredContent"].clone();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            structured,
            "the text block carries the structured result"
        );
        structured
    }

    /// A tool call that ran and failed (`isError`), with its message.
    pub async fn tool_failure(&mut self, name: &str, arguments: Value) -> String {
        let response = self.call(name, arguments.clone()).await;
        assert_eq!(
            response["result"]["isError"],
            json!(true),
            "{name} {arguments}: {response}"
        );
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// A tool call refused as a JSON-RPC error, with its code and message.
    pub async fn rpc_error(&mut self, name: &str, arguments: Value) -> (i64, String) {
        let response = self.call(name, arguments.clone()).await;
        let error = &response["error"];
        assert!(error.is_object(), "{name} {arguments}: {response}");
        (
            error["code"].as_i64().unwrap(),
            error["message"].as_str().unwrap().to_string(),
        )
    }

    pub async fn join(&mut self, url: &str) -> String {
        let joined = self.tool("join_document", json!({ "url": url })).await;
        joined["session_id"].as_str().unwrap().to_string()
    }

    /// The whole document as `read_document` reports it.
    pub async fn read_all(&mut self, session: &str) -> Value {
        self.tool(
            "read_document",
            json!({ "session_id": session, "limit": 100_000 }),
        )
        .await
    }

    pub async fn apply(&mut self, session: &str, edits: Value) -> Value {
        self.tool(
            "apply_edits",
            json!({ "session_id": session, "edits": edits }),
        )
        .await
    }

    /// Waits until the session has seen the server's log up to `seq`.
    pub async fn catch_up(&mut self, session: &str, seq: u64) {
        loop {
            let read = self
                .tool(
                    "read_document",
                    json!({ "session_id": session, "limit": 1 }),
                )
                .await;
            let at = read["seq"].as_u64().unwrap();
            if at >= seq {
                return;
            }
            let waited = self
                .tool(
                    "wait_for_changes",
                    json!({ "session_id": session, "after_seq": at, "timeout_ms": 10_000 }),
                )
                .await;
            assert_eq!(waited["changed"], json!(true), "caught up in time");
        }
    }
}

/// The statuses of an `apply_edits` result, in order.
pub fn statuses(applied: &Value) -> Vec<String> {
    applied["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap().to_string())
        .collect()
}

/// Paragraph texts of a docx `read_document` result.
pub fn texts(read: &Value) -> Vec<String> {
    read["paragraphs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["text"].as_str().unwrap().to_string())
        .collect()
}
