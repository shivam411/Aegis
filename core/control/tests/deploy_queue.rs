//! Only one deployment runs at a time on the server; the others wait in the
//! order they were queued and start when the one before them is done.
#![cfg(unix)]

use aegis_control::{ControlPlane, ControlSettings, DeployRequest, RegisterProject};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use sqlx::sqlite::SqlitePoolOptions;
use std::time::Duration;
use tempfile::TempDir;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn deployments_run_one_at_a_time_in_queue_order() {
    run(1).await;
}

#[tokio::test]
async fn the_limit_can_be_raised() {
    run(2).await;
}

async fn run(limit: usize) {
    let data = TempDir::new().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    let store = EventStore::new(pool);
    let mut settings = ControlSettings::new(data.path().to_path_buf());
    settings.health_poll_interval = Duration::from_millis(100);
    settings.liveness_window = Duration::from_millis(300);
    settings.max_parallel_deployments = limit;
    let control = ControlPlane::new(store.clone(), ProcessSupervisor::new(), settings)
        .await
        .unwrap();
    control.start();

    let mut srcs = Vec::new();
    let mut ids = Vec::new();
    for name in ["one", "two", "three"] {
        let src = TempDir::new().unwrap();
        let port = free_port();
        std::fs::write(
            src.path().join("aegis.toml"),
            format!(
                r#"
[project]
name = "{name}"

[build]
install_command = ""
build_command = "sleep 1"
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
        let project = control
            .register_project(RegisterProject {
                name: name.into(),
                source_dir: Some(src.path().to_path_buf()),
                ..Default::default()
            })
            .await
            .unwrap();
        srcs.push(src);
        ids.push(project.id);
    }

    // Queue all three at once.
    let mut deployments = Vec::new();
    for id in &ids {
        deployments.push(
            control
                .queue_deployment(DeployRequest {
                    project_id: *id,
                    trigger: "test".into(),
                    ..Default::default()
                })
                .await
                .unwrap(),
        );
    }

    // While the first builds, the others are still queued.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let busy = deployments
        .iter()
        .filter(|d| control.get_deployment(d).unwrap().status == "InProgress")
        .count();
    assert_eq!(busy, limit, "{limit} deployment(s) run at a time");

    for d in &deployments {
        for _ in 0..300 {
            let state = control.get_deployment(d).unwrap();
            if !matches!(state.status.as_str(), "Queued" | "InProgress") {
                assert_eq!(state.status, "Success", "{state:?}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // Started/Completed alternate: a deployment starts only after the one
    // before it completed, and in the order queued.
    let events = store.get_events().await.unwrap();
    let flow: Vec<(String, String)> = events
        .iter()
        .filter(|e| {
            matches!(
                e.event_type.as_str(),
                "DeploymentStarted" | "DeploymentCompleted"
            )
        })
        .map(|e| {
            let v: serde_json::Value = serde_json::from_str(&e.payload_json).unwrap();
            (
                e.event_type.clone(),
                v["deployment_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = deployments
        .iter()
        .flat_map(|d| {
            [
                ("DeploymentStarted".to_string(), d.to_string()),
                ("DeploymentCompleted".to_string(), d.to_string()),
            ]
        })
        .collect();
    if limit == 1 {
        assert_eq!(flow, expected);
    } else {
        assert_eq!(flow.len(), expected.len());
    }
}
