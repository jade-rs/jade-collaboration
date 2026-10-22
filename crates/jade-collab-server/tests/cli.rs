//! The `jade-collab` binary: serve files, then edit and read them from the
//! command line as an agent would.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jade-collab"))
}

struct Serve(Child);

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(args: &[&str]) -> String {
    let output = bin().args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn serve_apply_and_show() {
    let dir = std::env::temp_dir().join(format!("jade-collab-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    std::fs::copy(core.join("hello.docx"), dir.join("hello.docx")).unwrap();
    std::fs::copy(
        core.join("public/fixtures/grid-operations-workbook.xlsx"),
        dir.join("book.xlsx"),
    )
    .unwrap();

    let (_server, addr) = serve(&[
        dir.join("hello.docx").as_os_str(),
        dir.join("book.xlsx").as_os_str(),
    ]);

    let docx = format!("ws://{addr}/documents/hello.docx/ws");
    run(&[
        "apply",
        &docx,
        r#"{"type":"insert_text","paragraph":0,"offset":0,"text":"CLI: "}"#,
    ]);
    let shown = run(&["show", &docx]);
    assert!(shown.contains("CLI: Hello"), "{shown}");

    let xlsx = format!("ws://{addr}/documents/book.xlsx/ws");
    run(&[
        "apply",
        &xlsx,
        r#"{"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"number","value":42}}"#,
    ]);
    let shown = run(&["show", &xlsx]);
    assert!(shown.contains("\"E6\""), "{shown}");
    assert!(shown.contains("42"), "{shown}");

    // A rejected edit is reported and fails the command.
    let output = bin()
        .args([
            "apply",
            &xlsx,
            r#"{"type":"set_cell","sheet":"Nope","reference":"A1","value":{"kind":"blank"}}"#,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn locally_invalid_edit_arrays_never_submit_a_valid_prefix() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    for (source, target) in [
        ("hello.docx", "hello.docx"),
        ("public/fixtures/grid-operations-workbook.xlsx", "book.xlsx"),
        ("public/fixtures/sample-deck.pptx", "deck.pptx"),
    ] {
        std::fs::copy(core.join(source), dir.path().join(target)).unwrap();
    }
    let (_server, addr) = serve(&[
        dir.path().join("hello.docx").as_os_str(),
        dir.path().join("book.xlsx").as_os_str(),
        dir.path().join("deck.pptx").as_os_str(),
    ]);
    for (name, valid, invalid) in [
        (
            "hello.docx",
            json!({"type":"insert_text","paragraph":0,"offset":0,"text":"計画🙂"}),
            json!({"type":"insert_text","paragraph":99999,"offset":0,"text":"invalid"}),
        ),
        (
            "book.xlsx",
            json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"text","value":"planned"}}),
            json!({"type":"set_cell","sheet":"missing","reference":"A1","value":{"kind":"blank"}}),
        ),
        (
            "deck.pptx",
            json!({"type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"計画🙂"}),
            json!({"type":"delete_slide","slide":99999}),
        ),
        (
            "hello.docx",
            json!({"type":"insert_text","paragraph":0,"offset":0,"text":"valid prefix"}),
            json!({"type":"insert_text","paragraph":0,"offset":0,"text":"bad\0text"}),
        ),
        (
            "book.xlsx",
            json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"text","value":"valid prefix"}}),
            json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"formula","value":"="}}),
        ),
    ] {
        let url = format!("ws://{addr}/documents/{name}/ws");
        let before = run(&["show", &url]);
        let mut edits = vec![valid; 24];
        edits.push(invalid);
        let output = bin()
            .args(["apply", &url, &serde_json::to_string(&edits).unwrap()])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{name}: invalid planning must fail"
        );
        assert_eq!(
            run(&["show", &url]),
            before,
            "{name}: locally invalid input must submit nothing"
        );
        // Later edits address the structure created by earlier edits in
        // the same plan. A successful local plan must still reach the host.
        let valid = match name {
            "hello.docx" => json!([
                {"type":"insert_paragraph_break","paragraph":0,"offset":0},
                {"type":"insert_text","paragraph":1,"offset":0,"text":"計画🙂"}
            ]),
            "book.xlsx" => json!([
                {"type":"insert_row","sheet":"Operations","before":6},
                {"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"text","value":"planned"}}
            ]),
            _ => json!([
                {"type":"duplicate_slide","slide":1},
                {"type":"insert_text","slide":2,"target":"notes","paragraph":0,"offset":0,"text":"計画🙂"}
            ]),
        };
        let output = run(&["apply", &url, &serde_json::to_string(&valid).unwrap()]);
        let shown = run(&["show", &url]);
        assert_eq!(
            output, shown,
            "{name}: the planned result reaches the server"
        );
        assert_ne!(shown, before);
    }
}

