//! `GET /documents` on a collaboration server.

use std::time::Duration;

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::{Body, Bytes};
use hyper::{header, Request, Uri};
use hyper_util::rt::TokioIo;
use jade_collab_server::transport::TransportAddress;
use jade_collab_server::Format;
use serde::Deserialize;

pub(crate) struct Address {
    authority: String,
    endpoint: TransportAddress,
}
impl Address {
    pub(crate) fn parse(server: &str) -> Result<Self, String> {
        let invalid =
            || "expected a server address (http://host:port or https://host:port)".to_string();
        let uri: Uri = server.parse().map_err(|_| invalid())?;
        if server.contains('#')
            || !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.query().is_some()
            || !uri.path().trim_matches('/').is_empty()
        {
            return Err(invalid());
        }
        let endpoint = TransportAddress::parse(&uri).map_err(|_| invalid())?;
        let authority = uri.authority().ok_or_else(invalid)?.as_str().to_owned();
        Ok(Self {
            authority,
            endpoint,
        })
    }
    pub(crate) fn socket_base(&self) -> String {
        format!(
            "{}://{}",
            if self.endpoint.encrypted() {
                "wss"
            } else {
                "ws"
            },
            self.authority
        )
    }
}

struct ConnectionTask(tokio::task::AbortHandle);
impl Drop for ConnectionTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Deserialize)]
pub(crate) struct Listed {
    pub(crate) name: String,
    pub(crate) format: Format,
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// The documents the server at `authority` (`host:port`) hosts.
pub(crate) async fn documents(
    address: &Address,
    token: Option<&str>,
) -> Result<Vec<Listed>, String> {
    tokio::time::timeout(TIMEOUT, fetch(address, token))
        .await
        .map_err(|_| format!("{} did not answer within {TIMEOUT:?}", address.authority))?
}

async fn fetch(address: &Address, token: Option<&str>) -> Result<Vec<Listed>, String> {
    let stream = address.endpoint.connect().await?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| format!("cannot talk HTTP to {}: {e}", address.authority))?;
    let _connection = ConnectionTask(tokio::spawn(connection).abort_handle());
    let mut request = Request::get("/documents").header(header::HOST, address.authority.as_str());
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = request
        .body(Empty::<Bytes>::new())
        .map_err(|e| e.to_string())?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| format!("GET /documents failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("GET /documents answered {}", response.status()));
    }
    read_documents(response.into_body(), crate::MAX_DISCOVERY_BYTES).await
}

