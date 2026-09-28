//! Tests for the HTTP API, driven through the router without a network socket
//! (except for the deployed app itself).
#![cfg(unix)]

use aegis_control::{ControlPlane, ControlSettings};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_web::{router, ApiDoc, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::watch;
use tower::ServiceExt;
use utoipa::OpenApi;

struct Api {
    state: AppState,
    shutdown: watch::Sender<bool>,
    _data: TempDir,
}

async fn api() -> Api {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    let data = TempDir::new().unwrap();
    let mut settings = ControlSettings::new(data.path().to_path_buf());
    settings.health_poll_interval = Duration::from_millis(100);
    let control = ControlPlane::new(EventStore::new(pool), ProcessSupervisor::new(), settings)
        .await
        .unwrap();
    control.start();
    let (shutdown, rx) = watch::channel(false);
    Api {
        state: AppState {
            control,
            version: "test".into(),
            shutdown: rx,
        },
        shutdown,
        _data: data,
    }
}

impl Api {
    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let res = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        self.send(
            Request::get(uri)
                .header("host", "127.0.0.1:8420")
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn call(&self, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "127.0.0.1:8420")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_app(dir: &Path, port: u16) {
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

async fn serving(port: u16) -> Option<String> {
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

async fn wait_deployment(api: &Api, id: &str) -> Value {
    for _ in 0..300 {
        let (_, d) = api.get(&format!("/api/v1/deployments/{id}")).await;
        if !matches!(d["status"].as_str(), Some("Queued" | "InProgress")) {
            return d;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("deployment {id} did not finish");
}

#[tokio::test]
async fn guard_blocks_rebinding_and_cross_site_requests() {
    let api = api().await;
    let req = |host: &str| {
        Request::get("/api/v1/status")
            .header("host", host)
            .body(Body::empty())
            .unwrap()
    };

    let (status, body) = api.send(req("evil.example:8420")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "forbidden");
    let (status, _) = api
        .send(Request::get("/api/v1/status").body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "missing Host");
    // A tunnel to another local port is fine.
    assert_eq!(api.send(req("localhost:9000")).await.0, StatusCode::OK);

    let with = |origin: &str| {
        Request::get("/api/v1/status")
            .header("host", "127.0.0.1:8420")
            .header("origin", origin)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        api.send(with("http://127.0.0.1:8420")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        api.send(with("http://localhost:3000")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        api.send(with("https://evil.example")).await.0,
        StatusCode::FORBIDDEN
    );
    let cross_site = Request::get("/api/v1/status")
        .header("host", "127.0.0.1:8420")
        .header("sec-fetch-site", "cross-site")
        .body(Body::empty())
        .unwrap();
    assert_eq!(api.send(cross_site).await.0, StatusCode::FORBIDDEN);

    // A form-style POST (what a cross-site page can send without preflight).
    let form = Request::post("/api/v1/projects")
        .header("host", "127.0.0.1:8420")
        .header("content-type", "text/plain")
        .body(Body::from(r#"{"name":"x"}"#))
        .unwrap();
    let (status, body) = api.send(form).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["error"]["code"], "invalid_argument");
    assert!(api.state.control.list_projects().is_empty());
}

#[tokio::test]
async fn projects_schedules_and_validation_errors() {
    let api = api().await;
    let src = TempDir::new().unwrap();

    let (status, project) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "shop", "source_dir": src.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    assert_eq!(project["name"], "shop");
    assert_eq!(project["branch"], "main");
    let id = project["id"].as_str().unwrap().to_string();

    let (_, list) = api.get("/api/v1/projects").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(api.get("/api/v1/projects/shop").await.1["id"], id);
    assert_eq!(
        api.get(&format!("/api/v1/projects/{id}")).await.1["name"],
        "shop"
    );
    let (status, body) = api.get("/api/v1/projects/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(
        api.get("/api/v1/nothing-here").await.0,
        StatusCode::NOT_FOUND
    );

    let (status, p) = api
        .call(
            "PUT",
            "/api/v1/projects/shop/schedule",
            json!({"hour": 2, "minute": 15}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        p["schedule"],
        json!({"hour": 2, "minute": 15, "branch": "main"})
    );
    let (status, _) = api
        .call("PUT", "/api/v1/projects/shop/schedule", json!({"hour": 24}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Bad bodies produce JSON errors, not plain-text rejections.
    let (status, body) = api
        .call(
            "POST",
            "/api/v1/projects/shop/deploy",
            json!({"stratgy": "x"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("stratgy"));
    let (status, body) = api
        .call(
            "POST",
            "/api/v1/projects/shop/deploy",
            json!({"strategy": "BlueGreen"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Phase 5"));
    let (status, _) = api
        .call("POST", "/api/v1/projects/ghost/deploy", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = api
        .call("POST", "/api/v1/projects/shop/rollback", json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "failed_precondition");
    let (status, _) = api
        .call("POST", "/api/v1/processes/shop/explode", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn full_lifecycle_over_http() {
    let api = api().await;
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_app(src.path(), port);
    api.call(
        "POST",
        "/api/v1/projects",
        json!({"name": "web", "source_dir": src.path()}),
    )
    .await;

    for version in ["r1", "r2"] {
        let (status, accepted) = api
            .call(
                "POST",
                "/api/v1/projects/web/deploy",
                json!({"version": version}),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(accepted["project_name"], "web");
        let d = wait_deployment(&api, accepted["deployment_id"].as_str().unwrap()).await;
        assert_eq!(d["status"], "Success", "{d}");
        assert_eq!(serving(port).await.as_deref(), Some(version));
    }

    let (_, project) = api.get("/api/v1/projects/web").await;
    assert_eq!(project["current_release"], "r2");
    assert_eq!(project["process"]["status"], "Running");
    let (_, releases) = api.get("/api/v1/projects/web/releases").await;
    assert_eq!(releases.as_array().unwrap().len(), 2);
    let (_, deployments) = api.get("/api/v1/deployments?project=web").await;
    assert_eq!(deployments.as_array().unwrap().len(), 2);

    let (status, p) = api
        .call("POST", "/api/v1/processes/web/restart", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["restart_count"], 1);
    let (_, p) = api
        .call("POST", "/api/v1/processes/web/stop", json!({}))
        .await;
    assert_eq!(p["status"], "Stopped");
    assert_eq!(serving(port).await, None);
    let (status, _) = api
        .call("POST", "/api/v1/processes/web/start", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..50 {
        if serving(port).await.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    serving(port).await.expect("serving again after start");

    let (_, logs) = api.get("/api/v1/processes/web/logs?lines=50").await;
    assert!(logs["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["line"].as_str().unwrap().contains("GET /version.txt")));

    let (status, result) = api
        .call("POST", "/api/v1/projects/web/rollback", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["version"], "r1");
    assert_eq!(serving(port).await.as_deref(), Some("r1"));

    let (_, events) = api.get("/api/v1/events?project=web&limit=500").await;
    let types: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"ReleasePromoted"));
    assert!(types.contains(&"RollbackCompleted"));
    // Payloads are real JSON, not strings.
    assert!(events[0]["payload"].is_object());

    let (_, status) = api.get("/api/v1/status").await;
    assert_eq!(status["project_count"], 1);
    assert_eq!(status["running_processes"], 1);
    api.state.control.shutdown().await;
}

/// Reads SSE frames until `pred` matches the accumulated text.
async fn read_sse_until(body: &mut Body, pred: impl Fn(&str) -> bool) -> String {
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

#[tokio::test]
async fn event_stream_filters_by_project_and_ends_on_shutdown() {
    let api = api().await;
    let src = TempDir::new().unwrap();
    for name in ["a", "b"] {
        api.call(
            "POST",
            "/api/v1/projects",
            json!({"name": name, "source_dir": src.path()}),
        )
        .await;
    }
    let res = router(api.state.clone())
        .oneshot(
            Request::get("/api/v1/events/stream?project=b")
                .header("host", "127.0.0.1:8420")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "text/event-stream");
    let mut body = res.into_body();

    api.call("PUT", "/api/v1/projects/a/schedule", json!({"hour": 1}))
        .await;
    api.call("PUT", "/api/v1/projects/b/schedule", json!({"hour": 3}))
        .await;
    let text = read_sse_until(&mut body, |t| t.contains("event: ScheduleConfigured")).await;
    assert!(text.contains("\"hour\":3"), "{text}");
    assert!(
        !text.contains("\"hour\":1"),
        "project filter leaked: {text}"
    );

    api.shutdown.send(true).unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = body.frame().await {
            frame.unwrap();
        }
    })
    .await;
    assert!(end.is_ok(), "stream should end on shutdown");
}

#[tokio::test]
async fn log_stream_sends_history_then_follows() {
    let api = api().await;
    let src = TempDir::new().unwrap();
    std::fs::write(
        src.path().join("aegis.toml"),
        "[build]\ninstall_command = \"\"\nbuild_command = \"\"\nstart_command = \"echo hello-from-app; exec sleep 60\"\n[deploy]\nhealth_check_url = \"\"\nhealth_check_timeout_secs = 5\n",
    )
    .unwrap();
    api.call(
        "POST",
        "/api/v1/projects",
        json!({"name": "worker", "source_dir": src.path()}),
    )
    .await;
    let (_, accepted) = api
        .call("POST", "/api/v1/projects/worker/deploy", json!({}))
        .await;
    let d = wait_deployment(&api, accepted["deployment_id"].as_str().unwrap()).await;
    assert_eq!(d["status"], "Success", "{d}");
    api.call("POST", "/api/v1/processes/worker/stop", json!({}))
        .await;

    let res = router(api.state.clone())
        .oneshot(
            Request::get("/api/v1/processes/worker/logs/stream?lines=20")
                .header("host", "127.0.0.1:8420")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = res.into_body();
    // History from the stopped run, including the supervisor's own message...
    let text = read_sse_until(&mut body, |t| t.contains("Stopped (requested)")).await;
    assert!(text.contains("hello-from-app"), "{text}");
    assert!(text.contains("event: log"));
    // ...then the same stream follows the next run.
    api.call("POST", "/api/v1/processes/worker/start", json!({}))
        .await;
    let more = read_sse_until(&mut body, |t| t.contains("hello-from-app")).await;
    assert!(more.contains("\"stream\":\"out\""), "{more}");
    api.state.control.shutdown().await;
}

#[tokio::test]
async fn openapi_spec_is_served_and_checked_in() {
    let api = api().await;
    let (status, spec) = api.get("/api/v1/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    assert!(spec["paths"]["/api/v1/projects/{project}/deploy"]["post"].is_object());

    // docs/openapi.json must match the code. Regenerate with:
    //   UPDATE_OPENAPI=1 cargo test -p aegis-web openapi
    let generated = ApiDoc::openapi().to_pretty_json().unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/openapi.json");
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, &generated).unwrap();
    }
    let checked_in = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        checked_in == generated,
        "docs/openapi.json is out of date; run UPDATE_OPENAPI=1 cargo test -p aegis-web openapi"
    );
}
