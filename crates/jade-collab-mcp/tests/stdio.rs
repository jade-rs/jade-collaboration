//! The `jade-collab-mcp` binary, driven over stdin/stdout as an MCP client
//! drives it.

mod common;

use std::process::Stdio;
use std::time::Duration;

use common::{Hosted, DOCX};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Process {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl Process {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_jade-collab-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            stdin,
            stdout,
        }
    }

    async fn write(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.write_all(b"\n").await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(30), self.stdout.next_line())
            .await
            .expect("answered in time")
            .unwrap()
            .expect("the server is still running");
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.write(
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        )
        .await;
        let response = self.read().await;
        assert_eq!(response["id"], json!(id), "{response}");
        response
    }

    async fn tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        let response = self
            .request(
                id,
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        assert_eq!(response["result"]["isError"], json!(false), "{response}");
        response["result"]["structuredContent"].clone()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_edits_a_docx_over_stdio() {
    let hosted = Hosted::start().await;
    let mut mcp = Process::spawn();

    let init = mcp
        .request(
            1,
            "initialize",
            json!({ "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
        )
        .await;
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        json!("jade-collab-mcp")
    );
    // A notification gets no line back: the next line answers request 2.
    mcp.write(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .await;
    let tools = mcp.request(2, "tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 6);

    let listed = mcp
        .tool(3, "list_documents", json!({ "server": hosted.http }))
        .await;
    let url = listed["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == json!(DOCX))
        .unwrap()["url"]
        .clone();
    let joined = mcp.tool(4, "join_document", json!({ "url": url })).await;
    let session = joined["session_id"].clone();

    let applied = mcp
        .tool(
            5,
            "apply_edits",
            json!({ "session_id": session, "edits": [
                { "type": "insert_text", "paragraph": 1, "offset": 0, "text": "Hi " }
            ] }),
        )
        .await;
    assert_eq!(applied["results"][0]["status"], json!("applied"));
    let read = mcp
        .tool(6, "read_document", json!({ "session_id": session }))
        .await;
    assert_eq!(read["paragraphs"][1]["text"], json!("Hi World"));

    // Garbage and bad arguments are answered, and the server carries on.
    mcp.write("this is not json").await;
    assert_eq!(mcp.read().await["error"]["code"], json!(-32700));
    let bad = mcp
        .request(
            7,
            "tools/call",
            json!({ "name": "read_document", "arguments": { "session_id": 3 } }),
        )
        .await;
    assert_eq!(bad["error"]["code"], json!(-32602));

    let left = mcp
        .tool(8, "leave_document", json!({ "session_id": session }))
        .await;
    assert_eq!(left["left"], json!(true));

    // Closing stdin ends the process cleanly.
    drop(mcp.stdin);
    let status = tokio::time::timeout(Duration::from_secs(30), mcp.child.wait())
        .await
        .expect("exits once stdin closes")
        .unwrap();
    assert!(status.success());
}