#[test]
fn deleting_a_tracked_insertion_in_the_same_cli_array_removes_ones_own_text() {
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    let (_server, addr) = serve(&[core.join("hello.docx").as_os_str()]);
    let url = format!("ws://{addr}/documents/hello.docx/ws");
    let before = run(&["show", &url]);
    let result = run(&[
        "apply",
        &url,
        r#"[
        {"type":"set_tracking","on":true},
        {"type":"insert_text","paragraph":0,"offset":0,"text":"自分🙂"},
        {"type":"delete_text","paragraph":0,"offset":0,"direction":"forward","count":3}
    ]"#,
    ]);
    assert_eq!(
        result, before,
        "one's own tracked insertion is removed, rather than retained as deleted text"
    );
    assert_eq!(run(&["show", &url]), before);
}

fn serve(args: &[&std::ffi::OsStr]) -> (Serve, String) {
    let (server, addr, _) = serve_announcements(args);
    (server, addr)
}

fn serve_announcements(args: &[&std::ffi::OsStr]) -> (Serve, String, Vec<String>) {
    let mut child = bin()
        .args(["serve", "--addr", "127.0.0.1:0"])
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // The server prints `listening on <addr>` once every document is open.
    let stdout = child.stdout.take().unwrap();
    let server = Serve(child);
    let (found, addr) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut urls = Vec::new();
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(serving) = line.strip_prefix("serving ") {
                urls.push(serving.split_whitespace().next().unwrap().to_string());
            }
            if let Some(addr) = line.strip_prefix("listening on ") {
                let _ = found.send((addr.to_string(), std::mem::take(&mut urls)));
            }
        }
    });
    let (addr, urls) = addr
        .recv_timeout(Duration::from_secs(60))
        .expect("server did not start");
    (server, addr, urls)
}

#[test]
fn advertised_unicode_urls_edit_and_survive_a_process_restart() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    let cases = [
        (
            "日本 #?%.docx",
            "hello.docx",
            json!({"type":"insert_text","paragraph":1,"offset":0,"text":"名前"}),
        ),
        (
            "Budget +税.xlsx",
            "public/fixtures/grid-operations-workbook.xlsx",
            json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"number","value":17}}),
        ),
        (
            "literal%2F #.pptx",
            "public/fixtures/sample-deck.pptx",
            json!({"type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"名前"}),
        ),
    ];
    let paths: Vec<_> = cases
        .iter()
        .map(|(name, source, _)| {
            let path = dir.path().join(name);
            std::fs::copy(core.join(source), &path).unwrap();
            path
        })
        .collect();
    let mut args = vec!["--data".as_ref(), data.as_os_str()];
    args.extend(paths.iter().map(|p| p.as_os_str()));
    let (server, _, urls) = serve_announcements(&args);
    assert_eq!(urls.len(), cases.len());
    let mut expected = std::collections::BTreeMap::new();
    for (name, _, edit) in &cases {
        let url = urls
            .iter()
            .find(|url| jade_collab_server::document_name_from_url(url).as_deref() == Some(name))
            .unwrap();
        assert!(url.is_ascii());
        run(&["apply", url, &edit.to_string()]);
        let shown = run(&["show", url]);
        if name.ends_with("xlsx") {
            assert!(shown.contains("17"), "{shown}");
        } else {
            assert!(shown.contains("名前"), "{shown}");
        }
        expected.insert(*name, shown);
    }
    drop(server);
    let (_server, _, urls) = serve_announcements(&["--data".as_ref(), data.as_os_str()]);
    assert_eq!(urls.len(), cases.len());
    for url in urls {
        let name = jade_collab_server::document_name_from_url(&url).unwrap();
        assert_eq!(run(&["show", &url]), expected[name.as_str()]);
    }
}

#[test]
fn two_processes_cannot_own_the_same_data_and_a_kill_releases_ownership() {
    use std::io::Read;
    use std::time::Instant;

    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx");
    let (first, _) = serve(&["--data".as_ref(), data.as_os_str(), file.as_os_str()]);
    let mut second = Serve(
        bin()
            .args(["serve", "--addr", "127.0.0.1:0", "--data"])
            .arg(&data)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = second.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the competing server must fail without waiting for the owner"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut message = String::new();
    second
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut message)
        .unwrap();
    assert!(!status.success());
    assert!(message.contains("already in use"), "{message}");
    drop(first); // kills and reaps the process, without unlinking its lease file
    assert!(data.join(".lock").exists());
    let (_third, addr) = serve(&["--data".as_ref(), data.as_os_str()]);
    assert!(run(&["show", &format!("ws://{addr}/documents/hello.docx/ws")]).contains("Hello"));
}

