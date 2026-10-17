//! Shared helpers for the Aegis benchmarks (see `docs/benchmarks.md`).

use aegis_event_store::EventStore;
use aegis_types::{DeploymentId, Event, ProcessId, ProjectId, ReleaseId};
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;

/// Opens (creating it if needed) a file-backed event store the way the
/// daemon does: `sqlite://<path>?mode=rwc`, five connections, migrations run.
pub async fn open_store(db_path: &Path) -> EventStore {
    let url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("open sqlite");
    EventStore::initialize_db(&pool).await.expect("migrations");
    EventStore::new(pool)
}

/// Writes `history` straight into the events table in one transaction.
/// Much faster than `append_event` per event; used to prepare databases for
/// the replay benchmarks (the rows are the same).
pub async fn seed_events(store: &EventStore, history: &[(String, Value)]) {
    let mut tx = store.pool().begin().await.expect("begin");
    for (kind, payload) in history {
        let event = Event::new(kind.clone(), payload.to_string());
        sqlx::query(
            "INSERT INTO events (id, event_type, payload_json, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(event.id.to_string())
        .bind(&event.event_type)
        .bind(&event.payload_json)
        .bind(&event.created_at)
        .execute(&mut *tx)
        .await
        .expect("insert");
    }
    tx.commit().await.expect("commit");
}

/// A realistic event history: `projects` apps, each deployed `deploys`
/// times. Every deployment records the events the deploy saga writes
/// (queued, started, the five build stages, release created, process
/// started, release promoted, previous process stopped, completed), so one
/// deployment is 12 events.
pub fn deploy_history(projects: usize, deploys: usize) -> Vec<(String, Value)> {
    let mut out = Vec::with_capacity(projects * (1 + deploys * 12));
    for p in 0..projects {
        let project_id = ProjectId::new().to_string();
        out.push((
            "ProjectCreated".into(),
            json!({
                "project_id": project_id,
                "name": format!("app-{p}"),
                "repository_url": "https://github.com/example/app",
                "branch": "main",
                "runtime": "node",
                "source_dir": format!("/srv/app-{p}"),
            }),
        ));
        let mut previous_process: Option<String> = None;
        for d in 0..deploys {
            let deployment_id = DeploymentId::new().to_string();
            let release_id = ReleaseId::new().to_string();
            let process_id = ProcessId::new().to_string();
            let version = format!("20261016-{d:06}");
            let base = json!({"deployment_id": deployment_id, "project_id": project_id});
            out.push((
                "DeploymentQueued".into(),
                json!({
                    "deployment_id": deployment_id,
                    "project_id": project_id,
                    "version": version,
                    "strategy": "Restart",
                    "trigger_source": "cli",
                    "actor": "user:admin",
                }),
            ));
            out.push(("DeploymentStarted".into(), base.clone()));
            for stage in ["Clone", "Install", "Build", "Test", "Package"] {
                let mut payload = base.clone();
                payload["stage"] = json!(stage);
                payload["duration_ms"] = json!(120);
                out.push((format!("BuildStage{stage}"), payload));
            }
            out.push((
                "ReleaseCreated".into(),
                json!({
                    "release_id": release_id,
                    "project_id": project_id,
                    "deployment_id": deployment_id,
                    "version": version,
                    "commit_sha": "0123456789abcdef0123456789abcdef01234567",
                    "commit_message": "Bump dependencies",
                    "checksum": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
                    "path": format!("/var/lib/aegis/projects/{project_id}/releases/{version}"),
                }),
            ));
            out.push((
                "ProcessStarted".into(),
                json!({
                    "process_id": process_id,
                    "project_id": project_id,
                    "pid": 10_000 + d,
                    "release_version": version,
                    "command": "node index.js",
                    "cwd": format!("/var/lib/aegis/projects/{project_id}/releases/{version}"),
                    "port": 3000,
                }),
            ));
            out.push((
                "ReleasePromoted".into(),
                json!({
                    "release_id": release_id,
                    "project_id": project_id,
                    "version": version,
                    "process_id": process_id,
                    "port": 3000,
                }),
            ));
            if let Some(old) = previous_process.replace(process_id.clone()) {
                out.push((
                    "ProcessStopped".into(),
                    json!({"process_id": old, "project_id": project_id, "reason": "replaced"}),
                ));
            } else {
                out.push((
                    "HealthCheckPassed".into(),
                    json!({"process_id": process_id, "project_id": project_id}),
                ));
            }
            out.push((
                "DeploymentCompleted".into(),
                json!({
                    "deployment_id": deployment_id,
                    "project_id": project_id,
                    "release_id": release_id,
                    "version": version,
                    "duration_ms": 4200,
                }),
            ));
        }
    }
    out
}

/// The same history as in-memory events (for projection benchmarks that
/// don't touch SQLite).
pub fn history_events(history: &[(String, Value)]) -> Vec<Event> {
    history
        .iter()
        .map(|(kind, payload)| Event::new(kind.clone(), payload.to_string()))
        .collect()
}
