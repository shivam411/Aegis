//! Port safety and honest health checks: a release never starts on a port
//! another process holds (whose answers would pass the health check), and a
//! release that crashes right after its health check passed is not promoted.
#![cfg(unix)]

use aegis_control::{
    ControlError, ControlPlane, ControlSettings, DeployRequest, ProcessAction, RegisterProject,
};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_projection::DeploymentState;
use aegis_types::{DeploymentId, ProjectId};
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Harness {
    control: ControlPlane,
    store: EventStore,
    _data: TempDir,
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

const SERVE: &str = "exec python3 -m http.server $PORT --bind 127.0.0.1";

/// Answers one request (so the health check passes), then exits.
const CRASH_AFTER_FIRST_REQUEST: &str = r#"import http.server, os, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, *a):
        pass
server = http.server.HTTPServer(("127.0.0.1", int(os.environ["PORT"])), H)
server.handle_request()
print("crashing after the first request", flush=True)
sys.exit(3)
"#;

fn write_app(src: &Path, port: u16, start: &str) {
    std::fs::write(src.join("crash.py"), CRASH_AFTER_FIRST_REQUEST).unwrap();
    std::fs::write(
        src.join("aegis.toml"),
        format!(
            r#"
[project]
name = "img"

[build]
install_command = ""
build_command = "echo $AEGIS_RELEASE_VERSION > version.txt"
start_command = "{start}"

[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/api/health"
health_check_timeout_secs = 10
drain_timeout_secs = 2
"#
        ),
    )
    .unwrap();
}

async fn harness() -> Harness {
    let data = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_app(src.path(), port, SERVE);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    let store = EventStore::new(pool);
    let mut settings = ControlSettings::new(data.path().to_path_buf());
    settings.health_poll_interval = Duration::from_millis(100);
    settings.liveness_window = Duration::from_millis(1500);
    let control = ControlPlane::new(store.clone(), ProcessSupervisor::new(), settings)
        .await
        .unwrap();
    control.start();
    let project = control
        .register_project(RegisterProject {
            name: "img".into(),
            source_dir: Some(src.path().to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap();
    Harness {
        control,
        store,
        _data: data,
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

/// Another program on the app's port, answering 200 to everything (like
/// the app's old PM2 copy answering /api/health).
async fn occupy(port: u16) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 8\r\n\r\nimpostor")
                    .await;
            });
        }
    })
}

async fn event_types(h: &Harness) -> Vec<String> {
    h.store
        .get_events()
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event_type)
        .collect()
}

#[tokio::test]
async fn a_port_held_by_another_process_fails_the_deployment() {
    let h = harness().await;
    let impostor = occupy(h.port).await;
    assert_eq!(
        http_get(h.port, "/api/health").await.as_deref(),
        Some("impostor")
    );

    let d = deploy(&h, "r1").await;
    assert_eq!(d.status, "Failed", "{:?}", d);
    let error = d.error.clone().unwrap_or_default();
    assert!(
        error.contains(&format!("port {} is already in use", h.port)),
        "{error}"
    );
    assert!(error.contains("map a domain"), "{error}");
    // The impostor is named: it is this test process.
    assert!(
        error.contains(&format!("pid {}", std::process::id())),
        "{error}"
    );
    // Nothing was promoted, and the release's process never started.
    let project = h.control.resolve_project("img").unwrap();
    assert_eq!(project.current_release, None);
    assert_eq!(project.current_process, None);
    let events = event_types(&h).await;
    assert!(!events.iter().any(|e| e == "ReleasePromoted"), "{events:?}");
    assert!(!events.iter().any(|e| e == "ProcessStarted"), "{events:?}");
    assert_eq!(
        http_get(h.port, "/api/health").await.as_deref(),
        Some("impostor")
    );

    // Once the other process is gone, the same release deploys.
    impostor.abort();
    let _ = impostor.await;
    let d = deploy(&h, "r2").await;
    assert_eq!(d.status, "Success", "{:?}", d);
    assert_eq!(
        http_get(h.port, "/version.txt").await.as_deref(),
        Some("r2")
    );

    // Starting a stopped app on a taken port is refused with the same reason.
    h.control
        .process_action("img", ProcessAction::Stop)
        .await
        .unwrap();
    let impostor = occupy(h.port).await;
    let err = h
        .control
        .process_action("img", ProcessAction::Start)
        .await
        .unwrap_err();
    match err {
        ControlError::FailedPrecondition(m) => assert!(m.contains("already in use"), "{m}"),
        other => panic!("expected a failed precondition, got {other:?}"),
    }
    impostor.abort();
}

#[tokio::test]
async fn a_release_that_crashes_after_its_health_check_is_not_promoted() {
    let h = harness().await;

    // First deployment: nothing to roll back to, nothing promoted.
    write_app(h.src.path(), h.port, "exec python3 crash.py");
    let d = deploy(&h, "r1").await;
    assert_eq!(d.status, "Failed", "{:?}", d);
    let error = d.error.clone().unwrap_or_default();
    assert!(error.contains("after passing its health check"), "{error}");
    let project = h.control.resolve_project("img").unwrap();
    assert_eq!(project.current_release, None);
    assert!(!event_types(&h).await.iter().any(|e| e == "ReleasePromoted"));

    // A good release, then a crashing one: rolled back to the good one.
    write_app(h.src.path(), h.port, SERVE);
    let d = deploy(&h, "r2").await;
    assert_eq!(d.status, "Success", "{:?}", d);
    write_app(h.src.path(), h.port, "exec python3 crash.py");
    let d = deploy(&h, "r3").await;
    assert_eq!(d.status, "RolledBack", "{:?}", d);
    let project = h.control.resolve_project("img").unwrap();
    assert_eq!(project.current_release.as_deref(), Some("r2"));
    assert_eq!(
        http_get(h.port, "/version.txt").await.as_deref(),
        Some("r2")
    );
}