#[test]
fn serve_with_a_data_directory_survives_a_kill() {
    let dir = std::env::temp_dir().join(format!("jade-collab-cli-data-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    let file = dir.join("hello.docx");
    std::fs::copy(core.join("hello.docx"), &file).unwrap();
    let data = dir.join("data");

    let (server, addr) = serve(&[
        "--data".as_ref(),
        data.as_os_str(),
        "--fsync".as_ref(),
        "every".as_ref(),
        "--keep-tail".as_ref(),
        "10".as_ref(),
        file.as_os_str(),
    ]);
    let docx = format!("ws://{addr}/documents/hello.docx/ws");
    run(&[
        "apply",
        &docx,
        r#"{"type":"insert_text","paragraph":0,"offset":0,"text":"Durable: "}"#,
    ]);
    drop(server); // killed, never saved

    // The stored document wins over the file it was first opened from.
    let (_server, addr) = serve(&["--data".as_ref(), data.as_os_str(), file.as_os_str()]);
    let docx = format!("ws://{addr}/documents/hello.docx/ws");
    let shown = run(&["show", &docx]);
    assert!(shown.contains("Durable: Hello"), "{shown}");
    // Only `every` and `checkpoint` are policies.
    let refused = bin()
        .args(["serve", "--data"])
        .arg(&data)
        .args(["--fsync", "sometimes"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn serve_with_tokens() {
    let dir = std::env::temp_dir().join(format!("jade-collab-cli-tokens-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    let file = dir.join("hello.docx");
    std::fs::copy(core.join("hello.docx"), &file).unwrap();
    let tokens = dir.join("tokens.json");
    std::fs::write(
        &tokens,
        r#"{"ed": {"name": "Ed", "role": "editor"}, "vi": {"name": "Vi", "role": "viewer"}}"#,
    )
    .unwrap();

    let (_server, addr) = serve(&["--tokens".as_ref(), tokens.as_os_str(), file.as_os_str()]);
    let docx = format!("ws://{addr}/documents/hello.docx/ws");
    let edit = r#"{"type":"insert_text","paragraph":0,"offset":0,"text":"Signed: "}"#;
    let refused = bin().args(["show", &docx]).output().unwrap();
    assert!(!refused.status.success(), "no token");
    let refused = bin()
        .args(["apply", "--token", "vi", &docx, edit])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "a viewer cannot edit");
    run(&["apply", "--token", "ed", &docx, edit]);
    let shown = run(&["show", "--token", "vi", &docx]);
    assert!(shown.contains("Signed: Hello"), "{shown}");
    assert!(!shown.contains("Signed: Signed:"), "{shown}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Retries `show` with `token` until it succeeds or fails as `succeeds`.
fn until_show(docx: &str, token: &str, succeeds: bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let output = bin()
            .args(["show", "--token", token, docx])
            .output()
            .unwrap();
        if output.status.success() == succeeds {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "show with {token} never {}",
            if succeeds { "succeeded" } else { "failed" }
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn tokens_rotate_without_a_restart() {
    let dir = std::env::temp_dir().join(format!("jade-collab-cli-rotate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    let file = dir.join("hello.docx");
    std::fs::copy(core.join("hello.docx"), &file).unwrap();
    let tokens = dir.join("tokens.json");
    std::fs::write(&tokens, r#"{"one": {"name": "One", "role": "viewer"}}"#).unwrap();

    // Without polling, the file is read again on SIGHUP.
    let (server, addr) = serve(&[
        "--tokens".as_ref(),
        tokens.as_os_str(),
        "--tokens-poll".as_ref(),
        "0".as_ref(),
        file.as_os_str(),
    ]);
    let docx = format!("ws://{addr}/documents/hello.docx/ws");
    until_show(&docx, "one", true);
    std::fs::write(&tokens, r#"{"two": {"name": "Two", "role": "viewer"}}"#).unwrap();
    let hup = Command::new("kill")
        .args(["-HUP", &server.0.id().to_string()])
        .status()
        .unwrap();
    assert!(hup.success());
    until_show(&docx, "one", false);
    until_show(&docx, "two", true);
    drop(server);

    // By default the file is watched.
    let (_server, addr) = serve(&["--tokens".as_ref(), tokens.as_os_str(), file.as_os_str()]);
    let docx = format!("ws://{addr}/documents/hello.docx/ws");
    until_show(&docx, "two", true);
    std::fs::write(&tokens, r#"{"three": {"name": "Three", "role": "viewer"}}"#).unwrap();
    until_show(&docx, "two", false);
    until_show(&docx, "three", true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn decks_are_served_and_edited_from_the_command_line() {
    let dir = std::env::temp_dir().join(format!("jade-collab-cli-pptx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    std::fs::copy(
        core.join("public/fixtures/sample-deck.pptx"),
        dir.join("deck.pptx"),
    )
    .unwrap();
    let (_server, addr) = serve(&[dir.join("deck.pptx").as_os_str()]);
    let deck = format!("ws://{addr}/documents/deck.pptx/ws");
    let before = run(&["show", &deck]);
    assert!(before.contains("\"format\": \"pptx\""), "{before}");
    run(&[
        "apply",
        &deck,
        r#"[{"type":"insert_slide","at":1},{"type":"insert_text","slide":2,"target":"notes","paragraph":0,"offset":0,"text":"CLI note"}]"#,
    ]);
    let shown = run(&["show", &deck]);
    assert!(shown.contains("CLI note"), "{shown}");
}

#[test]
fn serve_with_an_audit_log_and_an_outgoing_limit() {
    let dir = tempfile::tempdir().unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    std::fs::copy(core.join("hello.docx"), dir.path().join("hello.docx")).unwrap();
    let log = dir.path().join("audit.jsonl");
    let (_server, addr) = serve(&[
        "--audit-log".as_ref(),
        log.as_os_str(),
        "--outgoing-limit".as_ref(),
        "64".as_ref(),
        dir.path().join("hello.docx").as_os_str(),
    ]);
    let docx = format!("ws://{addr}/documents/hello.docx/ws");
    run(&["show", &docx]);
    // `show` is done once it has the document, which can be before the
    // server has written the join down.
    let mut text = String::new();
    for _ in 0..100 {
        text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains(r#""event":"joined""#) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(text.contains(r#""event":"connected""#), "{text}");
    assert!(text.contains(r#""event":"joined""#), "{text}");
}

#[test]
fn serve_applies_upload_limits_to_actual_chunked_http_requests() {
    use std::io::{Read, Write};
    fn put(addr: &str, name: &str, bytes: &[u8]) -> u16 {
        let mut request = format!("PUT /documents/{name} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n").into_bytes();
        for chunk in bytes.chunks(127) {
            request.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            request.extend_from_slice(chunk);
            request.extend_from_slice(b"\r\n");
        }
        request.extend_from_slice(b"0\r\n\r\n");
        let mut socket = std::net::TcpStream::connect(addr).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        socket.write_all(&request).unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        response.split_whitespace().nth(1).unwrap().parse().unwrap()
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core/hello.docx");
    let bytes = std::fs::read(&path).unwrap();
    let limit = bytes.len().to_string();
    let (_server, addr) = serve(&["--upload-limit".as_ref(), limit.as_ref(), path.as_os_str()]);
    let mut extra = bytes.clone();
    extra.push(0);
    assert_eq!(put(&addr, "new.docx", &extra), 413);
    assert_eq!(put(&addr, "new.docx", &bytes), 201);
    let uploaded = format!("ws://{addr}/documents/new.docx/ws");
    assert!(run(&["show", &uploaded]).contains("Hello"));
    assert_eq!(put(&addr, "new.docx", &bytes), 409);
    let (_zero, addr) = serve(&["--upload-limit".as_ref(), "0".as_ref(), path.as_os_str()]);
    let local = format!("ws://{addr}/documents/hello.docx/ws");
    assert!(
        run(&["show", &local]).contains("Hello"),
        "local files are independent of the HTTP limit"
    );
    assert_eq!(put(&addr, "none.docx", &bytes), 413);
}

#[cfg(unix)]
#[test]
fn the_audit_log_is_reopened_on_sighup() {
    let dir = tempfile::tempdir().unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    std::fs::copy(core.join("hello.docx"), dir.path().join("hello.docx")).unwrap();
    let log = dir.path().join("audit.jsonl");
    let rotated = dir.path().join("audit.jsonl.1");
    // No tokens: SIGHUP still reopens the audit log.
    let (server, addr) = serve(&[
        "--audit-log".as_ref(),
        log.as_os_str(),
        dir.path().join("hello.docx").as_os_str(),
    ]);
    let docx = format!("ws://{addr}/documents/hello.docx/ws");
    run(&["show", &docx]);
    std::fs::rename(&log, &rotated).unwrap();
    let hup = Command::new("kill")
        .args(["-HUP", &server.0.id().to_string()])
        .status()
        .unwrap();
    assert!(hup.success());
    // The signal is handled asynchronously: connect until the new file
    // has an entry.
    let mut reopened = false;
    for _ in 0..100 {
        run(&["show", &docx]);
        if std::fs::read_to_string(&log).is_ok_and(|text| text.contains(r#""event":"connected""#)) {
            reopened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        reopened,
        "nothing was written at the log's path after SIGHUP"
    );
    assert!(std::fs::read_to_string(&rotated)
        .unwrap()
        .contains(r#""event":"connected""#));
}
