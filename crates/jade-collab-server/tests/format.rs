//! Which format a document URL names.

use jade_collab_server::{document_name_from_url, document_path, document_socket_path, Format};
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

#[test]
fn document_format_comes_from_the_decoded_path_without_query_credentials() {
    for (url, format) in [
        (
            "ws://h/documents/report%2Edocx/ws?token=secret",
            Format::Docx,
        ),
        (
            "wss://h/documents/Budget%2EXLSX/ws/?token=secret&next=/x.txt",
            Format::Xlsx,
        ),
        (
            "ws://[::1]:8787/documents/deck%20%231.pptx/ws?token=secret",
            Format::Pptx,
        ),
    ] {
        assert_eq!(Format::of_url(url), Some(format), "{url}");
    }
    for url in [
        "ws://h/documents/not-a-document.txt/ws?name=fake.docx",
        "ws://h/documents/%FF.docx/ws",
        "ws://h/documents/a%2Fhidden.docx/ws",
        "ws://h/documents/a%ZZ.docx/ws",
        "ws://h/documents/a%.docx/ws",
        "ws://h/documents/a.docx/ws#fragment",
    ] {
        assert_eq!(Format::of_url(url), None, "{url}");
    }
}

proptest! {
    #[test]
    fn opaque_unicode_names_round_trip_once_without_path_or_query_delimiters(
        parts in prop::collection::vec(prop::sample::select(&["x","日本","é","🙂"," ","#","?","%2F","%25","a+b","[x]","_~","&"][..]),0..12),
        extension in prop::sample::select(&["docx","XLSX","pptx"][..]),
        port in 1u16..,
    ) {
        let name = format!("doc {}.{extension}",parts.concat());
        let path = document_socket_path(&name);
        prop_assert!(path.is_ascii());
        prop_assert!(!path.contains('?') && !path.contains('#'));
        prop_assert_eq!(path.split('/').count(),4);
        let url = format!("ws://[::1]:{port}{path}?token=secret&redirect=/wrong.txt");
        let decoded = document_name_from_url(&url);
        prop_assert_eq!(decoded.as_deref(),Some(name.as_str()));
        prop_assert_eq!(Format::of_url(&url),Format::of(&name));
        let download = format!("http://h{}?download=true",document_path(&name));
        let decoded = document_name_from_url(&download);
        prop_assert_eq!(decoded.as_deref(),Some(name.as_str()));
    }

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
