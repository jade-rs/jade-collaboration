//! `jade-collab`: host documents for co-editing, and edit or read them from
//! the command line (which is also how a shell-driven agent takes part).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use jade_collab_server::{Client, Format, Fsync, PresenceLimits, Server, StorageOptions, Tokens};
use jade_collab_sync::ReplicaEvent;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::DocxModel;
use jade_pptx_collab::intent::{resolve as resolve_slide, PptxIntent};
use jade_pptx_collab::PptxModel;
use jade_xlsx_collab::intent::{resolve as resolve_cell, XlsxIntent};
use jade_xlsx_collab::XlsxModel;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(
    name = "jade-collab",
    about = "Collaborative editing of .docx and .xlsx with jade"
)]
enum Cli {
    /// Host documents. Each file is served as /documents/<file name>.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: String,
        /// Write every document back to its file on Ctrl-C.
        #[arg(long)]
        save: bool,
        /// Keep documents durable in this directory: everything stored there
        /// is served again on restart, and wins over a file of the same name.
        #[arg(long, value_name = "DIR")]
        data: Option<PathBuf>,
        /// With --data, take a checkpoint after this many journal records.
        #[arg(long, value_name = "N", requires = "data")]
        checkpoint_every: Option<u64>,
        /// With --data, keep this many committed operations from before each
        /// checkpoint, so replicas that far behind can still resume
        /// (default 1000).
        #[arg(long, value_name = "N", requires = "data")]
        keep_tail: Option<usize>,
        /// With --data, when the journal is synced to the disk: after every
        /// record, before anyone hears of it, or only at checkpoints (the
        /// default; the process dying loses nothing, a power failure can).
        #[arg(long, value_enum, value_name = "WHEN", requires = "data")]
        fsync: Option<FsyncArg>,
        /// Require a bearer token on every request. The file is a JSON
        /// object: {"<token>": {"name": "alice", "role": "editor"|"viewer"}};
        /// a grant may add "documents": ["contract.docx", "deck-*.pptx",
        /// {"document": "notes.docx", "role": "viewer"}] to limit the token
        /// to those names (globs; the first match decides the role).
        /// The file is the only source of tokens: it is read again on SIGHUP
        /// and whenever it changes, and sockets whose token was withdrawn
        /// are closed. A file that does not parse leaves the tokens as they
        /// were.
        #[arg(long, value_name = "FILE")]
        tokens: Option<PathBuf>,
        /// With --tokens, look for changes to the file this often; 0 leaves
        /// only SIGHUP.
        #[arg(long, value_name = "SECONDS", default_value_t = 1, requires = "tokens")]
        tokens_poll: u64,
        /// The largest presence state a connection may share, in bytes of
        /// JSON (default 16384).
        #[arg(long, value_name = "BYTES")]
        presence_max_bytes: Option<usize>,
        /// Presence updates a connection may send at once (default 40);
        /// more are dropped and the sender gets `presence_rejected`.
        #[arg(long, value_name = "N")]
        presence_burst: Option<u32>,
        /// Presence updates per second a connection may keep sending after
        /// that (default 20).
        #[arg(long, value_name = "N")]
        presence_per_second: Option<u32>,
        files: Vec<PathBuf>,
    },
    /// Print a document's current content as JSON.
    Show {
        /// The bearer token, if the server requires one.
        #[arg(long)]
        token: Option<String>,
        /// ws://host:port/documents/<name>/ws
        url: String,
    },
    /// Apply edits — one JSON object or an array — and wait for the server.
    /// docx: {"type":"insert_text","paragraph":0,"offset":0,"text":"…"},
    /// delete_text, replace_range, insert_paragraph_break,
    /// format ({"type":"format",…,"change":{"kind":"bold","value":true}}), format_paragraphs,
    /// add_comment, reply_to_comment / resolve_comment / delete_comment ("comment_id" from show).
    /// xlsx: {"type":"set_cell","sheet":"S","reference":"B2","value":{"kind":"number","value":1}},
    /// insert_row, delete_row, insert_column, delete_column.
    Apply {
        /// The bearer token, if the server requires one.
        #[arg(long)]
        token: Option<String>,
        url: String,
        edits: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum FsyncArg {
    Every,
    Checkpoint,
}

fn format_of(url: &str) -> Result<Format, String> {
    Format::of_url(url)
        .ok_or_else(|| format!("{url} does not name a .docx, .xlsx or .pptx document"))
}

fn edits<T: serde::de::DeserializeOwned>(text: &str) -> Result<Vec<T>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let items = match value {
        Value::Array(items) => items,
        single => vec![single],
    };
    items
        .into_iter()
        .map(|item| serde_json::from_value(item).map_err(|e| e.to_string()))
        .collect()
}

