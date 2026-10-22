//! An MCP server (JSON-RPC over stdio) through which an AI agent takes part
//! in a collaborative session as a replica.
//!
//! Each `join_document` opens a [`jade_collab_server::Client`] — the same
//! optimistic replica a person's editor holds — and later tools read its
//! view and submit index-addressed edits resolved against it
//! ([`jade_docx_collab::intent`], [`jade_xlsx_collab::intent`]). The agent
//! never touches OOXML: the collaboration server materializes what is
//! committed. Protocol handling follows jade-core's
//! `jade-docx-headless-mcp`: tool failures are results with `isError`,
//! arguments the schema forbids are JSON-RPC `-32602` errors.

mod catalogue;
mod framing;
mod http;
mod read;
mod receipts;
mod session;
#[cfg(test)]
mod snapshot_tests;

use std::collections::BTreeMap;
use std::time::Duration;

use jade_collab_server::{document_name_from_url, document_socket_path, Format};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

pub use read::{read_docx, read_pptx, read_xlsx};
use receipts::ReceiptCache;
use session::{ReadSnapshot, Session};

/// MCP protocol version echoed back when the client does not request one.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "jade-collab-mcp";
const SERVER_TITLE: &str = "jade collaborative editing";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Items `read_document` lists when no `limit` is given.
const DEFAULT_LIMIT: usize = 500;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// The longest a tool waits, whatever it is asked: requests are handled one
/// at a time.
const MAX_TIMEOUT_MS: u64 = 120_000;

/// Maximum UTF-8 JSON-RPC request bytes, excluding LF or CRLF framing.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Maximum decoded HTTP body bytes accepted for document discovery.
pub const MAX_DISCOVERY_BYTES: usize = 8 * 1024 * 1024;

/// Why a tool call failed.
enum CallError {
    /// The arguments break the tool's schema or name nothing: a JSON-RPC
    /// error.
    Invalid(String),
    /// The tool ran and failed (the server is unreachable, …): a result
    /// with `isError`, so the agent can read the reason.
    Failed(String),
}

fn arguments<T: DeserializeOwned>(args: Value) -> Result<T, CallError> {
    serde_json::from_value(args).map_err(|e| CallError::Invalid(format!("invalid arguments: {e}")))
}

