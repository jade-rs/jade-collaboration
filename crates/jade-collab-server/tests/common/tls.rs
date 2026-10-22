//! A real TLS termination proxy. Trust is supplied only to child processes;
//! tests never change the parent process environment or the OS trust store.
#![allow(dead_code)]
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio_rustls::{rustls, TlsAcceptor};

pub struct Proxy {
    pub ca: PathBuf,
    pub wrong_ca: PathBuf,
    pub port: u16,
    pub active: Arc<AtomicUsize>,
    directory: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
}
impl Proxy {
    pub async fn start(upstream: std::net::SocketAddr) -> Self {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let ca = directory.path().join("server.pem");
        std::fs::write(&ca, generated.cert.pem()).unwrap();
        let wrong_ca = directory.path().join("other.pem");
        std::fs::write(
            &wrong_ca,
            rcgen::generate_simple_self_signed(vec!["localhost".into()])
                .unwrap()
                .cert
                .pem(),
        )
        .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![generated.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let active = Arc::new(AtomicUsize::new(0));
        let count = active.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    incoming=listener.accept()=>{
                        let Ok((tcp,_))=incoming else {break};let acceptor=acceptor.clone();let count=count.clone();
                        connections.spawn(async move {
                            let Ok(mut secure)=acceptor.accept(tcp).await else {return};
                            let Ok(mut plain)=tokio::net::TcpStream::connect(upstream).await else {return};
                            count.fetch_add(1,Ordering::SeqCst);let _guard=Active(count);
                            let _=tokio::io::copy_bidirectional(&mut secure,&mut plain).await;
                        });
                    }
                    _=connections.join_next(),if !connections.is_empty()=>{}
                }
            }
        });
        Self {
            ca,
            wrong_ca,
            port,
            active,
            directory,
            task,
        }
    }
    pub fn base(&self, valid_host: bool) -> String {
        format!(
            "https://{}:{}",
            if valid_host { "localhost" } else { "127.0.0.1" },
            self.port
        )
    }
    pub fn socket(&self, name: &str, valid_host: bool) -> String {
        format!(
            "{}{}",
            self.base(valid_host).replacen("https://", "wss://", 1),
            jade_collab_server::document_socket_path(name)
        )
    }
    pub async fn idle(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while self.active.load(Ordering::SeqCst) != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("all terminated TLS connections were released");
    }
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn fixture(source: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../jade-core")
            .join(source),
    )
    .unwrap()
}
