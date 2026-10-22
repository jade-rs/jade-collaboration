//! A document name is one opaque UTF-8 path segment, decoded once by the server.
use axum::http::Uri;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The editing socket path for a raw document name. Percent signs in names
/// are escaped too; callers must pass the name, not a pre-encoded segment.
pub fn document_socket_path(name: &str) -> String {
    format!("{}/ws", document_path(name))
}

/// The HTTP upload/download path for a raw document name.
pub fn document_path(name: &str) -> String {
    format!("/documents/{}", utf8_percent_encode(name, SEGMENT))
}

/// The decoded document name at the end of an absolute HTTP(S)/WebSocket URL,
/// with an optional `/ws` suffix/trailing slash. Query parameters are separate
/// from the name. Rejects fragments, invalid escapes/UTF-8 and unusable names.
pub fn document_name_from_url(url: &str) -> Option<String> {
    if url.contains('#') {
        return None;
    }
    let uri: Uri = url.parse().ok()?;
    if !matches!(uri.scheme_str(), Some("http" | "https" | "ws" | "wss"))
        || uri.authority().is_none()
    {
        return None;
    }
    let path = uri.path().trim_end_matches('/');
    let path = path.strip_suffix("/ws").unwrap_or(path);
    let segment = path.rsplit('/').next()?;
    let mut bytes = segment.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            for _ in 0..2 {
                if !bytes.next().is_some_and(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
            }
        }
    }
    let name = percent_decode_str(segment).decode_utf8().ok()?.into_owned();
    crate::server::valid_name(&name).then_some(name)
}
