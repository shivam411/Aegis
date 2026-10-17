//! The built-in reverse proxy ("ingress").
//!
//! Routes requests by `Host` to the live process of the app a domain is
//! mapped to (`127.0.0.1:<port>`), terminates HTTPS with per-domain
//! certificates chosen by SNI (from ACME or from files), answers ACME
//! HTTP-01 challenges, and redirects HTTP to HTTPS once a domain has a
//! certificate. The control plane flips an app's upstream port during a
//! deployment, which makes cutovers zero-downtime.
//!
//! [`Ingress`] holds only routing state; the daemon owns the listeners
//! ([`Ingress::serve_http`], [`Ingress::serve_https`]) and the background
//! task that issues, renews and reloads certificates ([`Ingress::run_maintenance`]).

pub mod acme;
pub mod certs;
pub mod headers;
pub mod proxy;
pub mod routes;

pub use acme::{AcmeSettings, ChallengeStore, LETS_ENCRYPT_PRODUCTION, LETS_ENCRYPT_STAGING};
pub use aegis_types::domain::normalize_domain;
pub use certs::{CertStatus, CertStore};
pub use routes::{DomainRoute, InFlight, Resolution, RouteTable, Target, TlsMode};

use chrono::Utc;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long to wait before retrying a failed ACME order for a domain. Let's
/// Encrypt allows only a few failed validations per hour.
const ACME_RETRY_AFTER: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone)]
pub struct IngressConfig {
    /// Address the listeners bind to (informational; the daemon binds).
    pub host: String,
    pub http_port: u16,
    pub https_port: u16,
    /// Where ACME accounts and certificates are stored.
    pub certs_dir: PathBuf,
    /// `None` disables automatic certificates.
    pub acme: Option<AcmeSettings>,
    /// How long an upstream may take to send its response headers.
    pub upstream_timeout: Duration,
    /// How often certificates are checked for renewal and files for changes.
    pub maintenance_interval: Duration,
}

impl IngressConfig {
    pub fn new(certs_dir: PathBuf) -> Self {
        Self {
            host: "0.0.0.0".into(),
            http_port: 80,
            https_port: 443,
            certs_dir,
            acme: None,
            upstream_timeout: Duration::from_secs(120),
            maintenance_interval: Duration::from_secs(60),
        }
    }
}

/// One route as shown by the status API.
#[derive(Debug, Clone, Serialize)]
pub struct RouteStatus {
    pub domain: String,
    /// "project" or "dashboard".
    pub target: String,
    pub project_id: Option<String>,
    pub upstream_port: Option<u16>,
    /// Every instance's port (several when the app runs more than one).
    pub upstream_ports: Vec<u16>,
    pub cert: CertStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct IngressStatus {
    pub host: String,
    pub http_port: u16,
    pub https_port: u16,
    pub acme_enabled: bool,
    pub acme_email: Option<String>,
    pub acme_directory: Option<String>,
    pub routes: Vec<RouteStatus>,
}

#[derive(Debug, Clone)]
struct AcmeAttempt {
    at: Instant,
    error: String,
}

pub(crate) struct Inner {
    pub(crate) config: IngressConfig,
    pub(crate) routes: RouteTable,
    pub(crate) certs: Arc<CertStore>,
    pub(crate) challenges: ChallengeStore,
    pub(crate) inflight: InFlight,
    pub(crate) client: proxy::UpstreamClient,
    acme_failures: Mutex<HashMap<String, AcmeAttempt>>,
    /// Problems loading certificate files, by domain.
    file_errors: Mutex<HashMap<String, String>>,
    acme_busy: Mutex<Option<String>>,
    wake: tokio::sync::Notify,
}

/// A cheap, cloneable handle to the proxy's routing and certificate state.
#[derive(Clone)]
pub struct Ingress {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Ingress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ingress")
            .field("config", &self.inner.config)
            .finish()
    }
}

