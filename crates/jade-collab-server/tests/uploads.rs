use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use jade_collab_server::{Server, Tokens};
use tower::ServiceExt;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(name),
    )
    .unwrap()
}

#[tokio::test]
async fn failed_native_opens_are_audited_once_without_retaining_the_payload_or_name() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("audit.jsonl");
    let data = root.path().join("data");
    let server = Server::with_storage(&data)
        .unwrap()
        .with_audit_log(&log)
        .unwrap()
        .with_tokens(
            Tokens::from_json(r#"{"edit-secret":{"name":"writer","role":"editor"}}"#).unwrap(),
        );
    for (name, source) in [
        ("a.docx", "hello.docx"),
        ("b.xlsx", "public/fixtures/grid-operations-workbook.xlsx"),
        ("c.pptx", "public/fixtures/sample-deck.pptx"),
    ] {
        let response = server
            .router()
            .oneshot(
                Request::put(format!("/documents/{name}"))
                    .header("Authorization", "Bearer edit-secret")
                    .body(Body::from("private-payload-marker is not a ZIP"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!server.documents().iter().any(|(found, _)| found == name));
        assert!(
            !data.join(name).exists(),
            "failed parsing initialized storage"
        );
        let events: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["event"] == "refused"
                    && event["document"] == name
                    && event["status"] == 400)
                .count(),
            1
        );
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(!text.contains("edit-secret") && !text.contains("private-payload-marker"));
        let response = server
            .router()
            .oneshot(
                Request::put(format!("/documents/{name}"))
                    .header("Authorization", "Bearer edit-secret")
                    .body(Body::from(fixture(source)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "a refused open kept its name reserved"
        );
    }
}

#[tokio::test]
async fn default_uploads_accept_real_office_packages_larger_than_two_mebibytes() {
    let server = Server::new();
    // Incompressible media-like payload in an unrelated part: opening and
    // downloading must retain it, without inflating the collaborative model.
    let mut seed = 7u64;
    let payload: Vec<u8> = (0..(2 * 1024 * 1024 + 65536))
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    for (name, source) in [
        ("a.docx", "hello.docx"),
        ("b.xlsx", "public/fixtures/grid-operations-workbook.xlsx"),
        ("c.pptx", "public/fixtures/sample-deck.pptx"),
    ] {
        let mut package = jade_opc::OpcPackage::open(&fixture(source)).unwrap();
        package.replace("custom/preserved.bin", payload.clone());
        let bytes = package.write().unwrap();
        assert!(bytes.len() > 2 * 1024 * 1024);
        let response = server
            .router()
            .oneshot(
                Request::put(format!("/documents/{name}"))
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED, "{name}");
        let export = server.export(name).await.unwrap();
        let package = jade_opc::OpcPackage::open(&export).unwrap();
        assert_eq!(
            package.part("custom/preserved.bin"),
            Some(payload.as_slice())
        );
        assert_eq!(server.seq(name).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn unauthorized_uploads_never_poll_the_request_body() {
    let server = Server::new().with_tokens(
        Tokens::from_json(
            r#"{
        "view":{"name":"reader","role":"viewer"},
        "other":{"name":"other","role":"editor","documents":["other.docx"]}
    }"#,
        )
        .unwrap(),
    );
    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("wrong"), StatusCode::UNAUTHORIZED),
        (Some("view"), StatusCode::FORBIDDEN),
        (Some("other"), StatusCode::FORBIDDEN),
    ] {
        let polled = Arc::new(AtomicBool::new(false));
        let read = polled.clone();
        let body = Body::from_stream(futures_util::stream::once(async move {
            read.store(true, Ordering::SeqCst);
            Err::<Bytes, _>(std::io::Error::other("unauthorized body must not be read"))
        }));
        let mut request = Request::put("/documents/secret.docx");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = server
            .router()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "token {token:?}");
        assert!(
            !polled.load(Ordering::SeqCst),
            "token {token:?} consumed its body"
        );
    }
    assert!(server.documents().is_empty());
}

fn chunks(bytes: &[u8], size: usize) -> Body {
    Body::from_stream(futures_util::stream::iter(
        bytes
            .chunks(size)
            .map(|c| Ok::<_, std::io::Error>(Bytes::copy_from_slice(c)))
            .collect::<Vec<_>>(),
    ))
}

#[tokio::test]
async fn streamed_upload_limits_are_inclusive_and_failures_leave_no_partial_document() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("audit.jsonl");
    let bytes = fixture("hello.docx");
    let server = Server::with_storage(dir.path().join("data"))
        .unwrap()
        .with_upload_limit(bytes.len())
        .with_audit_log(&log)
        .unwrap();
    let mut extra = bytes.clone();
    extra.push(0);
    let response = server
        .router()
        .oneshot(
            Request::put("/documents/a.docx")
                .body(chunks(&extra, 31))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!dir.path().join("data/a.docx").exists());
    assert!(server.documents().is_empty());
    let interrupted = futures_util::stream::iter(vec![
        Ok(Bytes::copy_from_slice(&bytes[..10])),
        Err(std::io::Error::other("connection ended while uploading")),
    ]);
    let response = server
        .router()
        .oneshot(
            Request::put("/documents/a.docx")
                .body(Body::from_stream(interrupted))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!dir.path().join("data/a.docx").exists());
    let response = server
        .router()
        .oneshot(
            Request::put("/documents/a.docx")
                .body(chunks(&bytes, 7))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "exactly the configured size is accepted"
    );
    let events: Vec<serde_json::Value> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let rejected: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "refused")
        .map(|e| (e["document"].clone(), e["status"].clone()))
        .collect();
    assert_eq!(
        rejected,
        vec![
            (serde_json::json!("a.docx"), serde_json::json!(413)),
            (serde_json::json!("a.docx"), serde_json::json!(400))
        ]
    );
    assert_eq!(
        events.iter().filter(|e| e["event"] == "uploaded").count(),
        1
    );
    drop(server);
    let reopened = Server::with_storage(dir.path().join("data")).unwrap();
    assert_eq!(reopened.documents().len(), 1);
    assert_eq!(reopened.seq("a.docx").await.unwrap(), 0);
    let zero = Server::new().with_upload_limit(0);
    assert_eq!(
        zero.router()
            .oneshot(
                Request::put("/documents/a.docx")
                    .body(Body::from(bytes))
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert!(zero.documents().is_empty());
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]
    #[test]
    fn upload_authorization_limits_retries_and_restarts_match_the_admission_model(
        steps in proptest::collection::vec((0u8..6,0usize..4,0u8..5,proptest::num::u16::ANY),1..24),
        source in 0usize..3,
    ) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let (ext,path) = [("docx","hello.docx"),("xlsx","public/fixtures/grid-operations-workbook.xlsx"),("pptx","public/fixtures/sample-deck.pptx")][source];
            let bytes = fixture(path);
            let package = |marker: &[u8]| {
                let mut package = jade_opc::OpcPackage::open(&bytes).unwrap();
                package.replace("custom/upload-version.bin",marker.to_vec());
                package.write().unwrap()
            };
            let initial = b"initial000";
            let limit = package(initial).len()+64;
            let tokens = || Tokens::from_json(r#"{"edit":{"name":"editor","role":"editor"},"view":{"name":"reader","role":"viewer"},"denied":{"name":"denied","role":"editor","documents":[]}}"#).unwrap();
            let open = || Server::with_storage(dir.path()).unwrap().with_upload_limit(limit).with_tokens(tokens());
            let mut server = open();
            let name_at = |slot: usize| format!("{}.{ext}", ["日本 #?", "same+name", "same name", "literal%2F%25"][slot]);
            let seed_name = name_at(0);
            let seeded = server.router().oneshot(Request::put(jade_collab_server::document_path(&seed_name)).header("Authorization","Bearer edit").body(chunks(&package(initial),127)).unwrap()).await.unwrap();
            assert_eq!(seeded.status(),StatusCode::CREATED);
            let mut exists = std::collections::BTreeMap::from([(seed_name,initial.to_vec())]);
            let mut relocated = std::collections::BTreeSet::new();
            for (kind,slot,role,bits) in steps {
                if kind == 4 || kind == 5 {
                    drop(server);
                    let name = name_at(slot);
                    if kind == 5 && exists.contains_key(&name) {
                        let ordinary = dir.path().join(&name);
                        let physical = dir.path().join(format!("storage-{name}"));
                        if relocated.remove(&slot) {std::fs::rename(physical,ordinary).unwrap();}
                        else {std::fs::rename(ordinary,physical).unwrap();relocated.insert(slot);}
                    }
                    server = open();
                } else {
                    let name = name_at(slot);
                    let mut request = Request::put(jade_collab_server::document_path(&name));
                    let token = [None,Some("wrong"),Some("view"),Some("denied"),Some("edit")][role as usize];
                    if let Some(token) = token {request = request.header("Authorization",format!("Bearer {token}"));}
                    // Headers cannot authorize more bytes than actually read.
                    if bits & 1 != 0 {request = request.header("Content-Length",if bits & 2 == 0 {"0"} else {"18446744073709551615"});}
                    let marker = format!("{slot:02x}{role:02x}{kind:02x}{bits:04x}").into_bytes();
                    let mut input = package(&marker);
                    assert!(input.len() <= limit);
                    if kind == 0 {input.resize(limit+1,0);}
                    if kind == 1 {input.truncate(7);}
                    let expected = match role {
                        0|1 => StatusCode::UNAUTHORIZED,
                        2|3 => StatusCode::FORBIDDEN,
                        _ if input.len() > limit => StatusCode::PAYLOAD_TOO_LARGE,
                        _ if exists.contains_key(&name) => StatusCode::CONFLICT,
                        _ if kind == 1 => StatusCode::BAD_REQUEST,
                        _ => {exists.insert(name.clone(),marker); StatusCode::CREATED},
                    };
                    let response = server.router().oneshot(request.body(chunks(&input,1+usize::from(bits)%997)).unwrap()).await.unwrap();
                    assert_eq!(response.status(),expected,"kind {kind}, role {role}, {name}");
                }
                let actual: std::collections::BTreeSet<_> = server.documents().into_iter().map(|(n,_)|n).collect();
                assert_eq!(actual,exists.keys().cloned().collect());
                for slot in 0..4 {
                    let name = name_at(slot);
                    if let Some(marker) = exists.get(&name) {
                        assert_eq!(server.seq(&name).await.unwrap(),0);
                        assert!(server.faults(&name).await.unwrap().is_empty());
                        let export = server.export(&name).await.unwrap();
                        let export = jade_opc::OpcPackage::open(&export).unwrap();
                        assert_eq!(export.part("custom/upload-version.bin"),Some(marker.as_slice()),"an existing upload was replaced after refusal or restart");
                    } else {assert!(!dir.path().join(name).exists(),"a rejected request reserved durable storage");}
                }
            }
        });
    }
}