fn docx_json(model: &DocxModel) -> Value {
    json!({
        "format": "docx",
        "paragraphs": model.paragraphs().iter().enumerate()
            .map(|(index, p)| json!({ "index": index, "text": p.text }))
            .collect::<Vec<_>>(),
        "tables": model.tables().iter()
            .map(|t| json!({ "rows": t.rows.len(), "columns": t.columns.len(), "locked": t.locked }))
            .collect::<Vec<_>>(),
        // `id` is what reply_to_comment / resolve_comment / delete_comment take.
        "comments": model.comments().iter()
            .map(|c| json!({
                "id": c.id.to_string(),
                "file_id": c.file_id,
                "parent": c.parent.map(|p| p.to_string()),
                "author": c.content.author,
                "text": c.content.text,
                "resolved": c.resolved,
            }))
            .collect::<Vec<_>>(),
    })
}

fn xlsx_json(model: &XlsxModel) -> Value {
    let sheets = model
        .sheets()
        .iter()
        .map(|sheet| {
            let cells: serde_json::Map<String, Value> = model
                .filled_cells(sheet.id)
                .into_iter()
                .map(|(reference, value)| {
                    (
                        reference,
                        serde_json::to_value(value).expect("values serialize"),
                    )
                })
                .collect();
            json!({ "name": sheet.name, "cells": cells })
        })
        .collect::<Vec<_>>();
    json!({ "format": "xlsx", "sheets": sheets })
}

fn pptx_json(model: &PptxModel) -> Value {
    let slides = model
        .slides()
        .iter()
        .enumerate()
        .map(|(index, slide)| {
            json!({
                "slide": index + 1,
                "shapes": slide.shapes.iter()
                    .map(|shape| json!({
                        "shape": shape.number,
                        "kind": format!("{:?}", shape.kind),
                        "text": shape.text,
                        "editable": shape.editable,
                        "geometry": shape.geometry.map(|g| json!({
                            "x": g.x, "y": g.y, "width": g.width, "height": g.height, "rotation": g.rotation,
                        })),
                    }))
                    .collect::<Vec<_>>(),
                "notes": slide.notes,
            })
        })
        .collect::<Vec<_>>();
    json!({ "format": "pptx", "slides": slides })
}

fn report<Op: std::fmt::Debug>(events: Vec<ReplicaEvent<Op>>) -> Result<(), String> {
    let rejected: Vec<String> = events
        .into_iter()
        .filter_map(|event| match event {
            ReplicaEvent::Rejected { op, reason, .. } => Some(format!("{op:?}: {reason}")),
            _ => None,
        })
        .collect();
    if rejected.is_empty() {
        Ok(())
    } else {
        Err(format!("rejected by the server:\n{}", rejected.join("\n")))
    }
}

async fn show(url: &str, token: Option<&str>) -> Result<(), String> {
    let output = match format_of(url)? {
        Format::Docx => docx_json(
            &Client::<DocxModel>::connect(url, token)
                .await
                .map_err(|e| e.to_string())?
                .view(),
        ),
        Format::Xlsx => xlsx_json(
            &Client::<XlsxModel>::connect(url, token)
                .await
                .map_err(|e| e.to_string())?
                .view(),
        ),
        Format::Pptx => pptx_json(
            &Client::<PptxModel>::connect(url, token)
                .await
                .map_err(|e| e.to_string())?
                .view(),
        ),
    };
    println!("{}", serde_json::to_string_pretty(&output).expect("JSON"));
    Ok(())
}

async fn apply(url: &str, token: Option<&str>, text: &str) -> Result<(), String> {
    match format_of(url)? {
        Format::Docx => {
            let client = Client::<DocxModel>::connect(url, token)
                .await
                .map_err(|e| e.to_string())?;
            for edit in edits::<IntentOp>(text)? {
                let view = client.view();
                let ops =
                    resolve(&view, &edit, &mut |n| client.reserve(n)).map_err(|e| e.to_string())?;
                for op in ops {
                    client.submit(op).map_err(|e| e.to_string())?;
                }
            }
            client.settle().await.map_err(|e| e.to_string())?;
            report(client.take_events())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&docx_json(&client.view())).expect("JSON")
            );
        }
        Format::Xlsx => {
            let client = Client::<XlsxModel>::connect(url, token)
                .await
                .map_err(|e| e.to_string())?;
            for edit in edits::<XlsxIntent>(text)? {
                let view = client.view();
                let op = resolve_cell(&view, &edit, &mut |n| client.reserve(n))
                    .map_err(|e| e.to_string())?;
                client.submit(op).map_err(|e| e.to_string())?;
            }
            client.settle().await.map_err(|e| e.to_string())?;
            report(client.take_events())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&xlsx_json(&client.view())).expect("JSON")
            );
        }
        Format::Pptx => {
            let client = Client::<PptxModel>::connect(url, token)
                .await
                .map_err(|e| e.to_string())?;
            for edit in edits::<PptxIntent>(text)? {
                let view = client.view();
                let ops = resolve_slide(&view, &edit, &mut |n| client.reserve(n))
                    .map_err(|e| e.to_string())?;
                for op in ops {
                    client.submit(op).map_err(|e| e.to_string())?;
                }
            }
            client.settle().await.map_err(|e| e.to_string())?;
            report(client.take_events())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&pptx_json(&client.view())).expect("JSON")
            );
        }
    }
    Ok(())
}

