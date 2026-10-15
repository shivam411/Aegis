//! Posts deployment failures, crashes and resource alerts to a Slack
//! incoming webhook (`[notifications] slack_webhook_url`).

use aegis_plugins::Plugin;
use aegis_types::Event;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Turns a project id into its name for messages.
pub type NameLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct SlackPlugin {
    pub webhook_url: String,
    names: NameLookup,
}

impl SlackPlugin {
    pub fn new(webhook_url: String) -> Self {
        Self::with_names(webhook_url, Arc::new(|_| None))
    }

    pub fn with_names(webhook_url: String, names: NameLookup) -> Self {
        Self { webhook_url, names }
    }

    /// The message for an event, or `None` for events not worth a ping.
    pub fn message(&self, event: &Event) -> Option<String> {
        let p: Value = serde_json::from_str(&event.payload_json).unwrap_or(Value::Null);
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let app = || {
            let id = s("project_id");
            (self.names)(&id).unwrap_or(if id.is_empty() { "An app".into() } else { id })
        };
        Some(match event.event_type.as_str() {
            "DeploymentCompleted" => {
                format!(":white_check_mark: *{}* {} is live", app(), s("version"))
            }
            "DeploymentFailed" => {
                format!(
                    ":x: Deploying *{}* failed at {}: {}",
                    app(),
                    s("stage"),
                    s("reason")
                )
            }
            "DeploymentRolledBack" => format!(
                ":warning: *{}*: the new release was unhealthy, so {} was restored ({})",
                app(),
                s("target_version"),
                s("reason")
            ),
            "RollbackFailed" => format!(
                ":x: Rolling *{}* back to {} failed: {}",
                app(),
                s("target_version"),
                s("reason")
            ),
            "ProcessFailed" => format!(
                ":rotating_light: *{}* stopped and will not restart: {}",
                app(),
                s("reason")
            ),
            "ResourcePressureDetected" => format!(":warning: {}", s("message")),
            _ => return None,
        })
    }
}

#[async_trait]
impl Plugin for SlackPlugin {
    fn name(&self) -> &str {
        "slack-notifications-plugin"
    }

    async fn on_init(&self) -> Result<(), anyhow::Error> {
        tracing::info!("Slack notifications enabled");
        Ok(())
    }

    async fn on_event(&self, event: &Event) -> Result<(), anyhow::Error> {
        let Some(text) = self.message(event) else {
            return Ok(());
        };
        let body = serde_json::json!({ "text": text }).to_string();
        let status =
            tokio::time::timeout(Duration::from_secs(15), post_json(&self.webhook_url, &body))
                .await
                .map_err(|_| anyhow::anyhow!("Slack did not answer within 15 s"))??;
        if !(200..300).contains(&status) {
            anyhow::bail!("Slack answered HTTP {}", status);
        }
        Ok(())
    }
}

/// POSTs `body` as JSON and returns the HTTP status. A tiny HTTP/1.1
/// client, enough for webhooks, so the daemon needs no HTTP client stack.
pub async fn post_json(url: &str, body: &str) -> Result<u16, anyhow::Error> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        anyhow::bail!("Webhook URL must start with https:// or http://");
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, p.parse()?),
        _ => (authority, if tls { 443 } else { 80 }),
    };
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: Aegis\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let tcp = tokio::net::TcpStream::connect((host, port)).await?;
    let response = if tls {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.to_string())?;
        let mut stream = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await?;
        exchange(&mut stream, &request).await?
    } else {
        let mut stream = tcp;
        exchange(&mut stream, &request).await?
    };
    let status = response
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Unexpected response from the webhook"))?;
    Ok(status)
}

async fn exchange<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    request: &str,
) -> Result<String, anyhow::Error> {
    stream.write_all(request.as_bytes()).await?;
    let mut buf = Vec::new();
    // The status line is all that matters; don't wait for a slow body.
    let mut chunk = [0u8; 1024];
    while !buf.windows(2).any(|w| w == b"\r\n") {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_messages() {
        let plugin = SlackPlugin::with_names(
            "https://hooks.slack.com/services/xxx".into(),
            Arc::new(|id| (id == "p1").then(|| "shop".to_string())),
        );
        assert_eq!(plugin.name(), "slack-notifications-plugin");
        let msg = |t: &str, p: &str| plugin.message(&Event::new(t, p));
        assert_eq!(
            msg(
                "DeploymentFailed",
                r#"{"project_id":"p1","stage":"Build","reason":"exit 1"}"#
            )
            .unwrap(),
            ":x: Deploying *shop* failed at Build: exit 1"
        );
        assert!(msg(
            "ResourcePressureDetected",
            r#"{"message":"shop went over its 64 MiB memory limit"}"#
        )
        .unwrap()
        .contains("64 MiB"));
        assert!(msg("ProcessStarted", "{}").is_none());
        assert!(msg("UserLoggedIn", "{}").is_none());
    }

    #[tokio::test]
    async fn test_posts_to_the_webhook() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if body.len() >= len {
                        break;
                    }
                }
            }
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
            String::from_utf8(req).unwrap()
        });
        let plugin = SlackPlugin::new(format!("http://127.0.0.1:{port}/services/T/B/x"));
        plugin
            .on_event(&Event::new(
                "ProcessFailed",
                r#"{"project_id":"p","reason":"crash loop"}"#,
            ))
            .await
            .unwrap();
        let req = server.await.unwrap();
        assert!(
            req.starts_with("POST /services/T/B/x HTTP/1.1\r\n"),
            "{req}"
        );
        assert!(req.contains("Content-Type: application/json"));
        let body: Value = serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(body["text"].as_str().unwrap().contains("crash loop"));

        // Errors surface (and are logged by the plugin manager).
        let closed = SlackPlugin::new("http://127.0.0.1:1/x".into());
        assert!(closed
            .on_event(&Event::new("ProcessFailed", "{}"))
            .await
            .is_err());
    }
}
