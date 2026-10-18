//! `GET /documents` on a collaboration server.

use std::time::Duration;

use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::{header, Request};
use hyper_util::rt::TokioIo;
use jade_collab_server::Format;
use serde::Deserialize;
use tokio::net::TcpStream;

#[derive(Deserialize)]
pub(crate) struct Listed {
    pub(crate) name: String,
    pub(crate) format: Format,
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// The documents the server at `authority` (`host:port`) hosts.
pub(crate) async fn documents(authority: &str) -> Result<Vec<Listed>, String> {
    tokio::time::timeout(TIMEOUT, fetch(authority))
        .await
        .map_err(|_| format!("{authority} did not answer within {TIMEOUT:?}"))?
}

async fn fetch(authority: &str) -> Result<Vec<Listed>, String> {
    let stream = TcpStream::connect(authority)
        .await
        .map_err(|e| format!("cannot connect to {authority}: {e}"))?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| format!("cannot talk HTTP to {authority}: {e}"))?;
    tokio::spawn(connection);
    let request = Request::get("/documents")
        .header(header::HOST, authority)
        .body(Empty::<Bytes>::new())
        .map_err(|e| e.to_string())?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| format!("GET /documents failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("GET /documents answered {}", response.status()));
    }
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("GET /documents failed: {e}"))?
        .to_bytes();
    serde_json::from_slice(&body).map_err(|e| format!("GET /documents answered {e}"))
}