async fn read_documents<B>(body: B, limit: usize) -> Result<Vec<Listed>, String>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let body = Limited::new(body, limit)
        .collect()
        .await
        .map_err(|e| format!("GET /documents failed: {e}"))?
        .to_bytes();
    serde_json::from_slice(&body).map_err(|e| format!("GET /documents answered {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn discovery_accepts_the_exact_byte_limit_but_never_publishes_an_oversized_valid_json_prefix(
    ) {
        const LIMIT: usize = 8 * 1024 * 1024;
        for chunked in [false, true] {
            for size in [LIMIT, LIMIT + 1] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address =
                    Address::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
                let peer = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut headers = Vec::new();
                    loop {
                        headers.push(stream.read_u8().await.unwrap());
                        if headers.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    let body = format!("[]{}", " ".repeat(size - 2));
                    let response = if chunked {
                        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_string()
                    } else {
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n")
                    };
                    stream.write_all(response.as_bytes()).await.unwrap();
                    for fragment in body.as_bytes().chunks(65521) {
                        let result = if chunked {
                            let header = format!("{:x}\r\n", fragment.len());
                            if stream.write_all(header.as_bytes()).await.is_err() {
                                break;
                            }
                            if stream.write_all(fragment).await.is_err() {
                                break;
                            }
                            stream.write_all(b"\r\n").await
                        } else {
                            stream.write_all(fragment).await
                        };
                        if result.is_err() {
                            break;
                        }
                    }
                    if chunked {
                        let _ = stream.write_all(b"0\r\n\r\n").await;
                    }
                });
                let result = documents(&address, None).await;
                assert_eq!(
                    result.is_ok(),
                    size <= LIMIT,
                    "chunked={chunked}, bytes={size}"
                );
                peer.await.unwrap();
            }
        }
    }

    #[test]
    fn discovery_addresses_reject_credentials_fragments_queries_and_non_root_paths() {
        for server in [
            "https://user:secret@host/",
            "https://host/?token=secret",
            "https://host/#fragment",
            "https://host/prefix",
            "https://host:99999/",
            "https://host:abc/",
            "wss://host/",
            "http:///documents",
        ] {
            assert!(Address::parse(server).is_err(), "{server}");
        }
    }

    proptest! {
        #[test]
        fn discovery_frames_obey_the_byte_model_and_incomplete_streams_never_publish(
            names in prop::collection::vec(prop::sample::select(&["a.docx","日本語.xlsx","literal%2F.pptx","space #?.docx"][..]),0..12),
            limit in 0usize..1000,chunk in 1usize..63,interrupted in any::<bool>(),
        ) {
            let expected:Vec<_>=names.iter().map(|name|((*name).to_string(),Format::Docx)).collect();
            let encoded=serde_json::to_vec(&names.iter().map(|name|serde_json::json!({"name":name,"format":"docx"})).collect::<Vec<_>>()).unwrap();
            let accepted=encoded.len()<=limit && !interrupted;
            let mut frames:Vec<Result<hyper::body::Frame<Bytes>,std::io::Error>>=encoded.chunks(chunk).map(|c|Ok(hyper::body::Frame::data(Bytes::copy_from_slice(c)))).collect();
            if interrupted {frames.push(Err(std::io::Error::other("incomplete response")));}
            let body=http_body_util::StreamBody::new(futures_util::stream::iter(frames));
            let result=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(read_documents(body,limit));
            prop_assert_eq!(result.is_ok(),accepted);
            if let Ok(documents)=result {prop_assert_eq!(documents.into_iter().map(|d|(d.name,d.format)).collect::<Vec<_>>(),expected);}
        }

        #[test]
        fn discovery_socket_scheme_preserves_encryption_and_the_original_authority(
            secure in any::<bool>(),ipv6 in any::<bool>(),port in prop::option::of(any::<u16>()),slashes in 0..4usize,
        ) {
            let host=if ipv6 {"[::1]"} else {"example.org"};
            let authority=if let Some(port)=port {format!("{host}:{port}")} else {host.into()};
            let address=Address::parse(&format!("{}://{authority}{}",if secure {"https"} else {"http"},"/".repeat(slashes))).unwrap();
            prop_assert_eq!(address.socket_base(),format!("{}://{authority}",if secure {"wss"} else {"ws"}));
            prop_assert_eq!(address.endpoint.encrypted(),secure);
        }
    }

    #[tokio::test]
    async fn canceled_discovery_drops_its_http_connection_before_the_server_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address =
            Address::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (sent, seen) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            loop {
                headers.push(stream.read_u8().await.unwrap());
                if headers.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            assert!(headers.starts_with(b"GET /documents "));
            let _ = sent.send(());
            let mut next = [0; 1];
            assert_eq!(
                stream.read(&mut next).await.unwrap(),
                0,
                "cancellation must close the driver socket"
            );
        });
        let mut request = Box::pin(documents(&address, None));
        tokio::select! {_=&mut request=>panic!("no response was sent"),result=seen=>result.unwrap()}
        drop(request);
        tokio::time::timeout(Duration::from_secs(3), peer)
            .await
            .expect("canceled HTTP socket released")
            .unwrap();
    }

    #[tokio::test]
    async fn discovery_refuses_redirects_instead_of_forwarding_credentials_to_another_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address =
            Address::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            loop {
                headers.push(stream.read_u8().await.unwrap());
                if headers.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/stolen\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        });
        let error = documents(&address, Some("secret"))
            .await
            .err()
            .expect("redirect refused");
        assert!(error.contains("302"));
        peer.await.unwrap();
    }
}
