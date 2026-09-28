mod service;

use aegis_api::aegis::aegis_daemon_server::AegisDaemonServer;
use aegis_config::Config;
use aegis_control::{ControlPlane, ControlSettings};
use aegis_event_store::EventStore;
use aegis_plugins::PluginManager;
use aegis_process::ProcessSupervisor;
use async_trait::async_trait;
use service::{DaemonService, DAEMON_VERSION};
use sqlx::sqlite::SqlitePoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use tonic::transport::Server;

// A Demo System Plugin that implements the dynamic Plugin trait
struct DemoPlugin;

#[async_trait]
impl aegis_plugins::Plugin for DemoPlugin {
    fn name(&self) -> &str {
        "demo-system-plugin"
    }

    async fn on_init(&self) -> Result<(), anyhow::Error> {
        tracing::info!("DemoPlugin: System plugin initialized!");
        Ok(())
    }

    async fn on_event(&self, event: &aegis_types::Event) -> Result<(), anyhow::Error> {
        tracing::debug!(
            event_id = %event.id,
            event_type = %event.event_type,
            "DemoPlugin: Received operational event"
        );
        Ok(())
    }
}

/// Registers a `ScheduleConfigured` event with the scheduler.
fn apply_schedule(scheduler: &aegis_scheduler::SchedulerEngine, event: &aegis_types::Event) {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.payload_json) else {
        return;
    };
    let Some(project_id) = payload["project_id"]
        .as_str()
        .and_then(|s| s.parse::<aegis_types::ProjectId>().ok())
    else {
        return;
    };
    let hour = payload["hour"].as_u64().unwrap_or(0) as u32;
    let minute = payload["minute"].as_u64().unwrap_or(0) as u32;
    let branch = payload["branch"].as_str().unwrap_or("main").to_string();
    scheduler.schedule_daily_deploy(project_id, hour, minute, branch);
}

