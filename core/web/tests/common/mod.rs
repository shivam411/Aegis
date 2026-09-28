//! Shared harness for the HTTP API tests.
#![allow(dead_code)]

use aegis_auth::{AuthConfig, AuthStore, TokenScope, ADMIN_USER};
use aegis_control::{ControlPlane, ControlSettings};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_web::{router, AppState, WebSettings};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::watch;
use tower::ServiceExt;

pub struct Api {
    pub state: AppState,
    pub shutdown: watch::Sender<bool>,
    /// A deploy-scoped API token.
    pub token: String,
    _data: TempDir,
}

pub const ADMIN_PASSWORD: &str = "test-admin-password";

pub async fn api() -> Api {
    api_with(WebSettings::default()).await
}

pub async fn api_with(settings_web: WebSettings) -> Api {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    let auth = AuthStore::new(pool.clone(), AuthConfig::default()).unwrap();
    auth.set_password(ADMIN_USER, ADMIN_PASSWORD).await.unwrap();
    let token = auth
        .create_token("tests", TokenScope::Deploy)
        .await
        .unwrap()
        .token;
    let data = TempDir::new().unwrap();
    let mut settings = ControlSettings::new(data.path().to_path_buf());
    settings.health_poll_interval = Duration::from_millis(100);
    let control = ControlPlane::new(EventStore::new(pool), ProcessSupervisor::new(), settings)
        .await
        .unwrap();
    control.start();
    let (shutdown, rx) = watch::channel(false);
    Api {
        state: AppState::new(control, auth, settings_web, "test".into(), rx),
        shutdown,
        token,
        _data: data,
    }
}

impl Api {
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }

    pub async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let res = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    pub async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.send(
            Request::get(uri)
                .header("host", "127.0.0.1:8420")
                .header("authorization", self.bearer())
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    pub async fn call(&self, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "127.0.0.1:8420")
                .header("authorization", self.bearer())
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn write_app(dir: &Path, port: u16) {
    std::fs::write(
        dir.join("aegis.toml"),
        format!(
            r#"
[build]
install_command = ""
build_command = "echo $AEGIS_RELEASE_VERSION > version.txt"
start_command = "exec python3 -m http.server $PORT --bind 127.0.0.1"
[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/"
health_check_timeout_secs = 10
drain_timeout_secs = 2
"#
        ),
    )
    .unwrap();
}

pub async fn serving(port: u16) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .ok()?;
    s.write_all(b"GET /version.txt HTTP/1.0\r\n\r\n")
        .await
        .ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.ok()?;
    buf.split("\r\n\r\n").nth(1).map(|b| b.trim().to_string())
}

pub async fn wait_deployment(api: &Api, id: &str) -> Value {
    for _ in 0..300 {
        let (_, d) = api.get(&format!("/api/v1/deployments/{id}")).await;
        if !matches!(d["status"].as_str(), Some("Queued" | "InProgress")) {
            return d;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("deployment {id} did not finish");
}

/// Reads SSE frames until `pred` matches the accumulated text.
pub async fn read_sse_until(body: &mut Body, pred: impl Fn(&str) -> bool) -> String {
    let mut text = String::new();
    while !pred(&text) {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("timed out waiting for SSE data")
            .expect("stream ended early")
            .unwrap();
        if let Ok(data) = frame.into_data() {
            text.push_str(&String::from_utf8_lossy(&data));
        }
    }
    text
}
