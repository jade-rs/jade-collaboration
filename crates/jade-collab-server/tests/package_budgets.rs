//! Exercise the engine's shared package budgets through every hosted format.
//! No giant allocation/expansion is needed: ZIP metadata is checked first.
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::OnceLock;

use axum::body::{to_bytes, Body, Bytes};
use axum::http::{Request, StatusCode};
use jade_collab_server::{Server, Tokens};
use jade_opc::{OpcPackage, PackageLoadLimits};
use proptest::prelude::*;
use tower::ServiceExt;

struct Inputs {
    name: &'static str,
    valid: Vec<u8>,
    part: Vec<u8>,
    total: Vec<u8>,
    entries: Vec<u8>,
}
fn inputs() -> &'static [Inputs] {
    static INPUTS: OnceLock<Vec<Inputs>> = OnceLock::new();
    INPUTS.get_or_init(|| {
        [
            ("guard.docx", "hello.docx"),
            (
                "guard.xlsx",
                "public/fixtures/grid-operations-workbook.xlsx",
            ),
            ("guard.pptx", "public/fixtures/sample-deck.pptx"),
        ]
        .into_iter()
        .map(|(name, source)| {
            let valid = std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../jade-core")
                    .join(source),
            )
            .unwrap();
            let limits = PackageLoadLimits::default();
            let mut package = OpcPackage::open(&valid).unwrap();
            for n in 0..5 {
                package.replace(&format!("custom/budget-{n}.bin"), vec![0]);
            }
            let mut part = package.write().unwrap();
            claim_sizes(&mut part, |name| {
                (name == "custom/budget-0.bin").then_some(limits.max_part_bytes + 1)
            });
            let mut total = package.write().unwrap();
            claim_sizes(&mut total, |name| {
                name.starts_with("custom/budget-")
                    .then_some(limits.max_part_bytes)
            });
            assert!(5 * limits.max_part_bytes > limits.max_total_bytes);
            while package.part_names().len() <= limits.max_entries {
                package.replace(
                    &format!("custom/empty-{}.bin", package.part_names().len()),
                    vec![],
                );
            }
            let entries = package.write().unwrap();
            Inputs {
                name,
                valid,
                part,
                total,
                entries,
            }
        })
        .collect()
    })
}

/// Patch only central-directory declared sizes in our small, non-ZIP64 test
/// archives. Compressed bytes stay tiny; the native preflight must refuse
/// these declarations before attempting to inflate or parse any Office XML.
fn claim_sizes(bytes: &mut [u8], size: impl Fn(&str) -> Option<u64>) {
    let end = bytes.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
    let mut offset = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    while bytes.get(offset..offset + 4) == Some(b"PK\x01\x02") {
        let len = |at| {
            u16::from_le_bytes(bytes[offset + at..offset + at + 2].try_into().unwrap()) as usize
        };
        let (name, extra, comment) = (len(28), len(30), len(32));
        let selected = size(std::str::from_utf8(&bytes[offset + 46..offset + 46 + name]).unwrap());
        if let Some(selected) = selected {
            bytes[offset + 24..offset + 28]
                .copy_from_slice(&u32::try_from(selected).unwrap().to_le_bytes());
        }
        offset += 46 + name + extra + comment;
    }
}
fn body(bytes: &[u8], fragment: usize) -> Body {
    // Keep the 10,001-entry case from constructing a million tiny test
    // allocations; small-byte HTTP fragmentation has its own upload tests.
    let fragment = fragment.max(16);
    Body::from_stream(futures_util::stream::iter(
        bytes
            .chunks(fragment)
            .map(|c| Ok::<_, std::io::Error>(Bytes::copy_from_slice(c)))
            .collect::<Vec<_>>(),
    ))
}

