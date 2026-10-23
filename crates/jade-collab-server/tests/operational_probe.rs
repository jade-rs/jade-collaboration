//! Real process, concurrent clients, forced termination and offline backup restore.
//! The short lifecycle check runs in CI; the configurable load probe is opt-in.
use jade_collab_server::Client;
use jade_collab_sync::{Document, Replica, ReplicaEvent};
use jade_docx_collab::{intent::IntentOp, DocxModel, JadeDocx};
use jade_pptx_collab::{intent::PptxIntent, PptxModel};
use jade_xlsx_collab::{intent::XlsxIntent, CellInput, JadeXlsx, XlsxModel};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::Barrier, task::JoinHandle};

const TOKEN: &str = "local-probe-editor";
const DEADLINE: Duration = Duration::from_secs(30);

struct Process(Child);
impl Process {
    fn kill(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn ready(base: &str) -> Result<Client<DocxModel>, jade_collab_server::ClientError> {
    Client::<DocxModel>::connect_with_timeout(
        &format!("{base}/documents/probe.docx/ws"),
        Some(TOKEN),
        DEADLINE,
    )
    .await
}

#[tokio::test]
async fn readiness_allows_a_snapshot_taking_more_than_one_second() {
    use futures_util::{SinkExt, StreamExt};
    use jade_collab_core::id::ReplicaId;
    use jade_collab_server::wire::{Format, ServerMessage};
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        socket
            .send(Message::Text(
                ServerMessage::Welcome {
                    format: Format::Docx,
                    replica: ReplicaId(1),
                    seq: 0,
                    snapshot: serde_json::to_value(DocxModel::from_paragraphs(&["snapshot"]))
                        .unwrap(),
                }
                .to_json()
                .into(),
            ))
            .await
            .unwrap();
        let _ = socket.next().await;
    });
    let client = tokio::time::timeout(DEADLINE, ready(&base))
        .await
        .unwrap()
        .expect("a completed sync inside the total deadline is ready");
    client.disconnect().await;
    peer.await.unwrap();
}

async fn start(root: &Path, data: &Path, first: bool, stage: &str) -> (Process, String) {
    let log = root.join(format!("{stage}.log"));
    let output = fs::File::create(&log).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_jade-collab"));
    command
        .args(["serve", "--addr", "127.0.0.1:0", "--data"])
        .arg(data)
        .args(["--fsync", "every", "--checkpoint-every", "101", "--tokens"])
        .arg(root.join("tokens.json"))
        .args(["--tokens-poll", "0", "--audit-log"])
        .arg(root.join("audit.jsonl"))
        .stdout(output.try_clone().unwrap())
        .stderr(output);
    if first {
        for name in ["probe.docx", "probe.xlsx", "probe.pptx"] {
            command.arg(root.join(name));
        }
    }
    let mut process = Process(command.spawn().unwrap());
    let base = tokio::time::timeout(DEADLINE, async {
        loop {
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "server exited: {}",
                fs::read_to_string(&log).unwrap()
            );
            let text = fs::read_to_string(&log).unwrap();
            if text
                .lines()
                .filter(|line| line.starts_with("serving ws://"))
                .count()
                == 3
            {
                let url = text
                    .split_whitespace()
                    .find(|s| s.starts_with("ws://"))
                    .unwrap();
                let base = url.split("/documents/").next().unwrap().to_owned();
                // Printed paths alone are not readiness: require authenticated sync.
                if let Ok(client) = ready(&base).await {
                    client.disconnect().await;
                    break base;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "server startup deadline at {stage}: {error}; log {}: {}",
            log.display(),
            fs::read_to_string(&log).unwrap()
        )
    });
    (process, base)
}

fn download(base: &str, name: &str) -> Vec<u8> {
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "30",
            "--header",
        ])
        .arg(format!("Authorization: Bearer {TOKEN}"))
        .arg(format!(
            "{}/documents/{name}",
            base.replacen("ws://", "http://", 1)
        ))
        .output()
        .expect("the operational probe needs curl");
    assert!(
        output.status.success(),
        "download: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn docx(client: &Client<DocxModel>, worker: usize, n: u64) {
    let intent = IntentOp::InsertText {
        paragraph: 0,
        offset: 0,
        text: format!("[d{worker}:{n}]字🙂"),
    };
    let ops =
        jade_docx_collab::intent::resolve(&client.view(), &intent, &mut |n| client.reserve(n))
            .unwrap();
    for op in ops {
        client.submit(op).unwrap();
    }
}
fn xlsx(client: &Client<XlsxModel>, worker: usize, n: u64) {
    let intent = XlsxIntent::SetCell {
        sheet: "Operations".into(),
        reference: format!("E{}", worker + 6),
        value: CellInput::Number(n as f64),
    };
    let op = jade_xlsx_collab::intent::resolve(&client.view(), &intent, &mut |n| client.reserve(n))
        .unwrap();
    client.submit(op).unwrap();
}
fn pptx(client: &Client<PptxModel>, worker: usize, n: u64) {
    let intent = PptxIntent::InsertText {
        slide: 1,
        target: jade_pptx_collab::intent::Target::Notes,
        paragraph: 0,
        offset: 0,
        text: format!("[p{worker}:{n}]字🙂"),
    };
    for op in jade_pptx_collab::intent::resolve(&client.view(), &intent, &mut |n| client.reserve(n))
        .unwrap()
    {
        client.submit(op).unwrap();
    }
}

