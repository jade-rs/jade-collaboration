//! An MCP server (JSON-RPC over stdio) through which an AI agent takes part
//! in a collaborative session as a replica.
//!
//! Each `join_document` opens a [`jade_collab_server::Client`] — the same
//! optimistic replica a person's editor holds — and later tools read its
//! view and submit index-addressed edits resolved against it
//! ([`jade_docx_collab::intent`], [`jade_xlsx_collab::intent`]). The agent
//! never touches OOXML: the collaboration server materializes what is
//! committed. Protocol handling follows jade-core's
//! `jade-docx-headless-mcp`.

use jade_docx_collab::DocxModel;
use jade_xlsx_collab::XlsxModel;
use serde_json::Value;

/// The MCP server: the sessions this process has joined and the JSON-RPC
/// dispatch. Requests are handled one at a time, in order.
#[derive(Default)]
pub struct McpServer {}

impl McpServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles one JSON-RPC message. Returns the response line for a
    /// request and `None` for a notification or blank input.
    pub async fn handle_message(&mut self, line: &str) -> Option<String> {
        let _ = line;
        todo!()
    }
}

/// Serves newline-delimited JSON-RPC on stdin/stdout until stdin closes.
pub async fn serve_stdio(server: &mut McpServer) {
    let _ = server;
    todo!()
}

/// What `read_document` reports for a docx view, without `seq`/`pending`:
/// paragraphs `start..start + limit` and every top-level table.
pub fn read_docx(model: &DocxModel, start: usize, limit: usize) -> Value {
    let _ = (model, start, limit);
    todo!()
}

/// What `read_document` reports for an xlsx view, without `seq`/`pending`:
/// cells `start..start + limit` of the non-blank cells of every sheet in
/// order.
pub fn read_xlsx(model: &XlsxModel, start: usize, limit: usize) -> Value {
    let _ = (model, start, limit);
    todo!()
}