#[tokio::test]
async fn package_budget_refusals_release_names_storage_and_keep_other_rooms_alive() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let log = root.path().join("audit.jsonl");
    let server = Server::with_storage(&data)
        .unwrap()
        .with_audit_log(&log)
        .unwrap();
    server.open("live.docx", inputs()[0].valid.clone()).unwrap();
    let live = server.export("live.docx").await.unwrap();
    for input in inputs() {
        for (bytes, resource) in [
            (&input.part, "part bytes"),
            (&input.total, "expanded bytes"),
            (&input.entries, "entries"),
        ] {
            let response = server
                .router()
                .oneshot(
                    Request::put(format!("/documents/{}", input.name))
                        .body(body(bytes, 97))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let error = String::from_utf8(
                to_bytes(response.into_body(), 16384)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap();
            assert!(
                error.contains("package limit exceeded") && error.contains(resource),
                "{}: {error}",
                input.name
            );
            assert!(!data.join(input.name).exists());
            assert!(!server
                .documents()
                .iter()
                .any(|(name, _)| name == input.name));
            assert_eq!(server.export("live.docx").await.unwrap(), live);
            assert_eq!(server.seq("live.docx").await.unwrap(), 0);
        }
        let response = server
            .router()
            .oneshot(
                Request::put(format!("/documents/{}", input.name))
                    .body(body(&input.valid, 31))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(server.faults(input.name).await.unwrap().is_empty());
    }
    let events: Vec<serde_json::Value> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "refused" && e["status"] == 400)
            .count(),
        9
    );
    assert_eq!(
        events.iter().filter(|e| e["event"] == "uploaded").count(),
        3
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]
    #[test]
    fn resource_refusal_upload_authorization_and_restart_follow_the_publication_model(
        format in 0usize..3,
        history in prop::collection::vec((0u8..8, any::<bool>(), 1usize..256), 1..24),
    ) {
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap()).block_on(async {
            let input = &inputs()[format];
            let root = tempfile::tempdir().unwrap();
            let open = || Server::with_storage(root.path()).unwrap().with_tokens(Tokens::from_json(r#"{"edit":{"name":"writer","role":"editor"},"view":{"name":"reader","role":"viewer"}}"#).unwrap());
            let mut server = open();
            let mut loaded = false;
            let mut exported = None;
            for (kind, viewer, fragment) in history {
                if kind == 7 {
                    drop(server); server = open();
                } else if kind == 6 {
                    prop_assert_eq!(server.export(input.name).await.is_ok(), loaded);
                } else {
                    let bytes = match kind {0 => &input.valid, 1 => &input.part, 2 => &input.total, 3 => &input.entries, _ => b"not a zip".as_slice()};
                    let expected = if viewer {StatusCode::FORBIDDEN} else if loaded {StatusCode::CONFLICT} else if kind == 0 {StatusCode::CREATED} else {StatusCode::BAD_REQUEST};
                    let response = server.router().oneshot(Request::put(format!("/documents/{}",input.name)).header("Authorization",if viewer {"Bearer view"} else {"Bearer edit"}).body(body(bytes,fragment)).unwrap()).await.unwrap();
                    prop_assert_eq!(response.status(),expected);
                    if expected == StatusCode::CREATED {loaded=true; exported=Some(server.export(input.name).await.unwrap());}
                }
                prop_assert_eq!(server.documents().len(),usize::from(loaded));
                if loaded {
                    prop_assert_eq!(server.seq(input.name).await.unwrap(),0);
                    prop_assert_eq!(server.export(input.name).await.unwrap(),exported.as_ref().unwrap().as_slice());
                    prop_assert!(server.faults(input.name).await.unwrap().is_empty());
                } else {prop_assert!(!root.path().join(input.name).exists());}
            }
            Ok(())
        })?;
    }
}

proptest! {
    #[test]
    fn actual_inflated_reads_match_atomic_budget_accounting(
        part in 0u64..128, total in 0u64..512,
        history in prop::collection::vec((prop::collection::vec(any::<u8>(),0..160),any::<bool>(),1usize..33),0..25),
    ) {
        let limits=PackageLoadLimits {max_part_bytes:part,max_total_bytes:total,..PackageLoadLimits::default()};
        let mut used=0u64;
        for (bytes,broken,fragment) in history {
            let before=used;
            let mut input=Fragmented { input: Cursor::new(bytes.clone()), fragment };
            let result=if broken {
                jade_opc::package_limits::read_part((&mut input).chain(Broken),limits,&mut used)
            } else {jade_opc::package_limits::read_part(&mut input,limits,&mut used)};
            let fits=!broken && bytes.len() as u64<=part && before+bytes.len() as u64<=total;
            prop_assert_eq!(result.is_ok(),fits);
            if fits {prop_assert_eq!(result.unwrap(),bytes);prop_assert_eq!(used,before+input.position());}
            else {prop_assert_eq!(used,before);}
            prop_assert!(input.position()<=part.min(total-before).saturating_add(1));
            prop_assert!(used<=total);
        }
    }
}
struct Broken;
impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("injected inflate/read failure"))
    }
}

struct Fragmented {
    input: Cursor<Vec<u8>>,
    fragment: usize,
}
impl Fragmented {
    fn position(&self) -> u64 {
        self.input.position()
    }
}
impl Read for Fragmented {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = output.len().min(self.fragment);
        self.input.read(&mut output[..count])
    }
}
