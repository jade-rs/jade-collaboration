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
