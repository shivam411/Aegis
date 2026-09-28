//! End-to-end control plane tests with real processes: each app is
//! `python3 -m http.server`, and every build writes its release version into
//! `version.txt`, so `GET /version.txt` shows which release is serving.
#![cfg(unix)]

use aegis_control::{ControlPlane, ControlSettings, DeployRequest, ProcessAction, RegisterProject};
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

#[tokio::test]
async fn deploy_redeploy_process_control_and_rollback() {
    let h = harness().await;

    let d1 = deploy(&h, "r1").await;
    assert_eq!(d1.status, "Success", "{:?}", d1);
    assert_eq!(serving(h.port).await.as_deref(), Some("r1"));

    let d2 = deploy(&h, "r2").await;
    assert_eq!(d2.status, "Success", "{:?}", d2);
    assert_eq!(serving(h.port).await.as_deref(), Some("r2"));

    let project = h.control.resolve_project("web").unwrap();
    assert_eq!(project.current_release.as_deref(), Some("r2"));
    let statuses: Vec<_> = h
        .control
        .list_releases(&h.project_id)
        .into_iter()
        .map(|r| (r.version, r.status))
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("r1".to_string(), "Inactive".to_string()),
            ("r2".to_string(), "Active".to_string())
        ]
    );

    // Stop: the app goes down and is recorded as wanted-down.
    let stopped = h
        .control
        .process_action("web", ProcessAction::Stop)
        .await
        .unwrap();
    assert_eq!(stopped.status, "Stopped");
    assert_eq!(serving(h.port).await, None);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.control.resolve_process("web").unwrap().desired, "stopped");
    // Starting a stopped process works; starting it twice doesn't.
    let started = h
        .control
        .process_action("web", ProcessAction::Start)
        .await
        .unwrap();
    assert_eq!(started.status, "Running");
    assert!(h
        .control
        .process_action("web", ProcessAction::Start)
        .await
        .is_err());
    wait_serving(h.port, "r2").await;

    // Restart changes the OS pid.
    let before = h.control.resolve_process("web").unwrap().pid;
    let restarted = h
        .control
        .process_action("web", ProcessAction::Restart)
        .await
        .unwrap();
    assert_ne!(restarted.pid, before);
    wait_serving(h.port, "r2").await;

    // Logs from the app are captured (http.server logs requests to stderr).
    let logs = h.control.process_logs(&restarted, 50);
    assert!(
        logs.iter().any(|l| l.line.contains("GET /version.txt")),
        "{logs:?}"
    );

    // Rollback without a version goes to the previous release.
    let to = h.control.rollback(h.project_id, None).await.unwrap();
    assert_eq!(to, "r1");
    assert_eq!(serving(h.port).await.as_deref(), Some("r1"));
    assert_eq!(
        h.control
            .resolve_project("web")
            .unwrap()
            .current_release
            .as_deref(),
        Some("r1")
    );
    // Rolling back to the live release is refused.
    assert!(h
        .control
        .rollback(h.project_id, Some("r1".into()))
        .await
        .is_err());

    // The audit trail has the whole story, in order.
    let types: Vec<String> = h
        .control
        .list_events(Some(&h.project_id), 1000)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event_type)
        .collect();
    for expected in [
        "ProjectCreated",
        "DeploymentQueued",
        "BuildStageClone",
        "ReleaseCreated",
        "ReleasePromoted",
        "DeploymentCompleted",
        "ProcessStopRequested",
        "RollbackCompleted",
    ] {
        assert!(
            types.iter().any(|t| t == expected),
            "missing {expected} in {types:?}"
        );
    }
    h.control.shutdown().await;
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

#[tokio::test]
async fn failed_build_leaves_live_release_untouched() {
    let h = harness().await;
    assert_eq!(deploy(&h, "r1").await.status, "Success");
    let live_pid = h.control.resolve_process("web").unwrap().pid;

    write_app(h.src.path(), h.port, "echo compile error; exit 1", None, "");
    let failed = deploy(&h, "r2").await;
    assert_eq!(failed.status, "Failed");
    assert_eq!(failed.stage.as_deref(), Some("Build"));
    assert!(failed.error.unwrap().contains("code 1"));

    assert_eq!(serving(h.port).await.as_deref(), Some("r1"));
    assert_eq!(h.control.resolve_process("web").unwrap().pid, live_pid);
    let release_dir = h
        .data
        .path()
        .join(format!("projects/{}/releases/r2", h.project_id));
    assert!(
        !release_dir.exists(),
        "failed build output should be removed"
    );
    h.control.shutdown().await;
}

