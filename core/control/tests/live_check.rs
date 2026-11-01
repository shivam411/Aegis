//! The live URL check: after a release is promoted, `[deploy] verify_url`
//! must answer; if not, the previous release is put back and the failure
//! keeps the HTTP status. Apps are `python3 -m http.server` with the release
//! version in `version.txt`.
#![cfg(unix)]

use aegis_control::{ControlPlane, ControlSettings, DeployRequest, RegisterProject};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_projection::DeploymentState;
use aegis_types::{DeploymentId, ProjectId};
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[allow(dead_code)]
struct Harness {
    control: ControlPlane,
    store: EventStore,
    data: TempDir,
    src: TempDir,
    port: u16,
    project_id: ProjectId,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn settings(data_dir: &Path) -> ControlSettings {
    let mut settings = ControlSettings::new(data_dir.to_path_buf());
    settings.health_poll_interval = Duration::from_millis(100);
    settings.liveness_window = Duration::from_millis(500);
    settings
}

async fn new_store() -> EventStore {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    EventStore::new(pool)
}

fn write_app(src: &Path, port: u16, build: &str, start: Option<&str>, extra: &str) {
    let start = start
        .map(str::to_string)
        .unwrap_or_else(|| "exec python3 -m http.server $PORT --bind 127.0.0.1".into());
    std::fs::write(
        src.join("aegis.toml"),
        format!(
            r#"
[project]
name = "web"

[build]
install_command = ""
build_command = "{build}"
start_command = "{start}"

[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/"
health_check_timeout_secs = 10
drain_timeout_secs = 2
{extra}
"#
        ),
    )
    .unwrap();
}

const VERSION_BUILD: &str = "echo $AEGIS_RELEASE_VERSION > version.txt";

async fn harness() -> Harness {
    let data = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_app(src.path(), port, VERSION_BUILD, None, "");
    let store = new_store().await;
    let control = ControlPlane::new(
        store.clone(),
        ProcessSupervisor::new(),
        settings(data.path()),
    )
    .await
    .unwrap();
    control.start();
    let project = control
        .register_project(RegisterProject {
            name: "web".into(),
            source_dir: Some(src.path().to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap();
    Harness {
        control,
        store,
        data,
        src,
        port,
        project_id: project.id,
    }
}

async fn deploy(h: &Harness, version: &str) -> DeploymentState {
    let id = h
        .control
        .queue_deployment(DeployRequest {
            project_id: h.project_id,
            version: Some(version.to_string()),
            trigger: "test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    wait_deployment(&h.control, id).await
}

async fn wait_deployment(control: &ControlPlane, id: DeploymentId) -> DeploymentState {
    for _ in 0..600 {
        if let Some(d) = control.get_deployment(&id) {
            if !matches!(d.status.as_str(), "Queued" | "InProgress") {
                return d;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("deployment {id} did not finish");
}

async fn http_get(port: u16, path: &str) -> Option<String> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .ok()?;
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).await.ok()?;
    buf.split("\r\n\r\n").nth(1).map(|b| b.trim().to_string())
}

async fn serving(port: u16) -> Option<String> {
    http_get(port, "/version.txt").await
}

async fn wait_serving(port: u16, version: &str) {
    for _ in 0..100 {
        if serving(port).await.as_deref() == Some(version) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{version} is not being served");
}

fn rewrite(h: &Harness, extra: &str) {
    write_app(h.src.path(), h.port, VERSION_BUILD, None, extra);
}

#[tokio::test]
async fn a_live_url_that_is_not_up_rolls_the_release_back_and_keeps_the_status() {
    let h = harness().await;
    assert_eq!(deploy(&h, "r1").await.status, "Success");
    wait_serving(h.port, "r1").await;

    // /missing.txt answers 404, which is not in 200-299.
    rewrite(
        &h,
        &format!(
            "verify_url = \"http://127.0.0.1:{}/missing.txt\"\nverify_expect = \"200-299\"\nverify_timeout_secs = 2",
            h.port
        ),
    );
    let failed = deploy(&h, "r2").await;
    assert_eq!(failed.status, "RolledBack", "{failed:?}");
    let error = failed.error.clone().unwrap();
    assert!(error.contains("HTTP 404"), "{error}");
    wait_serving(h.port, "r1").await;
    assert_eq!(
        h.control
            .resolve_project("web")
            .unwrap()
            .current_release
            .as_deref(),
        Some("r1")
    );

    let events = h
        .control
        .list_events(Some(&h.project_id), 1000)
        .await
        .unwrap();
    let failure = events
        .iter()
        .find(|e| e.event_type == "DeploymentFailed")
        .expect("DeploymentFailed");
    let payload: serde_json::Value = serde_json::from_str(&failure.payload_json).unwrap();
    assert_eq!(payload["stage"], "LiveCheck");
    assert_eq!(payload["http_status"], 404);
    assert_eq!(payload["error_code"], "http_404");
    assert_eq!(payload["rolled_back_to"], "r1");
    assert!(events
        .iter()
        .any(|e| e.event_type == "DeploymentRolledBack"));

    // A URL that is up keeps the release.
    rewrite(
        &h,
        &format!(
            "verify_url = \"http://127.0.0.1:{}/version.txt\"\nverify_expect = \"200-299\"\nverify_timeout_secs = 2",
            h.port
        ),
    );
    let ok = deploy(&h, "r3").await;
    assert_eq!(ok.status, "Success", "{ok:?}");
    wait_serving(h.port, "r3").await;

    // The manual check reports without touching anything, or rolls back on request.
    let up = h
        .control
        .check_live_url(h.project_id, None, false)
        .await
        .unwrap();
    assert!(up.ok, "{up:?}");
    assert_eq!(up.status, Some(200));
    let down = h
        .control
        .check_live_url(
            h.project_id,
            Some(format!("http://127.0.0.1:{}/missing.txt", h.port)),
            false,
        )
        .await
        .unwrap();
    assert!(!down.ok);
    assert_eq!(down.error_code.as_deref(), Some("http_404"));
    assert_eq!(down.rolled_back_to, None);
    let refused = h
        .control
        .check_live_url(h.project_id, Some("http://127.0.0.1:9/".into()), true)
        .await
        .unwrap();
    assert!(!refused.ok);
    assert_eq!(refused.error_code.as_deref(), Some("connection_failed"));
    assert_eq!(refused.rolled_back_to.as_deref(), Some("r1"));
    wait_serving(h.port, "r1").await;
}
