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

#[async_trait]
impl HealthChecker for HttpHealthChecker {
    fn name(&self) -> &str {
        "HTTP"
    }

    async fn check_health(&self, target: &str) -> Result<bool, anyhow::Error> {
        let connect_fut = async {
            let mut stream = TcpStream::connect(target).await?;
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let req = format!(
                "GET /health HTTP/1.1\r\nHost: {}\r\nUser-Agent: AegisHealthChecker/1.0\r\nConnection: close\r\n\r\n",
                target
            );
            stream.write_all(req.as_bytes()).await?;

            let mut buf = [0u8; 512];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n > 0 {
                let resp_str = String::from_utf8_lossy(&buf[..n]);
                // Check if response status is HTTP 2xx or 3xx or 404/fallback
                if resp_str.starts_with("HTTP/") {
                    let parts: Vec<&str> = resp_str.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(code) = parts[1].parse::<u16>() {
                            return Ok(code < 500);
                        }
                    }
                }
            }
            Ok(true)
        };

        match tokio::time::timeout(self.timeout, connect_fut).await {
            Ok(res) => res,
            Err(_) => Ok(false),
        }
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
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK").await;
            }
        });

        let reach_tcp = tcp_checker.check_health(&local_addr).await.unwrap();
        assert!(reach_tcp);
        let reach_http = http_checker.check_health(&local_addr).await.unwrap();
        assert!(reach_http);
    }
}