#[tokio::test]
async fn unhealthy_release_is_rolled_back_automatically() {
    let h = harness().await;
    assert_eq!(deploy(&h, "r1").await.status, "Success");

    write_app(
        h.src.path(),
        h.port,
        VERSION_BUILD,
        Some("echo starting; exit 3"),
        "",
    );
    let failed = deploy(&h, "r2").await;
    assert_eq!(failed.status, "RolledBack", "{failed:?}");
    assert!(failed.error.unwrap().contains("code 3"));

    wait_serving(h.port, "r1").await;
    let project = h.control.resolve_project("web").unwrap();
    assert_eq!(project.current_release.as_deref(), Some("r1"));
    let r2 = h
        .control
        .list_releases(&h.project_id)
        .into_iter()
        .find(|r| r.version == "r2")
        .unwrap();
    assert_eq!(r2.status, "Failed");
    let events = h
        .control
        .list_events(Some(&h.project_id), 1000)
        .await
        .unwrap();
    let failed_event = events
        .iter()
        .find(|e| e.event_type == "DeploymentFailed")
        .unwrap();
    assert!(
        failed_event.payload_json.contains("starting"),
        "log tail is included"
    );
    h.control.shutdown().await;
}

#[tokio::test]
async fn daemon_restart_restores_running_apps_only() {
    let h = harness().await;
    assert_eq!(deploy(&h, "r1").await.status, "Success");
    h.control.shutdown().await;
    assert_eq!(serving(h.port).await, None);

    // A new control plane over the same event store, as after a daemon restart.
    let control = ControlPlane::new(
        h.store.clone(),
        ProcessSupervisor::new(),
        settings(h.data.path()),
    )
    .await
    .unwrap();
    control.start();
    assert_eq!(control.recover().await, 1);
    wait_serving(h.port, "r1").await;

    // Once an operator stops it, a restart leaves it down.
    control
        .process_action("web", ProcessAction::Stop)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    control.shutdown().await;
    let control = ControlPlane::new(
        h.store.clone(),
        ProcessSupervisor::new(),
        settings(h.data.path()),
    )
    .await
    .unwrap();
    control.start();
    assert_eq!(control.recover().await, 0);
    assert_eq!(serving(h.port).await, None);
}

#[tokio::test]
async fn retention_prunes_old_releases_and_validation_errors() {
    let h = harness().await;
    write_app(
        h.src.path(),
        h.port,
        "true",
        Some("exec sleep 60"),
        "max_retained_versions = 2\n",
    );
    // No health URL: healthy means "stays up for the liveness window".
    let toml = std::fs::read_to_string(h.src.path().join("aegis.toml"))
        .unwrap()
        .replace(
            &format!("health_check_url = \"http://127.0.0.1:{}/\"", h.port),
            "health_check_url = \"\"",
        );
    std::fs::write(h.src.path().join("aegis.toml"), toml).unwrap();

    for v in ["r1", "r2", "r3"] {
        assert_eq!(deploy(&h, v).await.status, "Success");
    }
    let releases_dir = h
        .data
        .path()
        .join(format!("projects/{}/releases", h.project_id));
    assert!(!releases_dir.join("r1").exists());
    assert!(releases_dir.join("r2").exists());
    assert!(releases_dir.join("r3").exists());
    let r1 = h
        .control
        .list_releases(&h.project_id)
        .into_iter()
        .find(|r| r.version == "r1")
        .unwrap();
    assert_eq!(r1.status, "Archived");
    // An archived release can't be rolled back to.
    assert!(h
        .control
        .rollback(h.project_id, Some("r1".into()))
        .await
        .is_err());

    // A raw event can't smuggle in a path-escaping release name.
    let escape = h.data.path().join("escaped");
    h.control
        .record(
            "DeploymentQueued",
            serde_json::json!({
                "deployment_id": DeploymentId::new().to_string(),
                "project_id": h.project_id.to_string(),
                "version": "../../../escaped",
            }),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!escape.exists());
    let last = h
        .control
        .list_deployments(Some(&h.project_id))
        .pop()
        .unwrap();
    assert_eq!(last.status, "Failed");
    assert!(last.error.unwrap().contains("Invalid release version"));

    // Bad requests are rejected before anything is queued.
    for req in [
        DeployRequest {
            project_id: h.project_id,
            strategy: Some("Rolling".into()),
            ..Default::default()
        },
        DeployRequest {
            project_id: h.project_id,
            version: Some("../escape".into()),
            ..Default::default()
        },
        DeployRequest {
            project_id: ProjectId::new(),
            ..Default::default()
        },
    ] {
        assert!(h.control.queue_deployment(req).await.is_err());
    }
    h.control.shutdown().await;
}

#[tokio::test]
async fn rollback_and_retention_follow_live_order_not_build_order() {
    let h = harness().await;
    for v in ["r1", "r2"] {
        assert_eq!(deploy(&h, v).await.status, "Success");
    }
    // r2 -> r1: now r1 is live and r2 was live before it.
    assert_eq!(h.control.rollback(h.project_id, None).await.unwrap(), "r1");
    // Deploy r3 with the default retention of 2: keep r3 and r1 (the last
    // live release), not r2 (merely the newest build).
    assert_eq!(deploy(&h, "r3").await.status, "Success");
    let releases_dir = h
        .data
        .path()
        .join(format!("projects/{}/releases", h.project_id));
    assert!(
        releases_dir.join("r1").exists(),
        "previously live release was pruned"
    );
    assert!(!releases_dir.join("r2").exists());
    assert_eq!(h.control.rollback(h.project_id, None).await.unwrap(), "r1");
    assert_eq!(serving(h.port).await.as_deref(), Some("r1"));
    h.control.shutdown().await;
}