struct Worker<D: Document> {
    client: Client<D>,
    latencies_ms: Vec<f64>,
}
async fn workers<D>(
    base: &str,
    name: &str,
    count: usize,
    barrier: Arc<Barrier>,
    duration: Duration,
    pace: Duration,
    edit: fn(&Client<D>, usize, u64),
) -> Vec<JoinHandle<Worker<D>>>
where
    D: Document + Serialize + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let mut tasks = Vec::new();
    for worker in 0..count {
        let client = Client::<D>::connect(&format!("{base}/documents/{name}/ws"), Some(TOKEN))
            .await
            .unwrap();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            let until = Instant::now() + duration;
            let mut latencies_ms = Vec::new();
            loop {
                let begin = Instant::now();
                edit(&client, worker, latencies_ms.len() as u64 + 1);
                tokio::time::timeout(DEADLINE, client.settle())
                    .await
                    .expect("edit deadline")
                    .unwrap();
                let events = client.take_events();
                assert!(
                    !events
                        .iter()
                        .any(|e| matches!(e, ReplicaEvent::Rejected { .. })),
                    "unexpected edit refusal"
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|e| matches!(e, ReplicaEvent::Acknowledged { .. }))
                        .count(),
                    1
                );
                latencies_ms.push(begin.elapsed().as_secs_f64() * 1000.0);
                if Instant::now() >= until {
                    break;
                }
                tokio::time::sleep(pace).await;
            }
            Worker {
                client,
                latencies_ms,
            }
        }));
    }
    tasks
}

async fn freeze<D>(tasks: Vec<JoinHandle<Worker<D>>>) -> (Vec<Client<D>>, D, Value)
where
    D: Document + Serialize + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let mut clients = Vec::new();
    let mut latencies = Vec::new();
    for task in tasks {
        let worker = task.await.unwrap();
        clients.push(worker.client);
        latencies.extend(worker.latencies_ms);
    }
    let seq = clients.iter().map(Client::seq).max().unwrap();
    for client in &clients {
        tokio::time::timeout(DEADLINE, client.wait_for_seq(seq))
            .await
            .expect("catch-up deadline")
            .unwrap();
    }
    let view = clients[0].view();
    for client in &clients {
        assert_eq!(
            serde_json::to_value(client.view()).unwrap(),
            serde_json::to_value(&view).unwrap()
        );
    }
    latencies.sort_by(f64::total_cmp);
    let percentile = |p: usize| latencies[((latencies.len() * p).div_ceil(100)).saturating_sub(1)];
    let metrics = json!({"clients":clients.len(), "acknowledged_edits":latencies.len(), "seq":seq,
        "p50_ms":percentile(50), "p95_ms":percentile(95), "p99_ms":percentile(99), "max_ms":latencies.last()});
    (clients, view, metrics)
}

async fn disconnected<D>(clients: Vec<Client<D>>) -> Vec<Replica<D>>
where
    D: Document + Serialize + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let mut replicas = Vec::new();
    for client in clients {
        replicas.push(client.disconnect().await);
    }
    replicas
}
async fn resumed<D>(base: &str, name: &str, replicas: &[Replica<D>], expected: &D)
where
    D: Document + Serialize + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    for replica in replicas {
        let client = Client::resume(&format!("{base}/documents/{name}/ws"), replica, Some(TOKEN))
            .await
            .unwrap();
        assert_eq!(
            client.seq(),
            replica.seq(),
            "no duplicate commits on resume"
        );
        assert_eq!(
            serde_json::to_value(client.view()).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(client.pending_len(), 0);
        client.disconnect().await;
    }
}

