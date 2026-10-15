//! Domains served by the built-in reverse proxy.
//!
//! Mappings are domain events (`DomainMapped`, `DomainUnmapped`), so they
//! are audited and the proxy's routes are rebuilt from history on boot.

use crate::{ControlError, ControlPlane, ControlResult};
use aegis_ingress::{CertStatus, DomainRoute, Ingress, Target, TlsMode};
use aegis_projection::DomainState;
use aegis_types::ProjectId;
use serde_json::json;
use std::path::PathBuf;

/// A mapped domain with its certificate state.
#[derive(Debug, Clone)]
pub struct DomainView {
    pub domain: String,
    /// "acme", "files" or "off".
    pub tls: String,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub created_at: String,
    /// `None` while the proxy is disabled.
    pub cert: Option<CertStatus>,
}

impl ControlPlane {
    pub(crate) fn apply_domain_route(
        &self,
        ingress: &Ingress,
        project_id: ProjectId,
        domain: &DomainState,
    ) {
        ingress.set_domain(
            &domain.domain,
            DomainRoute {
                target: Target::Project(project_id.to_string()),
                tls: TlsMode::parse(&domain.tls).unwrap_or(TlsMode::Off),
                cert_path: domain.cert_path.as_ref().map(PathBuf::from),
                key_path: domain.key_path.as_ref().map(PathBuf::from),
            },
        );
    }

    fn domain_view(&self, domain: DomainState) -> DomainView {
        let cert = self.ingress().map(|i| i.cert_status(&domain.domain));
        DomainView {
            domain: domain.domain,
            tls: domain.tls,
            cert_path: domain.cert_path,
            key_path: domain.key_path,
            created_at: domain.created_at,
            cert,
        }
    }

    /// A project's domains, sorted by name.
    pub fn list_domains(&self, project_id: &ProjectId) -> ControlResult<Vec<DomainView>> {
        let project = self
            .inner
            .projection
            .get_project(project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        Ok(project
            .domains
            .into_iter()
            .map(|d| self.domain_view(d))
            .collect())
    }

    /// Serves a project on `domain`. `tls` is "acme" (automatic
    /// certificates), "files" (PEM files at `cert_path`/`key_path`) or
    /// "off". Mapping a domain the project already has updates its TLS.
    pub async fn map_domain(
        &self,
        project_id: ProjectId,
        domain: &str,
        tls: &str,
        cert_path: Option<String>,
        key_path: Option<String>,
    ) -> ControlResult<DomainView> {
        let domain =
            aegis_ingress::normalize_domain(domain).map_err(ControlError::InvalidArgument)?;
        let mode = TlsMode::parse(tls).ok_or_else(|| {
            ControlError::InvalidArgument("tls must be \"acme\", \"files\" or \"off\"".into())
        })?;
        let (cert_path, key_path) = match mode {
            TlsMode::Files => {
                let clean =
                    |p: Option<String>| p.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                let (Some(cert), Some(key)) = (clean(cert_path), clean(key_path)) else {
                    return Err(ControlError::InvalidArgument(
                        "TLS from files needs cert_path and key_path".into(),
                    ));
                };
                for path in [&cert, &key] {
                    if !std::path::Path::new(path).is_absolute() {
                        return Err(ControlError::InvalidArgument(format!(
                            "'{}' must be an absolute path",
                            path
                        )));
                    }
                }
                aegis_ingress::certs::load_cert_files(cert.as_ref(), key.as_ref()).map_err(
                    |e| ControlError::InvalidArgument(format!("Unusable certificate: {}", e)),
                )?;
                (Some(cert), Some(key))
            }
            _ => (None, None),
        };

        let _guard = self.inner.domain_lock.lock().await;
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        if let Some(owner) = self
            .inner
            .projection
            .get_projects()
            .into_iter()
            .find(|p| p.id != project_id && p.domains.iter().any(|d| d.domain == domain))
        {
            return Err(ControlError::FailedPrecondition(format!(
                "{} is already mapped to '{}'; remove it there first",
                domain, owner.name
            )));
        }
        if let Some(ingress) = self.ingress() {
            if ingress
                .routes()
                .domain(&domain)
                .is_some_and(|r| r.target == Target::Dashboard)
            {
                return Err(ControlError::FailedPrecondition(format!(
                    "{} serves the Aegis dashboard",
                    domain
                )));
            }
        }
        self.record(
            "DomainMapped",
            json!({
                "project_id": project_id.to_string(),
                "domain": domain,
                "tls": mode.as_str(),
                "cert_path": cert_path,
                "key_path": key_path,
            }),
        )
        .await?;
        let state = self
            .inner
            .projection
            .get_project(&project_id)
            .and_then(|p| p.domains.into_iter().find(|d| d.domain == domain))
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Domain was not recorded")))?;
        if let Some(ingress) = self.ingress() {
            self.apply_domain_route(ingress, project.id, &state);
        }
        Ok(self.domain_view(state))
    }

    /// Stops serving a project on `domain`. Stored certificates are kept, so
    /// mapping it again doesn't need a new one.
    pub async fn unmap_domain(&self, project_id: ProjectId, domain: &str) -> ControlResult<()> {
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let _guard = self.inner.domain_lock.lock().await;
        let project = self
            .inner
            .projection
            .get_project(&project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        if !project.domains.iter().any(|d| d.domain == domain) {
            return Err(ControlError::NotFound(format!(
                "{} is not mapped to '{}'",
                domain, project.name
            )));
        }
        self.record(
            "DomainUnmapped",
            json!({"project_id": project_id.to_string(), "domain": domain}),
        )
        .await?;
        if let Some(ingress) = self.ingress() {
            ingress.remove_domain(&domain);
        }
        Ok(())
    }

    /// The proxy's listeners, routes and certificates; `None` when disabled.
    pub fn proxy_status(&self) -> Option<aegis_ingress::IngressStatus> {
        self.ingress().map(|i| i.status())
    }

    /// Points the project's domains at `port`, if the proxy is enabled.
    pub(crate) fn route_upstream(&self, project_id: ProjectId, port: Option<u16>) {
        if let Some(ingress) = self.ingress() {
            ingress.set_upstream(&project_id.to_string(), port);
        }
    }

    /// Whether the next cutover of this project runs side by side behind
    /// the proxy: the proxy is enabled and the app has a domain.
    pub(crate) fn is_proxied(&self, project_id: &ProjectId) -> bool {
        self.ingress().is_some()
            && self
                .inner
                .projection
                .get_project(project_id)
                .is_some_and(|p| !p.domains.is_empty())
    }
}

/// A free local port for a new release, chosen by the OS.
pub(crate) fn allocate_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port())
}

