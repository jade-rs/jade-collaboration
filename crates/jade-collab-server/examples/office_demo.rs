//! A loopback-only demo host. Production deployments use the normal CLI.
use axum::{
    body::Body,
    extract::OriginalUri,
    http::{header, Response, StatusCode},
    routing::get,
};
use clap::Parser;
use jade_collab_server::{Fsync, Server, StorageOptions};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8787")]
    addr: SocketAddr,
    #[arg(long)]
    assets: PathBuf,
    #[arg(long)]
    data: PathBuf,
    #[arg(long)]
    document: PathBuf,
}

fn open_seed(
    server: &Server,
    document: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !server
        .documents()
        .iter()
        .any(|(name, _)| name == "demo.docx")
    {
        server.open("demo.docx", std::fs::read(document)?)?;
    }
    Ok(())
}

async fn asset(root: Arc<PathBuf>, uri: OriginalUri) -> Response<Body> {
    let path = uri.0.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // Only our generated asset tree is public. Refuse encoded paths too.
    if path.contains('%')
        || path
            .split('/')
            .any(|p| p == ".." || p == "." || p.is_empty())
        || path.contains('\\')
    {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap();
    }
    let content_type = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("ttf") => "font/ttf",
        _ => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        }
    };
    let file = root.join(path);
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(file)).await;
    match bytes {
        Ok(Ok(bytes)) => Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CACHE_CONTROL, "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(Body::from(bytes))
            .unwrap(),
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if !args.addr.ip().is_loopback() {
        return Err("The unauthenticated demo must listen on loopback".into());
    }
    let server = Server::with_storage_options(
        args.data,
        StorageOptions {
            sync: Fsync::EveryRecord,
            checkpoint_every: 101,
            ..Default::default()
        },
    )?;
    open_seed(&server, &args.document)?;
    let root = Arc::new(args.assets.canonicalize()?);
    let router = server
        .router()
        .fallback_service(get(move |uri: OriginalUri| asset(Arc::clone(&root), uri)));
    let listener = tokio::net::TcpListener::bind(args.addr).await?;
    eprintln!("jade-office demo: http://{}/?name=Alice", args.addr);
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarting_restores_the_existing_document_without_reading_the_seed_again() {
        let data = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let seed = files.path().join("seed.docx");
        std::fs::write(&seed, include_bytes!("../../../jade-core/contract.docx")).unwrap();
        {
            let server = Server::with_storage(data.path()).unwrap();
            open_seed(&server, &seed).unwrap();
        }
        std::fs::remove_file(&seed).unwrap();
        let restored = Server::with_storage(data.path()).unwrap();
        open_seed(&restored, &seed).unwrap();
        assert_eq!(restored.documents().len(), 1);
        assert_eq!(restored.documents()[0].0, "demo.docx");
    }
}
