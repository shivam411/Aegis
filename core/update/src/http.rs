//! A small HTTPS client for the GitHub API and release downloads.
//!
//! TLS certificates are checked against the Mozilla root store bundled at
//! build time (webpki-roots). Only `https://` URLs are fetched; plain
//! `http://` is allowed for loopback addresses only when the API base was
//! overridden for tests (see [`crate::api_base_override`]).

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use hyper::{Method, Request, StatusCode, Uri};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::sync::Arc;
use std::time::Duration;

const MAX_REDIRECTS: usize = 5;

#[derive(Clone)]
pub struct Fetcher {
    client: Client<hyper_rustls::HttpsConnector<HttpConnector>, Empty<Bytes>>,
    user_agent: String,
    allow_loopback_http: bool,
}

/// True for `localhost`, `127.0.0.0/8` and `[::1]`.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

impl Fetcher {
    pub fn new(user_agent: &str, allow_loopback_http: bool) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
            user_agent: user_agent.to_string(),
            allow_loopback_http,
        }
    }

    fn check_url(&self, uri: &Uri) -> anyhow::Result<()> {
        let host = uri.host().unwrap_or_default();
        match uri.scheme_str() {
            Some("https") if !host.is_empty() => Ok(()),
            Some("http") if self.allow_loopback_http && is_loopback_host(host) => Ok(()),
            _ => anyhow::bail!("Refusing to fetch {} (only https:// URLs)", uri),
        }
    }

    /// GETs `url`, following up to 5 redirects, and returns the body if it
    /// is at most `limit` bytes. The whole exchange must finish in `timeout`.
    pub async fn get(
        &self,
        url: &str,
        accept: &str,
        limit: usize,
        timeout: Duration,
    ) -> anyhow::Result<Vec<u8>> {
        tokio::time::timeout(timeout, self.get_inner(url, accept, limit))
            .await
            .map_err(|_| {
                anyhow::anyhow!("Timed out after {}s fetching {}", timeout.as_secs(), url)
            })?
    }

    async fn get_inner(&self, url: &str, accept: &str, limit: usize) -> anyhow::Result<Vec<u8>> {
        let mut uri: Uri = url
            .parse()
            .map_err(|e| anyhow::anyhow!("Bad URL {}: {}", url, e))?;
        for _ in 0..=MAX_REDIRECTS {
            self.check_url(&uri)?;
            let req = Request::builder()
                .method(Method::GET)
                .uri(uri.clone())
                .header(hyper::header::USER_AGENT, &self.user_agent)
                .header(hyper::header::ACCEPT, accept)
                .header("X-GitHub-Api-Version", "2022-11-28")
                .body(Empty::<Bytes>::new())?;
            let res = self
                .client
                .request(req)
                .await
                .map_err(|e| anyhow::anyhow!("Cannot reach {}: {}", host_of(&uri), e))?;
            let status = res.status();
            if status.is_redirection() {
                let location = res
                    .headers()
                    .get(hyper::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| anyhow::anyhow!("Redirect without a Location from {}", uri))?;
                uri = resolve(&uri, location)?;
                continue;
            }
            if let Some(len) = res
                .headers()
                .get(hyper::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
            {
                if len > limit as u64 {
                    anyhow::bail!("{} is too large ({} bytes)", uri, len);
                }
            }
            let body = Limited::new(res.into_body(), limit)
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("Reading {} failed: {}", uri, e))?
                .to_bytes();
            if status != StatusCode::OK {
                let detail = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("message")?.as_str().map(str::to_string))
                    .unwrap_or_default();
                anyhow::bail!("{} answered {} {}", host_of(&uri), status, detail);
            }
            return Ok(body.to_vec());
        }
        anyhow::bail!("Too many redirects fetching {}", url)
    }
}

fn host_of(uri: &Uri) -> String {
    uri.authority()
        .map(|a| a.to_string())
        .unwrap_or_else(|| uri.to_string())
}

/// Resolves a Location header against the URL that sent it.
fn resolve(base: &Uri, location: &str) -> anyhow::Result<Uri> {
    if location.starts_with('/') && !location.starts_with("//") {
        let scheme = base.scheme_str().unwrap_or("https");
        let authority = base.authority().map(|a| a.as_str()).unwrap_or_default();
        return format!("{}://{}{}", scheme, authority, location)
            .parse()
            .map_err(|e| anyhow::anyhow!("Bad redirect {}: {}", location, e));
    }
    location
        .parse()
        .map_err(|e| anyhow::anyhow!("Bad redirect {}: {}", location, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        for h in ["localhost", "127.0.0.1", "127.1.2.3", "[::1]", "::1"] {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in [
            "example.com",
            "10.0.0.1",
            "github.com",
            "localhost.evil.com",
        ] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }

    #[test]
    fn only_https_unless_loopback_was_allowed() {
        let strict = Fetcher::new("t", false);
        let test = Fetcher::new("t", true);
        let ok = |f: &Fetcher, u: &str| f.check_url(&u.parse().unwrap()).is_ok();
        assert!(ok(&strict, "https://api.github.com/x"));
        assert!(!ok(&strict, "http://api.github.com/x"));
        assert!(!ok(&strict, "http://127.0.0.1:9/x"));
        assert!(ok(&test, "http://127.0.0.1:9/x"));
        assert!(!ok(&test, "http://example.com/x"));
        assert!(!ok(&test, "ftp://127.0.0.1/x"));
    }

    #[test]
    fn resolves_redirects() {
        let base: Uri = "https://github.com/o/r/releases/download/v1/x"
            .parse()
            .unwrap();
        assert_eq!(
            resolve(&base, "/elsewhere").unwrap().to_string(),
            "https://github.com/elsewhere"
        );
        assert_eq!(
            resolve(&base, "https://objects.example.com/a?b=c")
                .unwrap()
                .to_string(),
            "https://objects.example.com/a?b=c"
        );
    }
}
