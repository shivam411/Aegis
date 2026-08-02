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

    #[tokio::test]
    async fn test_tcp_health_checker() {
        let checker = TcpHealthChecker::new(100);
        // Connecting to unreachable port should fail gracefully returning false
        let result = checker.check_health("127.0.0.1:59999").await.unwrap();
        assert!(!result);
    }
}