impl Ingress {
    pub fn new(config: IngressConfig) -> Self {
        let client = proxy::upstream_client();
        Self {
            inner: Arc::new(Inner {
                config,
                routes: RouteTable::new(),
                certs: Arc::new(CertStore::default()),
                challenges: ChallengeStore::default(),
                inflight: InFlight::default(),
                client,
                acme_failures: Mutex::new(HashMap::new()),
                file_errors: Mutex::new(HashMap::new()),
                acme_busy: Mutex::new(None),
                wake: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn config(&self) -> &IngressConfig {
        &self.inner.config
    }

    pub fn routes(&self) -> &RouteTable {
        &self.inner.routes
    }

    pub fn challenges(&self) -> &ChallengeStore {
        &self.inner.challenges
    }

    /// Maps (or re-maps) a domain. Certificates are loaded right away when
    /// they exist; missing ACME certificates are requested in the background.
    pub fn set_domain(&self, domain: &str, route: DomainRoute) {
        let inner = &self.inner;
        inner.certs.remove(domain);
        inner.file_errors.lock().unwrap().remove(domain);
        inner.acme_failures.lock().unwrap().remove(domain);
        match route.tls {
            TlsMode::Files => self.load_files(domain, &route),
            TlsMode::Acme => {
                let (cert, key) = certs::acme_paths(&inner.config.certs_dir, domain);
                if cert.exists() {
                    match certs::load_cert_files(&cert, &key) {
                        Ok(loaded) => inner.certs.insert(domain, loaded),
                        Err(e) => {
                            tracing::warn!(domain, error = %e, "Stored certificate is unusable; requesting a new one")
                        }
                    }
                }
            }
            TlsMode::Off => {}
        }
        inner.routes.set_domain(domain, route);
        inner.wake.notify_one();
    }

    fn load_files(&self, domain: &str, route: &DomainRoute) {
        let (Some(cert), Some(key)) = (&route.cert_path, &route.key_path) else {
            self.inner.file_errors.lock().unwrap().insert(
                domain.to_string(),
                "No certificate and key paths configured".into(),
            );
            return;
        };
        match certs::load_cert_files(cert, key) {
            Ok(loaded) => {
                self.inner.certs.insert(domain, loaded);
                self.inner.file_errors.lock().unwrap().remove(domain);
            }
            Err(e) => {
                tracing::warn!(domain, error = %e, "Cannot load certificate files");
                self.inner
                    .file_errors
                    .lock()
                    .unwrap()
                    .insert(domain.to_string(), e.to_string());
            }
        }
    }

    pub fn remove_domain(&self, domain: &str) {
        self.inner.routes.remove_domain(domain);
        self.inner.certs.remove(domain);
        self.inner.file_errors.lock().unwrap().remove(domain);
        self.inner.acme_failures.lock().unwrap().remove(domain);
    }

    /// Points a project's domains at `port`; the atomic cutover flip.
    pub fn set_upstream(&self, project_id: &str, port: Option<u16>) {
        self.inner.routes.set_upstream(project_id, port);
    }

    pub fn upstream(&self, project_id: &str) -> Option<u16> {
        self.inner.routes.upstream(project_id)
    }

    /// Points a project's domains at a set of ports (one per instance) in
    /// one step; requests are spread over them round robin.
    pub fn set_upstreams(&self, project_id: &str, ports: Vec<u16>) {
        self.inner.routes.set_upstreams(project_id, ports);
    }

    /// The ports a project's requests go to.
    pub fn upstreams(&self, project_id: &str) -> Vec<u16> {
        self.inner.routes.upstreams(project_id)
    }

    /// Serves the Aegis dashboard (the web listener on `port`) on `domain`.
    pub fn set_dashboard(&self, domain: &str, route: DomainRoute, port: u16) {
        self.inner.routes.set_dashboard_port(Some(port));
        self.set_domain(domain, route);
    }

    /// Requests and upgraded connections currently proxied to `port`.
    pub fn in_flight(&self, port: u16) -> usize {
        self.inner.inflight.count(port)
    }

    /// Waits (up to `timeout`) until nothing is proxied to `port` any more.
    pub async fn wait_idle(&self, port: u16, timeout: Duration) -> bool {
        self.inner.inflight.wait_idle(port, timeout).await
    }

    /// The certificate state of a mapped domain.
    pub fn cert_status(&self, domain: &str) -> CertStatus {
        let route = self.inner.routes.domain(domain);
        let mode = route.as_ref().map_or(TlsMode::Off, |r| r.tls);
        let mut status = CertStatus {
            domain: domain.to_string(),
            mode: mode.as_str().to_string(),
            state: "off".into(),
            not_after: None,
            message: None,
        };
        if route.is_none() {
            status.state = "none".into();
            status.message = Some("Not mapped".into());
            return status;
        }
        if mode == TlsMode::Off {
            status.message = Some("Served over plain HTTP".into());
            return status;
        }
        let now = Utc::now();
        if let Some(cert) = self.inner.certs.get(domain) {
            status.not_after = cert.not_after.map(|t| t.to_rfc3339());
            status.state = match cert.not_after {
                Some(t) if t <= now => "expired",
                expiry if certs::needs_renewal(expiry, now) => "expiring",
                _ => "valid",
            }
            .into();
        }
        match mode {
            TlsMode::Files => {
                if let Some(error) = self.inner.file_errors.lock().unwrap().get(domain) {
                    if status.not_after.is_none() {
                        status.state = "error".into();
                    }
                    status.message = Some(error.clone());
                }
            }
            TlsMode::Acme => {
                let failure = self
                    .inner
                    .acme_failures
                    .lock()
                    .unwrap()
                    .get(domain)
                    .cloned();
                if self.inner.config.acme.is_none() {
                    if status.not_after.is_none() {
                        status.state = "disabled".into();
                    }
                    status.message =
                        Some("Automatic HTTPS is off; enable [proxy.acme] in aegis.toml".into());
                } else if let Some(failure) = failure {
                    if status.not_after.is_none() {
                        status.state = "error".into();
                    }
                    status.message = Some(failure.error);
                } else if status.not_after.is_none() {
                    status.state = "pending".into();
                    status.message = Some(
                        if self.inner.acme_busy.lock().unwrap().as_deref() == Some(domain) {
                            "Requesting a certificate"
                        } else {
                            "Waiting to request a certificate"
                        }
                        .into(),
                    );
                }
            }
            TlsMode::Off => {}
        }
        status
    }

    pub fn status(&self) -> IngressStatus {
        let config = &self.inner.config;
        let routes = self
            .inner
            .routes
            .domains()
            .into_iter()
            .map(|(domain, route)| {
                let (target, project_id) = match &route.target {
                    Target::Project(id) => ("project", Some(id.clone())),
                    Target::Dashboard => ("dashboard", None),
                };
                RouteStatus {
                    upstream_port: self.inner.routes.port_for(&route.target),
                    upstream_ports: self.inner.routes.ports_for(&route.target),
                    cert: self.cert_status(&domain),
                    domain,
                    target: target.into(),
                    project_id,
                }
            })
            .collect();
        IngressStatus {
            host: config.host.clone(),
            http_port: config.http_port,
            https_port: config.https_port,
            acme_enabled: config.acme.is_some(),
            acme_email: config.acme.as_ref().and_then(|a| a.email.clone()),
            acme_directory: config.acme.as_ref().map(|a| a.directory_url.clone()),
            routes,
        }
    }

    /// One maintenance pass: reloads changed certificate files and issues or
    /// renews ACME certificates that are due. Returns the domains for which
    /// a certificate was issued.
    pub async fn maintain(&self) -> Vec<String> {
        let inner = &self.inner;
        let now = Utc::now();
        let mut due = Vec::new();
        for (domain, route) in inner.routes.domains() {
            match route.tls {
                TlsMode::Files => {
                    let (Some(cert), Some(key)) = (&route.cert_path, &route.key_path) else {
                        continue;
                    };
                    let stamps = (certs::modified(cert), certs::modified(key));
                    let current = inner.certs.get(&domain).map(|c| c.stamps);
                    if current != Some(stamps) {
                        self.load_files(&domain, &route);
                        if inner.certs.get(&domain).map(|c| c.stamps) == Some(stamps) {
                            tracing::info!(domain = %domain, "Loaded certificate files");
                        }
                    }
                }
                TlsMode::Acme => {
                    let expiry = inner.certs.get(&domain).and_then(|c| c.not_after);
                    let has_cert = inner.certs.contains(&domain);
                    if (has_cert && !certs::needs_renewal(expiry, now))
                        || inner.config.acme.is_none()
                    {
                        continue;
                    }
                    let recently_failed = inner
                        .acme_failures
                        .lock()
                        .unwrap()
                        .get(&domain)
                        .is_some_and(|f| f.at.elapsed() < ACME_RETRY_AFTER);
                    if !recently_failed {
                        due.push(domain);
                    }
                }
                TlsMode::Off => {}
            }
        }

        let mut issued = Vec::new();
        let Some(settings) = inner.config.acme.clone() else {
            return issued;
        };
        for domain in due {
            *inner.acme_busy.lock().unwrap() = Some(domain.clone());
            tracing::info!(domain = %domain, "Requesting a certificate");
            let result = acme::issue(
                &settings,
                &inner.config.certs_dir,
                &domain,
                &inner.challenges,
            )
            .await;
            *inner.acme_busy.lock().unwrap() = None;
            let stored = result.and_then(|(chain, key)| {
                let loaded = certs::load_cert_pem(chain.as_bytes(), key.as_bytes())?;
                let (cert_path, key_path) = certs::acme_paths(&inner.config.certs_dir, &domain);
                if let Some(dir) = cert_path.parent() {
                    certs::create_private_dir(dir)?;
                }
                certs::write_private(&key_path, key.as_bytes())?;
                certs::write_private(&cert_path, chain.as_bytes())?;
                Ok(loaded)
            });
            // The domain may have been unmapped while the order ran.
            let still_acme = inner
                .routes
                .domain(&domain)
                .is_some_and(|r| r.tls == TlsMode::Acme);
            match stored {
                Ok(loaded) if still_acme => {
                    tracing::info!(domain = %domain, not_after = ?loaded.not_after, "Certificate issued");
                    inner.certs.insert(&domain, loaded);
                    inner.acme_failures.lock().unwrap().remove(&domain);
                    issued.push(domain);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(domain = %domain, error = %e, "Certificate request failed; retrying in an hour");
                    inner.acme_failures.lock().unwrap().insert(
                        domain,
                        AcmeAttempt {
                            at: Instant::now(),
                            error: format!("Certificate request failed: {:#}", e),
                        },
                    );
                }
            }
        }
        issued
    }

    /// Runs [`Ingress::maintain`] now, whenever a domain is mapped, and every
    /// `maintenance_interval`, until `shutdown` resolves.
    pub async fn run_maintenance(self, shutdown: impl std::future::Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = self.maintain() => {}
                _ = &mut shutdown => return,
            }
            tokio::select! {
                _ = tokio::time::sleep(self.inner.config.maintenance_interval) => {}
                _ = self.inner.wake.notified() => {}
                _ = &mut shutdown => return,
            }
        }
    }
}
