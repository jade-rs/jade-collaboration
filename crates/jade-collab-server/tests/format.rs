//! Which format a document URL names.

use jade_collab_server::Format;
use proptest::prelude::*;

#[test]
fn socket_urls_name_their_format() {
    assert_eq!(
        Format::of_url("ws://127.0.0.1:8787/documents/contract.docx/ws"),
        Some(Format::Docx)
    );
    assert_eq!(
        Format::of_url("ws://h/documents/Budget.XLSX/ws/"),
        Some(Format::Xlsx)
    );
    assert_eq!(Format::of_url("ws://h/documents/notes.txt/ws"), None);
    assert_eq!(Format::of_url("ws://h/documents/a.docx.bak/ws"), None);
}

proptest! {
    /// A socket URL and the bare document name agree on the format, with or
    /// without the `/ws` suffix and a trailing slash.
    #[test]
    fn a_socket_url_has_the_format_of_its_document_name(
        stem in "[A-Za-z0-9_-]{1,12}",
        extension in prop::sample::select(&["docx", "DOCX", "xlsx", "Xlsx", "pptx", "txt", "docx.bak", ""][..]),
        port in 1u16..,
        socket in any::<bool>(),
        slash in any::<bool>(),
    ) {
        let name = if extension.is_empty() { stem } else { format!("{stem}.{extension}") };
        let mut url = format!("ws://127.0.0.1:{port}/documents/{name}");
        if socket {
            url.push_str("/ws");
        }
        if slash {
            url.push('/');
        }
        prop_assert_eq!(Format::of_url(&url), Format::of(&name));
    }
}
