//! A container app on a host without Docker: the deployment fails in its
//! Build stage with a clear reason, and nothing else is touched. Runs
//! everywhere (it points Aegis at a `docker` binary that doesn't exist).
//! Kept in its own test binary because it sets an environment variable.
#![cfg(unix)]

use aegis_control::{ControlPlane, ControlSettings, DeployRequest, RegisterProject};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use sqlx::sqlite::SqlitePoolOptions;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn container_app_without_docker_fails_clearly() {
    std::env::set_var("AEGIS_DOCKER_BIN", "/nonexistent/aegis-test/docker");
    let data = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    std::fs::write(src.path().join("Dockerfile"), "FROM busybox\n").unwrap();
    std::fs::write(
        src.path().join("aegis.toml"),
        "[project]\nname = \"nodocker\"\n[deploy]\nport = 3999\n[run]\nruntime = \"docker\"\n",
    )
    .unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    let mut settings = ControlSettings::new(data.path().to_path_buf());
    settings.sample_interval = Duration::from_secs(3600);
    let control = ControlPlane::new(EventStore::new(pool), ProcessSupervisor::new(), settings)
        .await
        .unwrap();
    control.start();

    let status = control.check_docker().await;
    assert!(!status.available);
    assert!(
        status.reason.as_deref().unwrap().contains("not installed"),
        "{status:?}"
    );
    let overview = control.resource_overview();
    assert_eq!(overview.docker.map(|d| d.available), Some(false));

    let project = control
        .register_project(RegisterProject {
            name: "nodocker".into(),
            source_dir: Some(src.path().to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let id = control
        .queue_deployment(DeployRequest {
            project_id: project,
            version: Some("r1".into()),
            trigger: "test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut deployment = None;
    for _ in 0..300 {
        deployment = control
            .get_deployment(&id)
            .filter(|d| !matches!(d.status.as_str(), "Queued" | "InProgress"));
        if deployment.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let deployment = deployment.expect("deployment finished");
    assert_eq!(deployment.status, "Failed");
    let failed = control
        .list_events(Some(&project), usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.event_type == "DeploymentFailed")
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&failed.payload_json).unwrap();
    assert_eq!(payload["stage"], "Build");
    let reason = payload["reason"].as_str().unwrap();
    assert!(reason.contains("Docker is not installed"), "{reason}");
    assert!(reason.contains("runtime = \"docker\""), "{reason}");
    assert!(control
        .resolve_project("nodocker")
        .unwrap()
        .current_release
        .is_none());
    control.shutdown().await;
}
