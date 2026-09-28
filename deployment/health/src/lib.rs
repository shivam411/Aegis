use async_trait::async_trait;
use std::time::Duration;
use tokio::net::TcpStream;

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
        let (host_port, path) = parse_http_target(target)?;
        let probe = async {
            let mut stream = match TcpStream::connect(&host_port).await {
                Ok(s) => s,
                Err(_) => return false,
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let req = format!(
                "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: AegisHealthChecker/1.0\r\nConnection: close\r\n\r\n",
                path, host_port
            );
            if stream.write_all(req.as_bytes()).await.is_err() {
                return false;
            }

            let mut buf = [0u8; 512];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let resp_str = String::from_utf8_lossy(&buf[..n]);
            resp_str
                .strip_prefix("HTTP/")
                .and_then(|r| r.split_whitespace().nth(1))
                .and_then(|code| code.parse::<u16>().ok())
                .map(|code| code < 500)
                .unwrap_or(false)
        };

        Ok(tokio::time::timeout(self.timeout, probe)
            .await
            .unwrap_or(false))
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
