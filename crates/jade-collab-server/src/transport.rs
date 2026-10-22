//! Shared HTTP/WebSocket connection policy. Secure schemes complete native-root
//! certificate and hostname verification before any application bytes are sent.
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{rustls, TlsConnector};
use tokio_tungstenite::{tungstenite::http::Uri, MaybeTlsStream};

#[derive(Clone, Debug)]
pub struct TransportAddress {
    host: String,
    port: u16,
    encrypted: bool,
}

impl TransportAddress {
    /// Parses an absolute HTTP(S)/WS(S) endpoint. Userinfo and malformed ports
    /// are refused; credentials belong in the request's authorization header.
    pub fn parse(uri: &Uri) -> Result<Self, String> {
        let encrypted = match uri.scheme_str() {
            Some("https" | "wss") => true,
            Some("http" | "ws") => false,
            _ => return Err("expected an http(s) or ws(s) endpoint".into()),
        };
        let authority = uri.authority().ok_or("the endpoint needs a host")?;
        if authority.as_str().contains('@') {
            return Err("URL userinfo is not supported".into());
        }
        let raw_host = uri
            .host()
            .filter(|host| !host.is_empty())
            .ok_or("the endpoint needs a host")?;
        let suffix = authority
            .as_str()
            .strip_prefix(raw_host)
            .ok_or("invalid endpoint authority")?;
        let port = if suffix.is_empty() {
            if encrypted {
                443
            } else {
                80
            }
        } else {
            let port = suffix.strip_prefix(':').ok_or("invalid endpoint port")?;
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return Err("invalid endpoint port".into());
            }
            port.parse::<u16>().map_err(|_| "invalid endpoint port")?
        };
        let host = raw_host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(raw_host)
            .to_owned();
        Ok(Self {
            host,
            port,
            encrypted,
        })
    }
    pub fn encrypted(&self) -> bool {
        self.encrypted
    }

    /// Connects using the selected scheme, with no plaintext fallback. Callers
    /// bound the complete request, including this handshake, with one deadline.
    pub async fn connect(&self) -> Result<MaybeTlsStream<TcpStream>, String> {
        let stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|e| format!("cannot connect to {}:{}: {e}", self.host, self.port))?;
        if !self.encrypted {
            return Ok(MaybeTlsStream::Plain(stream));
        }
        let loaded = rustls_native_certs::load_native_certs();
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(loaded.certs);
        if roots.is_empty() {
            return Err(
                "no usable native root certificates (check SSL_CERT_FILE/SSL_CERT_DIR)".into(),
            );
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(self.host.clone())
            .map_err(|_| "invalid TLS server name")?;
        let stream = TlsConnector::from(Arc::new(config))
            .connect(name, stream)
            .await
            .map_err(|e| format!("TLS verification/handshake failed: {e}"))?;
        Ok(MaybeTlsStream::Rustls(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ports_ipv6_and_security_are_derived_from_the_actual_scheme() {
        for (url, host, port, encrypted) in [
            ("http://example.org/", "example.org", 80, false),
            ("https://example.org/", "example.org", 443, true),
            ("ws://[::1]:8787/a?token=secret", "::1", 8787, false),
            ("wss://[::1]/a", "::1", 443, true),
        ] {
            let address = TransportAddress::parse(&url.parse().unwrap()).unwrap();
            assert_eq!(
                (address.host.as_str(), address.port, address.encrypted),
                (host, port, encrypted)
            );
        }
        for url in [
            "ftp://example.org/",
            "http://user:secret@host/",
            "https://host:abc/",
            "https://host:99999/",
            "https://host:/",
        ] {
            assert!(
                url.parse::<Uri>()
                    .ok()
                    .and_then(|u| TransportAddress::parse(&u).ok())
                    .is_none(),
                "{url}"
            );
        }
    }
}
