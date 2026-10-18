//! `jade-collab`: host documents for co-editing, and edit or read them from
//! the command line (which is also how a shell-driven agent takes part).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use jade_collab_server::{Client, Format, Server};
use jade_collab_sync::ReplicaEvent;
use jade_docx_collab::intent::{resolve, IntentOp};
use jade_docx_collab::DocxModel;
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
        files: Vec<PathBuf>,
    },
    /// Print a document's current content as JSON.
    Show {
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
    Apply { url: String, edits: String },
}

fn format_of(url: &str) -> Result<Format, String> {
    Format::of_url(url).ok_or_else(|| format!("{url} does not name a .docx or .xlsx document"))
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

async fn show(url: &str) -> Result<(), String> {
    let output = match format_of(url)? {
        Format::Docx => docx_json(
            &Client::<DocxModel>::connect(url)
                .await
                .map_err(|e| e.to_string())?
                .view(),
        ),
        Format::Xlsx => xlsx_json(
            &Client::<XlsxModel>::connect(url)
                .await
                .map_err(|e| e.to_string())?
                .view(),
        ),
    };
    println!("{}", serde_json::to_string_pretty(&output).expect("JSON"));
    Ok(())
}

async fn apply(url: &str, text: &str) -> Result<(), String> {
    match format_of(url)? {
        Format::Docx => {
            let client = Client::<DocxModel>::connect(url)
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
            let client = Client::<XlsxModel>::connect(url)
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
    }
    Ok(())
}

async fn serve(addr: &str, save: bool, files: Vec<PathBuf>) -> Result<(), String> {
    let server = Server::new();
    let mut named = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{} has no file name", path.display()))?
            .to_string();
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        server.open(&name, bytes).map_err(|e| e.to_string())?;
        println!("serving ws://{addr}/documents/{name}/ws");
        named.push((name, path));
    }
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| e.to_string())?;
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

#[tokio::main]
async fn main() -> ExitCode {
    let result = match Cli::parse() {
        Cli::Serve { addr, save, files } => serve(&addr, save, files).await,
        Cli::Show { url } => show(&url).await,
        Cli::Apply { url, edits } => apply(&url, &edits).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
