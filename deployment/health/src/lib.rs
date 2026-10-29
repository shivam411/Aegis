use async_trait::async_trait;
use std::time::Duration;
use tokio::net::TcpStream;

// async-trait marks the boxed futures #[must_use] (clippy::double_must_use since Rust 1.99).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait HealthChecker: Send + Sync {
    fn name(&self) -> &str;
    async fn check_health(&self, target: &str) -> Result<bool, anyhow::Error>;
}

pub struct TcpHealthChecker {
    pub timeout: Duration,
}

impl TcpHealthChecker {
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            timeout: Duration::from_millis(timeout_ms),
        }
    }
}

#[async_trait]
impl HealthChecker for TcpHealthChecker {
    fn name(&self) -> &str {
        "TCP"
    }

    async fn check_health(&self, target: &str) -> Result<bool, anyhow::Error> {
        match tokio::time::timeout(self.timeout, TcpStream::connect(target)).await {
            Ok(Ok(_)) => Ok(true),
            _ => Ok(false),
        }
    }
}

/// What one request to a URL showed: the HTTP status, or why there was none
/// (connection refused, timeout, TLS error, no valid response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub status: Option<u16>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

impl ProbeResult {
    /// "HTTP 503", or the transport error.
    pub fn describe(&self) -> String {
        match (self.status, &self.error) {
            (Some(code), _) => format!("HTTP {}", code),
            (None, Some(e)) => e.clone(),
            (None, None) => "no response".to_string(),
        }
    }

    /// Whether the status is within `lo..=hi`.
    pub fn status_in(&self, lo: u16, hi: u16) -> bool {
        self.status.is_some_and(|s| (lo..=hi).contains(&s))
    }
}

struct UrlParts {
    tls: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<UrlParts, anyhow::Error> {
    let (tls, rest) = match url.strip_prefix("https://") {
        Some(rest) => (true, rest),
        None => (false, url.strip_prefix("http://").unwrap_or(url)),
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() || authority.contains('@') || authority.contains(' ') {
        anyhow::bail!("'{}' is not a valid URL", url);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => {
            let port = p
                .parse::<u16>()
                .map_err(|_| anyhow::anyhow!("'{}' has an invalid port", url))?;
            (h.trim_matches(['[', ']']).to_string(), port)
        }
        _ => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    Ok(UrlParts {
        tls,
        host,
        port,
        path,
    })
}

/// One `GET` of an `http://` or `https://` URL (certificates are checked
/// against the Mozilla roots), returning the status line's code or the reason
/// there was none. Never follows redirects: a 301/302 is a status like any other.
pub async fn probe_url(url: &str, timeout: Duration) -> ProbeResult {
    let started = std::time::Instant::now();
    let finish = |status: Option<u16>, error: Option<String>| ProbeResult {
        status,
        error,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    let parts = match parse_url(url) {
        Ok(p) => p,
        Err(e) => return finish(None, Some(e.to_string())),
    };
    let run = async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let tcp = TcpStream::connect((parts.host.as_str(), parts.port))
            .await
            .map_err(|e| format!("connection failed: {}", e))?;
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: AegisUrlCheck/1.0\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            parts.path, parts.host
        );
        let mut buf = [0u8; 512];
        let n = if parts.tls {
            let roots =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("TLS setup failed: {}", e))?
            .with_root_certificates(roots)
            .with_no_client_auth();
            let name = rustls::pki_types::ServerName::try_from(parts.host.clone())
                .map_err(|_| format!("'{}' is not a valid host name", parts.host))?;
            let mut stream = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config))
                .connect(name, tcp)
                .await
                .map_err(|e| format!("TLS handshake failed: {}", e))?;
            stream
                .write_all(request.as_bytes())
                .await
                .map_err(|e| format!("request failed: {}", e))?;
            stream.read(&mut buf).await.unwrap_or(0)
        } else {
            let mut stream = tcp;
            stream
                .write_all(request.as_bytes())
                .await
                .map_err(|e| format!("request failed: {}", e))?;
            stream.read(&mut buf).await.unwrap_or(0)
        };
        let head = String::from_utf8_lossy(&buf[..n]);
        head.strip_prefix("HTTP/")
            .and_then(|r| r.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| "no valid HTTP response".to_string())
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(Ok(code)) => finish(Some(code), None),
        Ok(Err(e)) => finish(None, Some(e)),
        Err(_) => finish(
            None,
            Some(format!("no answer within {}s", timeout.as_secs().max(1))),
        ),
    }
}