/// Resolves on Ctrl-C or SIGTERM (what systemd and `kill` send).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // 1. Load config. The daemon must not create aegis.toml itself: `aegis init`
    // owns that file, and a pre-existing daemon-only file made init skip writing
    // the [project] section, so every deploy got a fresh random project ID.
    let config_path = std::env::var("AEGIS_CONFIG").unwrap_or_else(|_| "aegis.toml".to_string());
    let config = Config::load_or_default(&config_path);

    // 2. Initialize logs
    aegis_logs::init_logging(&config.daemon.log_level);
    tracing::info!("Aegis Daemon v{} starting up...", DAEMON_VERSION);

    // 3. Connect to Database
    let db_path = config.daemon.database_path.clone();
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let conn_str = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
    tracing::info!(database = %conn_str, "Connecting to SQLite database");
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&conn_str)
        .await?;
    EventStore::initialize_db(&pool).await?;
    let event_store = EventStore::new(pool);

    // 4. Plugins
    let mut plugin_manager = PluginManager::new();
    plugin_manager.register(Arc::new(DemoPlugin));
    plugin_manager.initialize_all().await?;
    plugin_manager.start_event_loop(event_store.subscribe());
    let plugin_manager = Arc::new(plugin_manager);

    // 5. Control plane: rebuilds state from history and runs deployments.
    let data_dir = config.daemon.resolved_data_dir();
    let mut settings = ControlSettings::new(data_dir.clone());
    settings.max_retained_versions = config.daemon.max_retained_versions;
    let control = ControlPlane::new(event_store.clone(), ProcessSupervisor::new(), settings)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    control.start();
    tracing::info!(data_dir = %data_dir.display(), "Control plane ready");

    // 6. Scheduler: restore persisted schedules, then follow new ones.
    let scheduler = Arc::new(aegis_scheduler::SchedulerEngine::new());
    for event in event_store.get_events().await? {
        if event.event_type == "ScheduleConfigured" {
            apply_schedule(&scheduler, &event);
        }
    }
    let mut schedule_rx = event_store.subscribe();
    let scheduler_for_events = scheduler.clone();
    tokio::spawn(async move {
        loop {
            match schedule_rx.recv().await {
                Ok(event) if event.event_type == "ScheduleConfigured" => {
                    apply_schedule(&scheduler_for_events, &event)
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    // Scheduled deployments are published on their own bus; persist them so
    // the control plane picks them up like any other queued deployment.
    let sched_bus = aegis_event_bus::EventBus::new();
    let mut sched_rx = sched_bus.subscribe();
    let control_for_sched = control.clone();
    tokio::spawn(async move {
        while let Ok(event) = sched_rx.recv().await {
            let payload = serde_json::from_str(&event.payload_json).unwrap_or_default();
            if let Err(e) = control_for_sched.record(&event.event_type, payload).await {
                tracing::error!(error = %e, "Failed to record scheduled deployment");
            }
        }
    });
    scheduler.start_scheduler_loop(sched_bus);

    // 7. Bring back the apps that were running before the daemon stopped.
    let restored = control.recover().await;
    tracing::info!(restored, "Recovered app processes");

    // 8. gRPC server
    let addr: SocketAddr = format!("{}:{}", config.daemon.host, config.daemon.port).parse()?;
    tracing::info!(grpc_bind = %addr, "Starting gRPC service server");
    let service = DaemonService {
        control: control.clone(),
        plugin_manager,
    };
    Server::builder()
        .add_service(AegisDaemonServer::new(service))
        .serve_with_shutdown(addr, async {
            shutdown_signal().await;
            tracing::info!("Received shutdown signal. Gracefully shutting down Aegis Daemon...");
        })
        .await?;

    // 9. Stop apps cleanly; they are restarted on the next boot.
    control.shutdown().await;
    tracing::info!("Aegis Daemon stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_api::aegis::aegis_daemon_server::AegisDaemon;
    use aegis_api::aegis::{
        DeployRequest, EmitEventRequest, ListProjectsRequest, RegisterProjectRequest,
        StatusRequest, StreamEventsRequest,
    };
    use tokio_stream::StreamExt;
    use tonic::Request;

    async fn create_test_service(data_dir: &std::path::Path) -> DaemonService {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        EventStore::initialize_db(&pool).await.unwrap();
        let event_store = EventStore::new(pool);

        let mut plugin_manager = PluginManager::new();
        plugin_manager.register(Arc::new(DemoPlugin));

        let control = ControlPlane::new(
            event_store,
            ProcessSupervisor::new(),
            ControlSettings::new(data_dir.to_path_buf()),
        )
        .await
        .unwrap();
        control.start();
        DaemonService {
            control,
            plugin_manager: Arc::new(plugin_manager),
        }
    }

    #[tokio::test]
    async fn test_daemon_service_grpc_handlers() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = create_test_service(dir.path()).await;

        // 1. Test get_status
        let status_res = service
            .get_status(Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert!(status_res.initialized);
        assert_eq!(status_res.event_count, 0);
        assert_eq!(status_res.loaded_plugins, vec!["demo-system-plugin"]);
        assert_eq!(status_res.version, DAEMON_VERSION);

        // 2. Test emit_event invalid json
        let invalid_req = Request::new(EmitEventRequest {
            event_type: "TestEvent".to_string(),
            payload_json: "{ bad }".to_string(),
        });
        assert!(service.emit_event(invalid_req).await.is_err());

        // 3. Test emit_event valid
        let valid_req = Request::new(EmitEventRequest {
            event_type: "TestEvent".to_string(),
            payload_json: r#"{"foo":"bar"}"#.to_string(),
        });
        let emit_res = service.emit_event(valid_req).await.unwrap().into_inner();
        assert!(emit_res.success);
        assert!(!emit_res.event_id.is_empty());

        // 4. Verify event count updated
        let status_res2 = service
            .get_status(Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(status_res2.event_count, 1);

        // 5. Test stream_events
        let mut stream = service
            .stream_events(Request::new(StreamEventsRequest {}))
            .await
            .unwrap()
            .into_inner();
        let emit_req = Request::new(EmitEventRequest {
            event_type: "StreamedEvent".to_string(),
            payload_json: r#"{"num":42}"#.to_string(),
        });
        service.emit_event(emit_req).await.unwrap();
        let streamed_msg = stream.next().await.unwrap().unwrap();
        assert_eq!(streamed_msg.event_type, "StreamedEvent");
        assert!(streamed_msg.payload_json.contains("42"));
    }

    #[tokio::test]
    async fn test_register_and_deploy_validation() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = create_test_service(dir.path()).await;

        let project = service
            .register_project(Request::new(RegisterProjectRequest {
                name: "api".into(),
                source_dir: dir.path().to_string_lossy().to_string(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(project.name, "api");

        let listed = service
            .list_projects(Request::new(ListProjectsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.projects.len(), 1);

        // Unknown projects and unsupported strategies map to proper status codes.
        let err = service
            .deploy(Request::new(DeployRequest {
                project: "nope".into(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
        let err = service
            .deploy(Request::new(DeployRequest {
                project: "api".into(),
                strategy: "BlueGreen".into(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }
}
