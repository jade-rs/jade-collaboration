#[path = "common/tls.rs"]
mod support;
use jade_collab_server::{Server, Tokens};
use serde_json::json;
use support::{fixture, Proxy};

async fn cli(args: &[&str], ca: &std::path::Path) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_jade-collab"));
    command
        .args(args)
        .env("SSL_CERT_FILE", ca)
        .env_remove("JADE_COLLAB_TOKEN")
        .kill_on_drop(true);
    tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
        .await
        .expect("CLI network operation is bounded")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_sdk_edits_all_formats_over_verified_wss_and_rejects_bad_certificates() {
    let server = Server::new()
        .with_tokens(Tokens::from_json(r#"{"editor":{"name":"Ada","role":"editor"}}"#).unwrap());
    let cases = [
        (
            "日本 #?.docx",
            "hello.docx",
            json!({"type":"insert_text","paragraph":1,"offset":0,"text":"TLS"}),
        ),
        (
            "Budget +税.xlsx",
            "public/fixtures/grid-operations-workbook.xlsx",
            json!({"type":"set_cell","sheet":"Operations","reference":"E6","value":{"kind":"number","value":17}}),
        ),
        (
            "Deck %.pptx",
            "public/fixtures/sample-deck.pptx",
            json!({"type":"insert_text","slide":1,"target":"notes","paragraph":0,"offset":0,"text":"TLS"}),
        ),
    ];
    for (name, source, _) in &cases {
        server.open(name, fixture(source)).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = listener.local_addr().unwrap();
    let serving = tokio::spawn(server.clone().serve(listener));
    let proxy = Proxy::start(upstream).await;
    for (name, _, edit) in cases {
        let url = proxy.socket(name, true);
        let result = cli(
            &["apply", "--token", "editor", &url, &edit.to_string()],
            &proxy.ca,
        )
        .await;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(server.seq(name).await.unwrap(), 1);
        let result = cli(&["show", &format!("{url}?token=editor")], &proxy.ca).await;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let shown = String::from_utf8(result.stdout).unwrap();
        assert!(shown.contains(if name.ends_with("xlsx") { "17" } else { "TLS" }));
        for (url, ca) in [
            (proxy.socket(name, false), &proxy.ca),
            (proxy.socket(name, true), &proxy.wrong_ca),
        ] {
            let result = cli(&["apply", "--token", "editor", &url, &edit.to_string()], ca).await;
            assert!(
                !result.status.success(),
                "a wrong host or untrusted key was accepted"
            );
            assert_eq!(
                server.seq(name).await.unwrap(),
                1,
                "failed verification cannot send edits"
            );
        }
        assert!(server.faults(name).await.unwrap().is_empty());
    }
    let secret = "query-credential-must-stay-private";
    let invalid = format!("{}?token={secret}", proxy.socket("unknown.txt", true));
    let failure = cli(&["show", &invalid], &proxy.ca).await;
    assert!(!failure.status.success());
    assert!(!String::from_utf8_lossy(&failure.stderr).contains(secret));
    proxy.idle().await;
    serving.abort();
    let _ = serving.await;
}
