//! Stateful token reload, roles, credential rotation and durable author labels.
use jade_collab_server::{Client, Role, Server, StorageOptions, Tokens};
use jade_collab_sync::{Rejection, ReplicaEvent};
use jade_docx_collab::{DocxModel, DocxOp, JadeDocx, StoryId};
use proptest::prelude::*;
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Duration};

fn table(key: &str, name: &str, editor: bool) -> String {
    json!({key:{"name":name,"role":if editor {"editor"} else {"viewer"}}}).to_string()
}
fn letters(text: impl Iterator<Item = char>) -> BTreeMap<char, usize> {
    let mut result = BTreeMap::new();
    for ch in text {
        *result.entry(ch).or_default() += 1;
    }
    result
}
fn check_authors(model: &DocxModel, name: &str, expected: &str) {
    let revisions = model.revisions();
    for revision in &revisions {
        assert_eq!(revision.revision.author.as_deref(), Some(name));
    }
    assert_eq!(
        letters(revisions.iter().flat_map(|revision| revision.text.chars())),
        letters(expected.chars())
    );
}
async fn listen(server: &Server) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/documents/authors.docx/ws",
        listener.local_addr().unwrap()
    );
    (url, tokio::spawn(server.clone().serve(listener)))
}
async fn storage_released(root: &Path) {
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .open(root.join(".lock"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match file.try_lock() {
                Ok(()) => {
                    file.unlock().unwrap();
                    break;
                }
                Err(std::fs::TryLockError::WouldBlock) => tokio::task::yield_now().await,
                Err(error) => panic!("storage lease: {error}"),
            }
        }
    })
    .await
    .unwrap();
}
async fn run(name: &str, checkpoint: bool, steps: &[(u8, u8)]) {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("tokens.json");
    let options = StorageOptions {
        checkpoint_every: if checkpoint { 1 } else { 1000 },
        ..Default::default()
    };
    let mut key_index = 0;
    let mut key = "credential-0".to_string();
    let mut editor = true;
    let mut server = Server::with_storage_options(root.path(), options)
        .unwrap()
        .with_tokens(Tokens::from_json(&table(&key, name, editor)).unwrap());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx");
    server
        .open("authors.docx", std::fs::read(fixture).unwrap())
        .unwrap();
    let (mut url, mut serving) = listen(&server).await;
    let mut client = Some(
        Client::<DocxModel>::connect(&url, Some(&key))
            .await
            .unwrap(),
    );
    let mut expected = String::new();
    let mut commits = 0;
    let mut authored = false;
    let mut edit_index = 0;
    // Every case exercises bad reload before and after author registration,
    // a viewer refusal, rotation, restart, then generated interleavings.
    for &(kind, value) in [
        (0, 0),
        (1, 0),
        (0, 4),
        (2, 0),
        (1, 0),
        (3, 0),
        (4, 0),
        (2, 0),
        (1, 0),
    ]
    .iter()
    .chain(steps)
    {
        match kind {
            0 | 5 => {
                let bad = [
                    "",
                    " \t\r\n",
                    "\u{2003}\u{3000}",
                    "bad\0name",
                    "bad\u{fffe}",
                    "bad\u{ffff}",
                    "bad\u{b}name",
                    "bad\u{1f}name",
                ][value as usize % 8];
                let input = if kind == 5 {
                    "{invalid-json".to_string()
                } else {
                    json!({"a-valid-but-not-published":{"name":"Other","role":"editor"},"broken-credential":{"name":bad,"role":if editor {"viewer"} else {"editor"}}}).to_string()
                };
                std::fs::write(&config, input).unwrap();
                let error = server
                    .reload_tokens(&config)
                    .expect_err("invalid identity table must never become active");
                assert!(!error.to_string().contains("broken-credential"));
                assert!(client.as_ref().unwrap().connection_error().is_none());
            }
            1 | 6 => {
                let client = client.as_ref().unwrap();
                client.take_events();
                let text = format!("E{edit_index:03}");
                edit_index += 1;
                client
                    .try_edit(None, |edit| {
                        let first = edit.try_reserve(text.chars().count() as u64)?;
                        edit.submit(DocxOp::TrackInsertText {
                            story: StoryId::Body,
                            parent: None,
                            first,
                            text: text.clone(),
                            date: None,
                        })?;
                        Ok::<_, Rejection>(())
                    })
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), client.settle())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(client.pending_len(), 0);
                let events = client.take_events();
                if editor {
                    assert!(events
                        .iter()
                        .any(|event| matches!(event, ReplicaEvent::Acknowledged { .. })));
                    expected.push_str(&text);
                    commits += 1 + u64::from(!authored);
                    authored = true;
                } else {
                    assert!(events
                        .iter()
                        .any(|event| matches!(event, ReplicaEvent::Rejected { .. })));
                }
            }
            2 => {
                editor = !editor;
                std::fs::write(&config, table(&key, name, editor)).unwrap();
                server.reload_tokens(&config).unwrap();
            }
            3 => {
                let replica = client.take().unwrap().disconnect().await;
                key_index += 1;
                key = format!("credential-{key_index}");
                std::fs::write(&config, table(&key, name, editor)).unwrap();
                server.reload_tokens(&config).unwrap();
                let resumed = Client::resume(&url, &replica, Some(&key)).await.unwrap();
                assert_eq!(resumed.id(), replica.id());
                client = Some(resumed);
            }
            _ => {
                let replica = client.take().unwrap().disconnect().await;
                serving.abort();
                let _ = serving.await;
                drop(server);
                storage_released(root.path()).await;
                server = Server::with_storage_options(root.path(), options)
                    .unwrap()
                    .with_tokens(Tokens::from_json(&table(&key, name, editor)).unwrap());
                (url, serving) = listen(&server).await;
                client = Some(Client::resume(&url, &replica, Some(&key)).await.unwrap());
            }
        }
        assert_eq!(server.seq("authors.docx").await.unwrap(), commits);
        let client = client.as_ref().unwrap();
        check_authors(&client.view(), name, &expected);
        let exported = server.export("authors.docx").await.unwrap();
        let jade = JadeDocx::load(&exported).unwrap();
        let native = DocxModel::from_word_document(jade.word()).unwrap();
        check_authors(&native, name, &expected);
        assert_eq!(native.paragraph_texts(), client.view().paragraph_texts());
        assert!(server.faults("authors.docx").await.unwrap().is_empty());
    }
    client.take().unwrap().disconnect().await;
    serving.abort();
    let _ = serving.await;
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn stateful_author_labels_survive_bad_reload_roles_rekey_and_restart(
        name in prop::sample::select(vec!["Ada", "研究者🙂 & < > \\\" '", " A\tB\nC\rD ", "A\u{85}\u{2028}\u{fdd0}\u{1fffe}", "Literal &amp; &#9; &custom;"]),
        checkpoint in any::<bool>(),
        steps in prop::collection::vec((0u8..7,0u8..8),1..30),
    ) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(run(name,checkpoint,&steps));
    }
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn token_author_validation_matches_the_scalar_and_nonblank_reference(chars in prop::collection::vec(prop_oneof![
        2 => prop::sample::select(vec!['\0','\u{b}','\u{1f}','\u{fffe}','\u{ffff}']),
        2 => prop::sample::select(vec![' ','\t','\r','\n','\u{2003}','\u{3000}']),
        6 => any::<char>()
    ],0..60)) {
        let name:String=chars.into_iter().collect();
        let scalar=|c:char| {let n=c as u32; [9,10,13].contains(&n)||(32..=0xd7ff).contains(&n)||(0xe000..=0xfffd).contains(&n)||(0x10000..=0x10ffff).contains(&n)};
        let allowed=!name.trim().is_empty()&&name.chars().all(scalar);
        for editor in [true,false] {
            let result=Tokens::from_json(&table("private-credential",&name,editor));
            prop_assert_eq!(result.is_ok(),allowed);
            if let Ok(tokens)=result {
                let grant=tokens.grant("private-credential").unwrap();
                prop_assert_eq!(&grant.name,&name,"names must never be trimmed or normalized");
                prop_assert_eq!(grant.role,if editor {Role::Editor} else {Role::Viewer});
            }
        }
    }
}
