//! HTTPS serving with rustls.
//!
//! Certificates are read from PEM files and re-read when the files change
//! (checked at most every `reload_interval`), so renewals from certbot or
//! similar take effect without restarting the daemon, which would restart
//! every app.

use crate::{router, AppState};
use axum::extract::ConnectInfo;
use axum::Extension;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use tokio_rustls::TlsAcceptor;
use tower::ServiceBuilder;

fn load_certified_key(cert_path: &Path, key_path: &Path) -> Result<CertifiedKey, anyhow::Error> {
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(cert_path)
            .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", cert_path.display(), e))?,
    ))
    .collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        anyhow::bail!("No certificates found in {}", cert_path.display());
    }
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::File::open(key_path)
            .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", key_path.display(), e))?,
    ))?
    .ok_or_else(|| anyhow::anyhow!("No private key found in {}", key_path.display()))?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|e| anyhow::anyhow!("Unsupported private key: {}", e))?;
    let key = CertifiedKey::new(certs, signing);
    // During a renewal the two files can briefly belong to different pairs.
    key.keys_match()
        .map_err(|e| anyhow::anyhow!("Certificate and private key don't match: {}", e))?;
    Ok(key)
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Serves the current certificate, reloading it when the files change.
pub struct ReloadingResolver {
    cert_path: PathBuf,
    key_path: PathBuf,
    reload_interval: Duration,
    current: RwLock<Arc<CertifiedKey>>,
    /// (last check, file modification times at last load)
    checked: Mutex<(Instant, Option<SystemTime>, Option<SystemTime>)>,
}

impl std::fmt::Debug for ReloadingResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReloadingResolver")
            .field("cert_path", &self.cert_path)
            .finish()
    }
}

impl ReloadingResolver {
    pub fn new(
        cert_path: PathBuf,
        key_path: PathBuf,
        reload_interval: Duration,
    ) -> Result<Self, anyhow::Error> {
        let key = load_certified_key(&cert_path, &key_path)?;
        let stamps = (modified(&cert_path), modified(&key_path));
        Ok(Self {
            current: RwLock::new(Arc::new(key)),
            checked: Mutex::new((Instant::now(), stamps.0, stamps.1)),
            cert_path,
            key_path,
            reload_interval,
        })
    }

    fn maybe_reload(&self) {
        let mut checked = self.checked.lock().unwrap();
        if checked.0.elapsed() < self.reload_interval {
            return;
        }
        checked.0 = Instant::now();
        let stamps = (modified(&self.cert_path), modified(&self.key_path));
        if (stamps.0, stamps.1) == (checked.1, checked.2) {
            return;
        }
        match load_certified_key(&self.cert_path, &self.key_path) {
            Ok(key) => {
                *self.current.write().unwrap() = Arc::new(key);
                (checked.1, checked.2) = stamps;
                tracing::info!(cert = %self.cert_path.display(), "Reloaded TLS certificate");
            }
            // Mid-renewal the pair can be inconsistent; keep serving the old one.
            Err(e) => {
                tracing::warn!(error = %e, "Failed to reload TLS certificate; keeping the current one")
            }
        }
    }
}

impl ResolvesServerCert for ReloadingResolver {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.maybe_reload();
        Some(self.current.read().unwrap().clone())
    }
}

/// A rustls server config (TLS 1.2+, HTTP/2 and HTTP/1.1) for the given files.
pub fn server_config(
    cert_path: PathBuf,
    key_path: PathBuf,
    reload_interval: Duration,
) -> Result<Arc<rustls::ServerConfig>, anyhow::Error> {
    let resolver = ReloadingResolver::new(cert_path, key_path, reload_interval)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(resolver));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// Serves the API over HTTPS until `shutdown` resolves, then gives open
/// requests up to 5 seconds to finish.
pub async fn serve_tls(
    listener: tokio::net::TcpListener,
    state: AppState,
    config: Arc<rustls::ServerConfig>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(config);
    let app = router(state);
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    tokio::pin!(shutdown);
    loop {
        let (tcp, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to accept connection");
                    continue;
                }
            },
            _ = &mut shutdown => break,
        };
        let acceptor = acceptor.clone();
        let service = ServiceBuilder::new()
            .layer(Extension(ConnectInfo(peer)))
            .service(app.clone());
        let watcher = graceful.watcher();
        tokio::spawn(async move {
            // Don't let a client that never finishes the handshake hold a task forever.
            let tls =
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                    Ok(Ok(tls)) => tls,
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, peer = %peer, "TLS handshake failed");
                        return;
                    }
                    Err(_) => return,
                };
            let builder = Builder::new(TokioExecutor::new());
            let conn = builder.serve_connection_with_upgrades(
                TokioIo::new(tls),
                TowerToHyperService::new(service),
            );
            if let Err(e) = watcher.watch(conn.into_owned()).await {
                tracing::debug!(error = %e, peer = %peer, "Connection ended with an error");
            }
        });
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), graceful.shutdown()).await;
    Ok(())
}