async fn writable<D>(
    base: &str,
    name: &str,
    replicas: &[Replica<D>],
    edit: fn(&Client<D>, usize, u64),
) where
    D: Document + Serialize + DeserializeOwned + Send + 'static,
    D::Op: Serialize + DeserializeOwned + Send + 'static,
{
    let client = Client::<D>::connect(&format!("{base}/documents/{name}/ws"), Some(TOKEN))
        .await
        .unwrap();
    assert!(
        replicas.iter().all(|r| r.id() < client.id()),
        "restored replica ids must not be reused"
    );
    let before = client.seq();
    edit(&client, 0, 999999);
    tokio::time::timeout(DEADLINE, client.settle())
        .await
        .expect("post-restore edit deadline")
        .unwrap();
    assert_eq!(client.seq(), before + 1);
    let events = client.take_events();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ReplicaEvent::Acknowledged { .. }))
            .count(),
        1
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, ReplicaEvent::Rejected { .. })));
    client.disconnect().await;
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}
fn rss_kib(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

async fn probe(count: usize, duration: Duration, pace: Duration, report_path: Option<PathBuf>) {
    let root = if let Some(parent) = std::env::var_os("JADE_PROBE_DATA_PARENT") {
        tempfile::tempdir_in(parent).unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let path = if std::env::var_os("JADE_PROBE_KEEP_DATA").is_some_and(|s| s == "1") {
        let path = root.keep();
        println!("PROBE_DATA_DIR {}", path.display());
        path
    } else {
        root.path().to_owned()
    };
    let root = path.as_path();
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../jade-core");
    for (source, name) in [
        ("hello.docx", "probe.docx"),
        (
            "public/fixtures/grid-operations-workbook.xlsx",
            "probe.xlsx",
        ),
        ("public/fixtures/sample-deck.pptx", "probe.pptx"),
    ] {
        fs::copy(core.join(source), root.join(name)).unwrap();
    }
    fs::write(
        root.join("tokens.json"),
        json!({(TOKEN):{"name":"probe","role":"editor"}}).to_string(),
    )
    .unwrap();
    let data = root.join("data");
    let (mut process, base) = start(root, &data, true, "initial").await;
    let pid = process.0.id();
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let memory = tokio::spawn(async move {
        let mut peak = rss_kib(pid);
        tokio::pin!(stopping);
        loop {
            tokio::select! {
                _ = &mut stopping => break,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    if let Some(rss) = rss_kib(pid) { peak = Some(peak.unwrap_or(0).max(rss)); }
                }
            }
        }
        peak
    });
    let barrier = Arc::new(Barrier::new(count * 3 + 1));
    let a = workers(
        &base,
        "probe.docx",
        count,
        barrier.clone(),
        duration,
        pace,
        docx,
    )
    .await;
    let b = workers(
        &base,
        "probe.xlsx",
        count,
        barrier.clone(),
        duration,
        pace,
        xlsx,
    )
    .await;
    let c = workers(
        &base,
        "probe.pptx",
        count,
        barrier.clone(),
        duration,
        pace,
        pptx,
    )
    .await;
    let begin = Instant::now();
    barrier.wait().await;
    let ((a, doc, doc_metrics), (b, book, book_metrics), (c, deck, deck_metrics)) =
        tokio::join!(freeze(a), freeze(b), freeze(c));
    let elapsed = begin.elapsed().as_secs_f64();
    let _ = stop.send(());
    let peak_rss = memory.await.unwrap();
    let bytes = [
        download(&base, "probe.docx"),
        download(&base, "probe.xlsx"),
        download(&base, "probe.pptx"),
    ];
    // Native files, not only replicas, must contain the acknowledged text/values.
    assert_eq!(
        DocxModel::from_word_document(JadeDocx::load(&bytes[0]).unwrap().word())
            .unwrap()
            .paragraph_texts(),
        doc.paragraph_texts()
    );
    let native = JadeXlsx::open(&bytes[1]).unwrap();
    let imported = XlsxModel::from_workbook(native.workbook(), Default::default()).unwrap();
    for sheet in book.sheets() {
        let other = imported
            .sheets()
            .into_iter()
            .find(|s| s.name == sheet.name)
            .unwrap();
        assert_eq!(book.filled_cells(sheet.id), imported.filled_cells(other.id));
    }
    assert_eq!(
        PptxModel::from_pptx(&bytes[2])
            .unwrap()
            .slides()
            .iter()
            .map(|s| &s.notes)
            .collect::<Vec<_>>(),
        deck.slides().iter().map(|s| &s.notes).collect::<Vec<_>>()
    );
    if let Some(path) = &report_path {
        // Retain the measurements even if a later recovery assertion fails.
        let load = json!({"status":"load_completed_recovery_pending", "clients_per_format":count,
            "requested_seconds":duration.as_secs_f64(), "elapsed_seconds":elapsed, "fsync":"every",
            "checkpoint_every":101, "data_parent":root.parent(), "peak_server_rss_kib":peak_rss,
            "docx":doc_metrics, "xlsx":book_metrics, "pptx":deck_metrics});
        fs::write(path, serde_json::to_vec_pretty(&load).unwrap()).unwrap();
    }
    process.kill(); // SIGKILL/TerminateProcess: no graceful save or final checkpoint.
    let a = disconnected(a).await;
    let b = disconnected(b).await;
    let c = disconnected(c).await;
    let restore = Instant::now();
    let (mut recovered, base) = start(root, &data, false, "recovered").await;
    let crash_recovery_ms = restore.elapsed().as_secs_f64() * 1000.0;
    resumed(&base, "probe.docx", &a, &doc).await;
    resumed(&base, "probe.xlsx", &b, &book).await;
    resumed(&base, "probe.pptx", &c, &deck).await;
    for (name, expected) in ["probe.docx", "probe.xlsx", "probe.pptx"]
        .into_iter()
        .zip(&bytes)
    {
        assert_eq!(
            &download(&base, name),
            expected,
            "crash recovery export: {name}"
        );
    }
    recovered.kill();
    // Copy only with the writer stopped: the entire storage tree is one backup.
    let backup = root.join("backup");
    copy_tree(&data, &backup);
    let restored_data = root.join("restored");
    copy_tree(&backup, &restored_data);
    let restore = Instant::now();
    let (mut restored, base) = start(root, &restored_data, false, "restored").await;
    let backup_restore_ms = restore.elapsed().as_secs_f64() * 1000.0;
    resumed(&base, "probe.docx", &a, &doc).await;
    resumed(&base, "probe.xlsx", &b, &book).await;
    resumed(&base, "probe.pptx", &c, &deck).await;
    for (name, expected) in ["probe.docx", "probe.xlsx", "probe.pptx"]
        .into_iter()
        .zip(&bytes)
    {
        assert_eq!(
            &download(&base, name),
            expected,
            "backup restore export: {name}"
        );
    }
    writable(&base, "probe.docx", &a, docx).await;
    writable(&base, "probe.xlsx", &b, xlsx).await;
    writable(&base, "probe.pptx", &c, pptx).await;
    restored.kill();
    let report = json!({"status":"passed", "checkpoint_every":101, "clients_per_format":count, "requested_seconds":duration.as_secs_f64(), "elapsed_seconds":elapsed,
        "pace_ms":pace.as_millis(), "fsync":"every", "data_parent":root.parent(), "peak_server_rss_kib":peak_rss,
        "docx":doc_metrics, "xlsx":book_metrics, "pptx":deck_metrics, "crash_recovery_ms":crash_recovery_ms,
        "backup_restore_ms":backup_restore_ms, "acknowledged_edits_preserved":true,
        "exports_byte_identical_after_both_restores":true, "fresh_replica_ids_and_post_restore_writes":true});
    println!(
        "OPERATIONAL_PROBE {}",
        serde_json::to_string(&report).unwrap()
    );
    if let Some(path) = report_path {
        fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acknowledged_edits_survive_process_kill_and_cold_backup_restore() {
    probe(
        1,
        Duration::from_millis(200),
        Duration::from_millis(30),
        None,
    )
    .await;
}

fn setting(name: &str, default: u64, max: u64) -> u64 {
    let value = std::env::var(name)
        .map(|s| s.parse().expect("unsigned probe setting"))
        .unwrap_or(default);
    assert!((1..=max).contains(&value), "{name} must be in 1..={max}");
    value
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "configurable load/kill/backup probe; requires curl and ps; see docs/operations.md"]
async fn concurrent_load_process_kill_and_backup_restore() {
    probe(
        setting("JADE_PROBE_CLIENTS_PER_FORMAT", 4, 64) as usize,
        Duration::from_secs(setting("JADE_PROBE_SECONDS", 60, 86400)),
        Duration::from_millis(setting("JADE_PROBE_INTERVAL_MS", 100, 60000)),
        std::env::var_os("JADE_PROBE_REPORT").map(PathBuf::from),
    )
    .await;
}