pub struct HttpHealthChecker {
    pub timeout: Duration,
}

impl HttpHealthChecker {
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            timeout: Duration::from_millis(timeout_ms),
        }
    }
}

impl HttpHealthChecker {
    /// Like [`HealthChecker::check_health`], but says what came back.
    pub async fn probe(&self, target: &str) -> Result<ProbeResult, anyhow::Error> {
        let (host_port, path) = parse_http_target(target)?;
        Ok(probe_url(&format!("http://{}{}", host_port, path), self.timeout).await)
    }
}

/// Splits `http://host:port/path`, `host:port/path` or `host:port` into the
/// socket address and request path. A bare `host:port` probes `/health`.
pub fn parse_http_target(target: &str) -> Result<(String, String), anyhow::Error> {
    if target.starts_with("https://") {
        anyhow::bail!(
            "HTTPS health checks are not supported; probe the app over plain HTTP on localhost"
        );
    }
    let rest = target.strip_prefix("http://").unwrap_or(target);
    let (host_port, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/health"),
    };
    if host_port.is_empty() {
        anyhow::bail!("Health check target '{}' has no host", target);
    }
    let host_port = if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{}:80", host_port)
    };
    Ok((host_port, path.to_string()))
}

#[async_trait]
impl HealthChecker for HttpHealthChecker {
    fn name(&self) -> &str {
        "HTTP"
    }

    /// Healthy when the app answers the request with any non-5xx status: the
    /// probe checks that the server is up and serving, not the route itself.
    async fn check_health(&self, target: &str) -> Result<bool, anyhow::Error> {
        Ok(self.probe(target).await?.status_in(0, 499))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_tcp_and_http_health_checkers() {
        let tcp_checker = TcpHealthChecker::new(100);
        let http_checker = HttpHealthChecker::new(100);

        assert_eq!(tcp_checker.name(), "TCP");
        assert_eq!(http_checker.name(), "HTTP");

        // 1. Unreachable port -> false
        let unreach_tcp = tcp_checker.check_health("127.0.0.1:59999").await.unwrap();
        assert!(!unreach_tcp);
        let unreach_http = http_checker.check_health("127.0.0.1:59999").await.unwrap();
        assert!(!unreach_http);

        // 2. Bound local listener with mock HTTP server -> true
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap().to_string();

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                    .await;
            }
        });

        let reach_tcp = tcp_checker.check_health(&local_addr).await.unwrap();
        assert!(reach_tcp);
        let reach_http = http_checker.check_health(&local_addr).await.unwrap();
        assert!(reach_http);
        let reach_url = http_checker
            .check_health(&format!("http://{}/ready", local_addr))
            .await
            .unwrap();
        assert!(reach_url);
    }

    #[tokio::test]
    async fn test_http_checker_rejects_5xx_and_silent_servers() {
        let checker = HttpHealthChecker::new(300);
        for response in [&b"HTTP/1.1 503 Service Unavailable\r\n\r\n"[..], &b""[..]] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            tokio::spawn(async move {
                if let Ok((mut socket, _)) = listener.accept().await {
                    use tokio::io::AsyncWriteExt;
                    let _ = socket.write_all(response).await;
                }
            });
            assert!(!checker.check_health(&addr).await.unwrap());
        }
    }

    #[test]
    fn test_parse_http_target() {
        assert_eq!(
            parse_http_target("http://127.0.0.1:3000/health").unwrap(),
            ("127.0.0.1:3000".to_string(), "/health".to_string())
        );
        assert_eq!(
            parse_http_target("127.0.0.1:8080").unwrap(),
            ("127.0.0.1:8080".to_string(), "/health".to_string())
        );
        assert_eq!(
            parse_http_target("localhost/ping").unwrap(),
            ("localhost:80".to_string(), "/ping".to_string())
        );
        assert!(parse_http_target("https://x:1/").is_err());
    }
}