struct ServeOptions {
    addr: String,
    save: bool,
    data: Option<PathBuf>,
    checkpoint_every: Option<u64>,
    keep_tail: Option<usize>,
    fsync: Option<FsyncArg>,
    tokens: Option<PathBuf>,
    tokens_poll: u64,
    presence: PresenceLimits,
    files: Vec<PathBuf>,
}

async fn serve(options: ServeOptions) -> Result<(), String> {
    let ServeOptions {
        addr,
        save,
        data,
        checkpoint_every,
        keep_tail,
        fsync,
        tokens,
        tokens_poll,
        presence,
        files,
    } = options;
    // Bound first, so `--addr host:0` works and the URLs printed are real.
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let server = match data {
        Some(dir) => {
            let mut storage = StorageOptions::default();
            if let Some(n) = checkpoint_every {
                storage.checkpoint_every = n.max(1);
            }
            if let Some(n) = keep_tail {
                storage.keep_tail = n;
            }
            storage.sync = match fsync {
                Some(FsyncArg::Every) => Fsync::EveryRecord,
                Some(FsyncArg::Checkpoint) | None => Fsync::Checkpoints,
            };
            let server = Server::with_storage_options(dir, storage).map_err(|e| e.to_string())?;
            for (name, _) in server.documents() {
                println!("serving ws://{addr}/documents/{name}/ws (stored)");
            }
            server
        }
        None => Server::new(),
    };
    let server = server.with_presence_limits(presence);
    let server = match &tokens {
        Some(path) => server.with_tokens(Tokens::load(path).map_err(|e| e.to_string())?),
        None => server,
    };
    if let Some(path) = tokens {
        if tokens_poll > 0 {
            server.watch_tokens(path.clone(), Duration::from_secs(tokens_poll));
        }
        reload_on_hangup(server.clone(), path).map_err(|e| e.to_string())?;
    }
    let stored: Vec<String> = server.documents().into_iter().map(|(n, _)| n).collect();
    let mut named = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{} has no file name", path.display()))?
            .to_string();
        if !stored.contains(&name) {
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            server.open(&name, bytes).map_err(|e| e.to_string())?;
            println!("serving ws://{addr}/documents/{name}/ws");
        }
        named.push((name, path));
    }
    // Scripts wait for this line before connecting.
    println!("listening on {addr}");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    tokio::select! {
        result = server.clone().serve(listener) => result.map_err(|e| e.to_string())?,
        _ = tokio::signal::ctrl_c() => {}
    }
    if save {
        for (name, path) in named {
            let bytes = server.export(&name).await.map_err(|e| e.to_string())?;
            std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("saved {}", path.display());
        }
    }
    Ok(())
}

/// Reads the tokens file again whenever the process gets SIGHUP.
#[cfg(unix)]
fn reload_on_hangup(server: Server, path: PathBuf) -> std::io::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut hangups = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        while hangups.recv().await.is_some() {
            match server.reload_tokens(&path) {
                Ok(()) => eprintln!("took new tokens from {}", path.display()),
                Err(error) => eprintln!("kept the tokens in force: {error}"),
            }
        }
    });
    Ok(())
}

#[cfg(not(unix))]
fn reload_on_hangup(_: Server, _: PathBuf) -> std::io::Result<()> {
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match Cli::parse() {
        Cli::Serve {
            addr,
            save,
            data,
            checkpoint_every,
            keep_tail,
            fsync,
            tokens,
            tokens_poll,
            presence_max_bytes,
            presence_burst,
            presence_per_second,
            files,
        } => {
            let defaults = PresenceLimits::default();
            let presence = PresenceLimits {
                max_bytes: presence_max_bytes.unwrap_or(defaults.max_bytes),
                burst: presence_burst.unwrap_or(defaults.burst),
                per_second: presence_per_second.unwrap_or(defaults.per_second),
            };
            serve(ServeOptions {
                addr,
                save,
                data,
                checkpoint_every,
                keep_tail,
                fsync,
                tokens,
                tokens_poll,
                presence,
                files,
            })
            .await
        }
        Cli::Show { token, url } => show(&url, token.as_deref()).await,
        Cli::Apply { token, url, edits } => apply(&url, token.as_deref(), &edits).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