fn timeout(ms: Option<u64>) -> Duration {
    Duration::from_millis(ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    server: String,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinArgs {
    url: String,
    #[serde(default)]
    token: Option<String>,
}

/// The access token to present: the call's own, else `JADE_COLLAB_TOKEN`.
fn token(given: Option<String>) -> Option<String> {
    given.or_else(|| {
        std::env::var("JADE_COLLAB_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionArgs {
    session_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    session_id: String,
    #[serde(default)]
    start: usize,
    limit: Option<usize>,
    snapshot: Option<String>,
    include_base_formatting: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyArgs {
    session_id: String,
    edits: Vec<Value>,
    timeout_ms: Option<u64>,
    snapshot: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditStatusArgs {
    session_id: String,
    receipt: String,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    session_id: String,
    after_seq: u64,
    timeout_ms: Option<u64>,
}

/// The MCP server: the sessions this process has joined and the JSON-RPC
/// dispatch. Requests are handled one at a time, in order.
pub struct McpServer {
    sessions: BTreeMap<String, Session>,
    next_session: u64,
    /// At most one immutable read view per live session.
    snapshots: BTreeMap<String, (String, ReadSnapshot)>,
    next_snapshot: u64,
    instance_id: uuid::Uuid,
    receipts: BTreeMap<String, ReceiptCache>,
    next_receipt: u64,
}

impl Default for McpServer {
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            next_session: 0,
            snapshots: BTreeMap::new(),
            next_snapshot: 0,
            instance_id: uuid::Uuid::new_v4(),
            receipts: BTreeMap::new(),
            next_receipt: 0,
        }
    }
}

impl McpServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles one JSON-RPC message. Returns the response line for a
    /// request and `None` for a notification or blank input. Message payloads
    /// above MAX_REQUEST_BYTES are refused before parsing or changing state.
    pub async fn handle_message(&mut self, line: &str) -> Option<String> {
        if line.len() > MAX_REQUEST_BYTES {
            return Some(oversized_response());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let message: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                return Some(error_response(
                    Value::Null,
                    -32700,
                    &format!("parse error: {err}"),
                ))
            }
        };
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Some(error_response(
                id.unwrap_or(Value::Null),
                -32600,
                "invalid request: missing method",
            ));
        };
        // Notifications never get a response.
        if method == "initialized" || method.starts_with("notifications/") {
            return None;
        }
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let outcome = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": catalogue::catalogue() })),
            "tools/call" => self.call(params).await,
            other => Err((-32601, format!("method not found: {other}"))),
        };
        // A request without an `id` is a notification: do the work, stay silent.
        let id = id?;
        Some(match outcome {
            Ok(result) => success_response(id, result),
            Err((code, message)) => error_response(id, code, &message),
        })
    }

    async fn call(&mut self, params: Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "invalid params: missing tool name".to_string()))?
            .to_string();
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(args) => args.clone(),
        };
        let result = match name.as_str() {
            "list_documents" => list_documents(args).await,
            "join_document" => self.join(args).await,
            "read_document" => self.read(args).await,
            "apply_edits" => self.apply(args).await,
            "get_edit_status" => self.edit_status(args).await,
            "wait_for_changes" => self.wait(args).await,
            "leave_document" => self.leave(args).await,
            other => return Err((-32602, format!("unknown tool: {other}"))),
        };
        match result {
            Ok(value) => Ok(tool_result(Ok(value))),
            Err(CallError::Failed(message)) => Ok(tool_result(Err(message))),
            Err(CallError::Invalid(message)) => Err((-32602, message)),
        }
    }

    fn session(&self, id: &str) -> Result<&Session, CallError> {
        self.sessions
            .get(id)
            .ok_or_else(|| CallError::Invalid(format!("unknown session_id: {id}")))
    }

    async fn join(&mut self, args: Value) -> Result<Value, CallError> {
        let JoinArgs { url, token: given } = arguments(args)?;
        if !(url.starts_with("ws://") || url.starts_with("wss://")) {
            return Err(CallError::Invalid(
                "expected a ws:// or wss:// socket URL (list_documents gives each document's)"
                    .into(),
            ));
        }
        let format = Format::of_url(&url).ok_or_else(|| {
            CallError::Invalid("the URL does not name a .docx, .xlsx or .pptx document".into())
        })?;
        let next_session = self
            .next_session
            .checked_add(1)
            .ok_or_else(|| CallError::Failed("session identifier space exhausted".into()))?;
        let session = Session::join(&url, format, token(given).as_deref())
            .await
            .map_err(CallError::Failed)?;
        let document =
            document_name_from_url(&url).expect("document URL format checked before joining");
        self.next_session = next_session;
        let session_id = format!("session-{}-{}", self.instance_id, self.next_session);
        let joined = json!({
            "session_id": session_id,
            "document": document,
            "format": format,
            "replica": session.replica(),
            "seq": session.seq(),
        });
        self.sessions.insert(session_id, session);
        Ok(joined)
    }

    async fn read(&mut self, args: Value) -> Result<Value, CallError> {
        let ReadArgs {
            session_id,
            start,
            limit,
            snapshot,
            include_base_formatting,
        } = arguments(args)?;
        let limit = limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 {
            return Err(CallError::Invalid("limit must be at least 1".into()));
        }
        self.refresh_receipts(&session_id)?;
        if let Some(token) = snapshot {
            let (_, view) = self.snapshots.get(&session_id)
                .filter(|(id, _)| id == &token)
                .ok_or_else(|| CallError::Failed("read snapshot is unavailable for this session; start a new read without snapshot".into()))?;
            if include_base_formatting
                .is_some_and(|requested| requested != view.includes_base_formatting())
            {
                return Err(CallError::Failed("this snapshot has a different formatting mode; omit include_base_formatting or start a new read".into()));
            }
            let mut value = view.read(start, limit);
            value["snapshot"] = json!(token);
            return Ok(value);
        }
        let next = self
            .next_snapshot
            .checked_add(1)
            .ok_or_else(|| CallError::Failed("read snapshot identifier space exhausted".into()))?;
        let view = if include_base_formatting.unwrap_or(false) {
            self.session(&session_id)?
                .inspected_snapshot()
                .await
                .map_err(CallError::Failed)?
        } else {
            self.session(&session_id)?.snapshot()
        };
        let token = format!("read-{}-{next}", self.instance_id);
        let mut value = view.read(start, limit);
        value["snapshot"] = json!(token);
        self.snapshots.insert(session_id, (token, view));
        self.next_snapshot = next;
        Ok(value)
    }

    fn refresh_receipts(&mut self, session_id: &str) -> Result<(), CallError> {
        let answers = self.session(session_id)?.answers();
        self.receipts
            .entry(session_id.to_owned())
            .or_default()
            .observe(answers);
        Ok(())
    }

    async fn resolve_receipt(
        &mut self,
        session_id: &str,
        token: &str,
        wait: Duration,
    ) -> Result<Value, CallError> {
        self.refresh_receipts(session_id)?;
        let receipt = self
            .receipts
            .get(session_id)
            .and_then(|c| c.get(token))
            .ok_or_else(|| {
                CallError::Failed("edit receipt is expired or unavailable for this session".into())
            })?;
        let through = if receipt.complete() {
            None
        } else {
            receipt.through()
        };
        if let Some(ticket) = through {
            if let Ok(Err(error)) =
                tokio::time::timeout(wait, self.session(session_id)?.settle_through(ticket)).await
            {
                self.receipts
                    .get_mut(session_id)
                    .unwrap()
                    .disconnected(error);
            }
            self.refresh_receipts(session_id)?;
        }
        let mut value = self.receipts[session_id].get(token).unwrap().render();
        value["receipt"] = json!(token);
        value["seq"] = json!(self.session(session_id)?.seq());
        value["pending"] = json!(self.session(session_id)?.pending());
        Ok(value)
    }

    async fn edit_status(&mut self, args: Value) -> Result<Value, CallError> {
        let EditStatusArgs {
            session_id,
            receipt,
            timeout_ms,
        } = arguments(args)?;
        self.resolve_receipt(
            &session_id,
            &receipt,
            timeout(Some(timeout_ms.unwrap_or(0))),
        )
        .await
    }

    async fn apply(&mut self, args: Value) -> Result<Value, CallError> {
        let ApplyArgs {
            session_id,
            edits,
            timeout_ms,
            snapshot,
        } = arguments(args)?;
        self.refresh_receipts(&session_id)?;
        if !self.receipts[&session_id].has_room() {
            return Err(CallError::Failed("all 64 edit receipts are still unresolved; query their status before submitting more edits".into()));
        }
        let next = self
            .next_receipt
            .checked_add(1)
            .ok_or_else(|| CallError::Failed("edit receipt identifier space exhausted".into()))?;
        let expected = match snapshot {
            None => None,
            Some(token) => Some(&self.snapshots.get(&session_id)
                .filter(|(id, _)| id == &token)
                .ok_or_else(|| CallError::Failed("read snapshot is unavailable for this session; read again before editing".into()))?.1),
        };
        let receipt = self
            .session(&session_id)?
            .submit(edits, expected)
            .map_err(CallError::Failed)?;
        let token = format!("receipt-{}-{next}", self.instance_id);
        self.receipts
            .get_mut(&session_id)
            .unwrap()
            .insert(token.clone(), receipt)
            .expect("receipt capacity checked before submitting");
        self.next_receipt = next;
        self.resolve_receipt(&session_id, &token, timeout(timeout_ms))
            .await
    }

    async fn wait(&mut self, args: Value) -> Result<Value, CallError> {
        let WaitArgs {
            session_id,
            after_seq,
            timeout_ms,
        } = arguments(args)?;
        let result = self
            .session(&session_id)?
            .wait(after_seq, timeout(timeout_ms))
            .await
            .map_err(CallError::Failed);
        self.refresh_receipts(&session_id)?;
        result
    }

    async fn leave(&mut self, args: Value) -> Result<Value, CallError> {
        let SessionArgs { session_id } = arguments(args)?;
        let session = self
            .sessions
            .remove(&session_id)
            .ok_or_else(|| CallError::Invalid(format!("unknown session_id: {session_id}")))?;
        self.snapshots.remove(&session_id);
        self.receipts.remove(&session_id);
        let unanswered = session.leave().await;
        Ok(json!({ "session_id": session_id, "left": true, "unanswered": unanswered }))
    }
}

