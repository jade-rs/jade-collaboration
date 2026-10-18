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

fn serve(args: &[&std::ffi::OsStr]) -> (Serve, String) {
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
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(addr) = line.strip_prefix("listening on ") {
                let _ = found.send(addr.to_string());
            }
        }
    });
    let addr = addr
        .recv_timeout(Duration::from_secs(60))
        .expect("server did not start");
    (server, addr)
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