/// Whether nothing listens on `port` right now.
pub(crate) fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Points a health check URL at the port a proxied release listens on.
///
/// The URL in `aegis.toml` names the app's configured port (e.g.
/// `http://127.0.0.1:3000/health`), but behind the proxy each release gets
/// its own port. The port is replaced when it is the configured one (or no
/// port is configured); a URL naming some other port is left alone, since
/// it doesn't depend on `PORT`.
pub(crate) fn rewrite_health_port(url: &str, configured: Option<u16>, port: u16) -> String {
    let (scheme, rest) = ["http://", "tcp://"]
        .iter()
        .find_map(|s| url.strip_prefix(s).map(|r| (*s, r)))
        .unwrap_or(("", url));
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let (host, current) = if let Some(end) = authority.strip_prefix('[').and_then(|a| a.find(']')) {
        let host = &authority[..end + 2];
        (host, authority[end + 2..].strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let current: Option<u16> = current.and_then(|p| p.parse().ok());
    let matches = match (current, configured) {
        (Some(c), Some(conf)) => c == conf,
        (Some(_), None) => true,
        // No port in the URL: port 80, which a release never gets.
        (None, _) => false,
    };
    if !matches {
        return url.to_string();
    }
    format!("{}{}:{}{}", scheme, host, port, path)
}

#[cfg(test)]
mod tests {
    use super::rewrite_health_port as rw;

    #[test]
    fn health_urls_follow_the_allocated_port() {
        assert_eq!(
            rw("http://127.0.0.1:3000/health", Some(3000), 41000),
            "http://127.0.0.1:41000/health"
        );
        assert_eq!(
            rw("http://localhost:3000", Some(3000), 41000),
            "http://localhost:41000"
        );
        assert_eq!(
            rw("tcp://127.0.0.1:3000", Some(3000), 41000),
            "tcp://127.0.0.1:41000"
        );
        assert_eq!(
            rw("127.0.0.1:3000/x", Some(3000), 41000),
            "127.0.0.1:41000/x"
        );
        assert_eq!(
            rw("http://[::1]:3000/health?full=1", Some(3000), 41000),
            "http://[::1]:41000/health?full=1"
        );
        // No configured port: the URL's port is the app's.
        assert_eq!(
            rw("http://127.0.0.1:8080/", None, 41000),
            "http://127.0.0.1:41000/"
        );
        // A separate admin port doesn't follow PORT.
        assert_eq!(
            rw("http://127.0.0.1:9090/health", Some(3000), 41000),
            "http://127.0.0.1:9090/health"
        );
        // No port at all means port 80.
        assert_eq!(
            rw("http://127.0.0.1/health", Some(3000), 41000),
            "http://127.0.0.1/health"
        );
    }

    #[test]
    fn allocated_ports_are_free() {
        let port = super::allocate_port().unwrap();
        assert!(port > 0);
        assert!(super::port_is_free(port));
        let held = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        assert!(!super::port_is_free(port));
        drop(held);
    }
}
