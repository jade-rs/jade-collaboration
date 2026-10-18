//! `jade-collab-mcp`: an MCP server on stdio through which an agent joins
//! documents hosted by `jade-collab serve` as a replica.

use jade_collab_mcp::{serve_stdio, McpServer};

#[tokio::main]
async fn main() {
    let mut server = McpServer::new();
    serve_stdio(&mut server).await;
}
