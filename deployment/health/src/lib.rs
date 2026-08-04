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
        // TCP check fallback for zero-dependency health monitoring
        match tokio::time::timeout(self.timeout, TcpStream::connect(target)).await {
            Ok(Ok(_)) => Ok(true),
            _ => Ok(false),
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

        // 2. Bound local listener -> true
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap().to_string();

        let reach_tcp = tcp_checker.check_health(&local_addr).await.unwrap();
        assert!(reach_tcp);
        let reach_http = http_checker.check_health(&local_addr).await.unwrap();
        assert!(reach_http);
    }
}
