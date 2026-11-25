use super::{Fixture, Manifest, Updater, CHANNEL_HEADER, PAYLOADS};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct Server {
    origin: String,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    task: tokio::task::JoinHandle<()>,
    certificate: rustls::pki_types::CertificateDer<'static>,
    hang: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn openssl(root: &Path, arguments: &[&str]) {
    let output = std::process::Command::new("openssl")
        .args(arguments)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "disposable TLS fixture generation failed"
    );
}

impl Server {
    async fn start(root: &Path) -> Self {
        openssl(
            root,
            &[
                "req",
                "-x509",
                "-newkey",
                "ed25519",
                "-noenc",
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost",
                "-addext",
                "basicConstraints=critical,CA:FALSE",
                "-addext",
                "extendedKeyUsage=serverAuth",
                "-keyout",
                "key.pem",
                "-out",
                "cert.pem",
            ],
        );
        openssl(
            root,
            &[
                "x509", "-in", "cert.pem", "-outform", "DER", "-out", "cert.der",
            ],
        );
        openssl(
            root,
            &[
                "pkcs8", "-topk8", "-nocrypt", "-in", "key.pem", "-outform", "DER", "-out",
                "key.der",
            ],
        );
        let certificate =
            rustls::pki_types::CertificateDer::from(std::fs::read(root.join("cert.der")).unwrap());
        let private =
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                std::fs::read(root.join("key.der")).unwrap(),
            ));
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], private)
        .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let hang: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> = Arc::default();
        let responses = routes.clone();
        let hang_response = hang.clone();
        let task = tokio::spawn(async move {
            while let Ok((connection, _)) = listener.accept().await {
                let Ok(mut connection) = acceptor.accept(connection).await else {
                    continue;
                };
                let mut request = Vec::new();
                loop {
                    let byte = connection.read_u8().await.unwrap();
                    request.push(byte);
                    assert!(request.len() <= 8192);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let path = std::str::from_utf8(&request)
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let body = responses.lock().unwrap().get(path).cloned().unwrap();
                connection
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let signal = (path == "/sift-desktop")
                    .then(|| hang_response.lock().unwrap().take())
                    .flatten();
                if let Some(signal) = signal {
                    connection.write_all(&body[..1]).await.unwrap();
                    connection.flush().await.unwrap();
                    let _ = signal.send(());
                    // The owner aborts this exact fixture after cancelling its download.
                    std::future::pending::<()>().await;
                }
                connection.write_all(&body).await.unwrap();
                let _ = connection.shutdown().await;
            }
        });
        Self {
            origin,
            routes,
            task,
            certificate,
            hang,
        }
    }

    fn configure(&self, fixture: &mut Fixture, sequence: u64) -> Manifest {
        fixture.updater.config.channel_url = format!("{}/channel", self.origin);
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.certificate.clone()).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        fixture.updater.client = reqwest::Client::builder()
            .use_preconfigured_tls(config)
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let mut manifest = fixture.manifest(sequence);
        for payload in &mut manifest.targets[0].files {
            payload.url = format!("{}/{}", self.origin, payload.name);
        }
        let (raw, signature) = fixture.signed(&manifest);
        let mut routes = self.routes.lock().unwrap();
        routes.insert(
            "/channel".into(),
            [
                CHANNEL_HEADER,
                &super::URL_SAFE_NO_PAD.decode(signature).unwrap(),
                &raw,
            ]
            .concat(),
        );
        for name in PAYLOADS {
            routes.insert(format!("/{name}"), name.as_bytes().to_vec());
        }
        manifest
    }
}

use base64::Engine as _;

#[tokio::test]
async fn https_stage_rejects_corruption_before_advancing_sequence_then_accepts_a_valid_retry() {
    let mut fixture = Fixture::new();
    let server = Server::start(fixture.root.path()).await;
    server.configure(&mut fixture, 1);
    server
        .routes
        .lock()
        .unwrap()
        .insert("/sift-desktop".into(), b"corrupt".to_vec());
    assert!(fixture.updater.check().await.is_err());
    let pointers = fixture.updater.pointers().unwrap();
    assert_eq!(pointers.highest_sequence, 0);
    assert!(pointers.pending.is_none());
    assert_eq!(
        std::fs::read_dir(fixture.updater.config.state_dir.join("versions"))
            .unwrap()
            .count(),
        0
    );
    server
        .routes
        .lock()
        .unwrap()
        .insert("/sift-desktop".into(), b"sift-desktop".to_vec());
    assert_eq!(
        fixture.updater.check().await.unwrap().as_deref(),
        Some("0.1.0-dev.1")
    );
    let candidate = fixture.updater.claim().unwrap().unwrap();
    fixture.updater.settle(&candidate.digest, true).unwrap();
    assert!(fixture.updater.check().await.unwrap().is_none());
    assert_eq!(fixture.updater.pointers().unwrap().highest_sequence, 1);
    let unchanged = Updater::with_keys(
        fixture.updater.config.clone(),
        &super::URL_SAFE_NO_PAD.encode(fixture.signing.verifying_key().as_bytes()),
    )
    .unwrap();
    server.configure(&mut fixture, 2);
    // Production trust must still reject the fixture's private CA.
    assert!(unchanged.check().await.is_err());
    assert_eq!(unchanged.pointers().unwrap().highest_sequence, 1);
}

#[tokio::test]
async fn cancelling_an_incomplete_https_payload_leaves_no_selection_or_partial_bundle() {
    let mut fixture = Fixture::new();
    let server = Server::start(fixture.root.path()).await;
    server.configure(&mut fixture, 1);
    let (started, receiving) = tokio::sync::oneshot::channel();
    *server.hang.lock().unwrap() = Some(started);
    let update = fixture.updater.clone();
    let task = tokio::spawn(async move { update.check().await });
    tokio::time::timeout(Duration::from_secs(10), receiving)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let pointers = fixture.updater.pointers().unwrap();
    assert!(pointers.current.is_none() && pointers.pending.is_none());
    assert_eq!(pointers.highest_sequence, 0);
    assert_eq!(
        std::fs::read_dir(fixture.updater.config.state_dir.join("versions"))
            .unwrap()
            .count(),
        0
    );
}
