//! The `jade-collab` binary: serve files, then edit and read them from the
//! command line as an agent would.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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

    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let _server = Serve(
        bin()
            .args(["serve", "--addr", &addr])
            .arg(dir.join("hello.docx"))
            .arg(dir.join("book.xlsx"))
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(&addr).is_err() {
        assert!(Instant::now() < deadline, "server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }

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
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let server = Serve(
        bin()
            .args(["serve", "--addr", &addr])
            .args(args)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(&addr).is_err() {
        assert!(Instant::now() < deadline, "server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
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

    let (server, addr) = serve(&["--data".as_ref(), data.as_os_str(), file.as_os_str()]);
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

#[test]
fn decks_are_served_and_edited_from_the_command_line() {
    let dir = std::env::temp_dir().join(format!("jade-collab-cli-pptx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    std::fs::copy(core.join("public/fixtures/sample-deck.pptx"), dir.join("deck.pptx")).unwrap();
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let _server = Serve(
        bin()
            .args(["serve", "--addr", &addr])
            .arg(dir.join("deck.pptx"))
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(&addr).is_err() {
        assert!(Instant::now() < deadline, "server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
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