async fn list_documents(args: Value) -> Result<Value, CallError> {
    let ListArgs {
        server,
        token: given,
    } = arguments(args)?;
    let address = http::Address::parse(&server).map_err(CallError::Invalid)?;
    let documents = http::documents(&address, token(given).as_deref())
        .await
        .map_err(CallError::Failed)?;
    Ok(json!({
        "documents": documents
            .into_iter()
            .map(|listed| json!({
                "url": format!("{}{}", address.socket_base(), document_socket_path(&listed.name)),
                "name": listed.name,
                "format": listed.format,
            }))
            .collect::<Vec<_>>(),
    }))
}

fn initialize(params: &Value) -> Value {
    let protocol = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol,
        // The catalogue is fixed for the process lifetime.
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": SERVER_NAME,
            "title": SERVER_TITLE,
            "version": SERVER_VERSION,
        },
        "instructions": catalogue::INSTRUCTIONS,
    })
}

/// Wraps a tool outcome in the MCP `tools/call` result shape: successes as
/// `structuredContent` and as text, failures as text with `isError`.
fn tool_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&value).expect("JSON values serialize"),
            }],
            "structuredContent": value,
            "isError": false,
        }),
        Err(message) => json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true,
        }),
    }
}

fn success_response(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn oversized_response() -> String {
    error_response(
        Value::Null,
        -32600,
        &format!("request exceeds the {MAX_REQUEST_BYTES}-byte limit"),
    )
}

/// Serves newline-delimited JSON-RPC on stdin/stdout until stdin closes.
pub async fn serve_stdio(server: &mut McpServer) {
    use tokio::io::{AsyncWriteExt, BufReader};

    let mut lines = framing::Reader::new(BufReader::new(tokio::io::stdin()), MAX_REQUEST_BYTES);
    let mut stdout = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next().await {
        let response = match line {
            framing::Frame::TooLarge => Some(oversized_response()),
            framing::Frame::Bytes(bytes) => match String::from_utf8(bytes) {
                Ok(line) => server.handle_message(&line).await,
                Err(_) => Some(error_response(
                    Value::Null,
                    -32700,
                    "message is not valid UTF-8",
                )),
            },
        };
        if let Some(response) = response {
            if stdout.write_all(response.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    }
}
