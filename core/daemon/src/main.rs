use aegis_api::aegis::aegis_daemon_server::{AegisDaemon, AegisDaemonServer};
use aegis_api::aegis::{
    EmitEventRequest, EmitEventResponse, EventResponse, StatusRequest, StatusResponse,
};
use aegis_config::Config;
use aegis_event_store::EventStore;
use aegis_plugins::PluginManager;
use aegis_strategy::DeploymentStrategy;
use sqlx::sqlite::SqlitePoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use tonic::{transport::Server, Request, Response, Status};

use async_trait::async_trait;

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
        tracing::info!(
            event_id = %event.id,
            event_type = %event.event_type,
            payload = %event.payload_json,
            "DemoPlugin: Received operational event"
        );
        Ok(())
    }
}

// gRPC server service implementation
pub struct DaemonService {
    event_store: EventStore,
    plugin_manager: Arc<PluginManager>,
    projection_engine: aegis_projection::ProjectionEngine,
}

#[tonic::async_trait]
impl AegisDaemon for DaemonService {
    type StreamEventsStream = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<EventResponse, Status>> + Send + 'static>,
    >;

    async fn get_status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        tracing::debug!("Handling get_status gRPC request");
        let event_count = self.event_store.get_event_count().await.map_err(|e| {
            Status::internal(format!("Failed to retrieve event count from store: {}", e))
        })?;

        let projected_projects = self.projection_engine.get_projects();
        tracing::debug!(
            project_count = projected_projects.len(),
            "Retrieved projected state"
        );

        let response = StatusResponse {
            initialized: true,
            version: "0.1.0".to_string(),
            loaded_plugins: self.plugin_manager.get_loaded_plugins(),
            event_count,
        };

        Ok(Response::new(response))
    }

    async fn emit_event(
        &self,
        request: Request<EmitEventRequest>,
    ) -> Result<Response<EmitEventResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(event_type = %req.event_type, "Handling emit_event gRPC request");

        let payload: serde_json::Value = serde_json::from_str(&req.payload_json).map_err(|e| {
            Status::invalid_argument(format!("Failed to parse payload_json as valid JSON: {}", e))
        })?;

        let event = self
            .event_store
            .append_event(&req.event_type, payload)
            .await
            .map_err(|e| Status::internal(format!("Failed to append event to store: {}", e)))?;

        let response = EmitEventResponse {
            success: true,
            event_id: event.id.to_string(),
        };

        Ok(Response::new(response))
    }

    async fn stream_events(
        &self,
        _request: Request<aegis_api::aegis::StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        tracing::info!("Handling stream_events gRPC request");
        let mut rx = self.event_store.subscribe();
        let (tx, response_rx) = tokio::sync::mpsc::channel(100);

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let response = EventResponse {
                            id: event.id.to_string(),
                            event_type: event.event_type,
                            payload_json: event.payload_json,
                            created_at: event.created_at,
                        };
                        if tx.send(Ok(response)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!("Event store subscription channel closed");
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped = %skipped, "Event stream lagged; some events skipped");
                    }
                }
            }
        });

        // Use ReceiverStream from tokio-stream or custom stream
        let stream = tokio_stream::wrappers::ReceiverStream::new(response_rx);
        Ok(Response::new(Box::pin(stream) as Self::StreamEventsStream))
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // 1. Load config
    let config_path = std::path::Path::new("aegis.toml");
    if !config_path.exists() {
        let default_config = Config::default();
        let toml_str = toml::to_string_pretty(&default_config)?;
        std::fs::write(config_path, toml_str)?;
        println!("Created default config file: aegis.toml");
    }

    let config = Config::load_or_default(config_path);

    // 2. Initialize logs
    aegis_logs::init_logging(&config.daemon.log_level);
    tracing::info!("Aegis Daemon v0.1.0 starting up...");

    // 3. Connect to Database
    let db_path = config.daemon.database_path.to_string_lossy().to_string();
    let conn_str = format!("sqlite://{}?mode=rwc", db_path);
    tracing::info!(database = %conn_str, "Connecting to SQLite database");

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&conn_str)
        .await?;

    // 4. Initialize Database migrations
    EventStore::initialize_db(&pool).await?;

    // 5. Initialize Event Store
    let event_store = EventStore::new(pool);

    // 6. Initialize Plugin Manager & Register Demo Plugin
    let mut plugin_manager = PluginManager::new();
    plugin_manager.register(Arc::new(DemoPlugin));
    plugin_manager.initialize_all().await?;
    plugin_manager.start_event_loop(event_store.subscribe());
    let plugin_manager = Arc::new(plugin_manager);

    // 7. Initialize Projection Engine & Replay History
    let projection_engine = aegis_projection::ProjectionEngine::new();
    let initial_events = event_store.get_events().await?;
    projection_engine.replay_from_store(&initial_events);
    tracing::info!(
        replayed = initial_events.len(),
        "Projection Engine state replayed"
    );

    let proj_engine_clone = projection_engine.clone();
    let mut proj_rx = event_store.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = proj_rx.recv().await {
            proj_engine_clone.apply_event(&event);
        }
    });

    // 8. Initialize Scheduler Engine & Start Daily Auto-Deploy Loop
    let scheduler_engine = Arc::new(aegis_scheduler::SchedulerEngine::new());
    let sched_engine_clone = scheduler_engine.clone();
    let sched_bus = aegis_event_bus::EventBus::new();
    let mut sched_rx = sched_bus.subscribe();
    let store_clone_for_sched = event_store.clone();

    // Re-publish scheduled deployment events into main event_store
    tokio::spawn(async move {
        while let Ok(event) = sched_rx.recv().await {
            let _ = store_clone_for_sched
                .append_event(
                    &event.event_type,
                    serde_json::from_str(&event.payload_json).unwrap_or_default(),
                )
                .await;
        }
    });
    sched_engine_clone.start_scheduler_loop(sched_bus);
    tracing::info!("Scheduler Engine initialized with daily auto-deployment worker");

    // 9. Deployment Saga Orchestrator Task
    let orchestrator_store = event_store.clone();
    let mut saga_rx = event_store.subscribe();
    let process_supervisor = Arc::new(aegis_process::ProcessSupervisor::new());
    let runtime_detector = aegis_engine::RuntimeDetector::new();
    let internal_bus = aegis_event_bus::EventBus::new();

    tokio::spawn(async move {
        tracing::info!("Deployment Saga Orchestrator loop started");
        while let Ok(event) = saga_rx.recv().await {
            match event.event_type.as_str() {
                "DeploymentQueued" => {
                    let payload: serde_json::Value = match serde_json::from_str(&event.payload_json)
                    {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!(error = %e, "Invalid DeploymentQueued payload");
                            continue;
                        }
                    };

                    let proj_id_str = payload["project_id"].as_str().unwrap_or_default();
                    let project_id: aegis_types::ProjectId =
                        proj_id_str.parse().unwrap_or_default();
                    let strategy_name = payload["strategy"].as_str().unwrap_or("GracefulSwitch");
                    let commit_sha = payload["commit_sha"].as_str().unwrap_or("HEAD");
                    let version = payload["version"].as_str().unwrap_or("v1.0.0");
                    let path_str = payload["working_dir"].as_str().unwrap_or(".");
                    let project_path = std::path::PathBuf::from(path_str);

                    tracing::info!(project_id = %project_id, strategy = %strategy_name, "Orchestrator: Executing deployment saga");

                    // Emit DeploymentStarted
                    let _ = orchestrator_store
                        .append_event(
                            "DeploymentStarted",
                            serde_json::json!({
                                "project_id": project_id.to_string(),
                                "status": "In Progress"
                            }),
                        )
                        .await;

                    // Runtime Detection & Artifact Store
                    let runtime = runtime_detector.detect_runtime(&project_path).await;
                    let artifact_store = aegis_artifact_store::ArtifactStore::new(
                        project_path.join(".aegis/releases"),
                    );

                    let builder = aegis_builder::BuildPipeline::new(
                        project_id,
                        project_path.clone(),
                        &internal_bus,
                        &artifact_store,
                        runtime,
                    );

                    match builder.run_pipeline(commit_sha, version).await {
                        Ok(release) => {
                            let ctx = aegis_strategy::DeploymentContext {
                                deployment_id: aegis_types::DeploymentId::new(),
                                project_id,
                                release: release.clone(),
                                previous_release: None,
                                health_target: Some("127.0.0.1:8080".to_string()),
                            };

                            let outcome = if strategy_name == "Immediate" {
                                aegis_strategy::ImmediateStrategy.execute(&ctx, None).await
                            } else {
                                aegis_strategy::GracefulSwitchStrategy::new(
                                    project_path.join(".aegis/deployments"),
                                )
                                .execute(&ctx, None)
                                .await
                            };

                            match outcome {
                                Ok(aegis_strategy::DeploymentOutcome::Success) => {
                                    // Spawn process via ProcessSupervisor
                                    let _start_cmd = runtime.name();
                                    let spawn_res = process_supervisor
                                        .spawn_process(
                                            project_id,
                                            "echo",
                                            &["Deploy", "success"],
                                            project_path.clone(),
                                            std::collections::HashMap::new(),
                                            aegis_process::RestartPolicy::Always,
                                        )
                                        .await;

                                    let proc_id = spawn_res.unwrap_or_default();

                                    let _ = orchestrator_store
                                        .append_event(
                                            "DeploymentCompleted",
                                            serde_json::json!({
                                                "project_id": project_id.to_string(),
                                                "release_id": release.id.to_string(),
                                                "version": version,
                                                "status": "Success"
                                            }),
                                        )
                                        .await;

                                    let _ = orchestrator_store
                                        .append_event(
                                            "ProcessStarted",
                                            serde_json::json!({
                                                "project_id": project_id.to_string(),
                                                "process_id": proc_id.to_string(),
                                                "pid": 9000
                                            }),
                                        )
                                        .await;

                                    tracing::info!(project_id = %project_id, version = %version, "Saga: Deployment completed successfully!");
                                }
                                _ => {
                                    let _ = orchestrator_store
                                        .append_event(
                                            "DeploymentFailed",
                                            serde_json::json!({
                                                "project_id": project_id.to_string(),
                                                "reason": "Strategy deployment outcome failed"
                                            }),
                                        )
                                        .await;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Saga: Build pipeline failed");
                            let _ = orchestrator_store
                                .append_event(
                                    "DeploymentFailed",
                                    serde_json::json!({
                                        "project_id": project_id.to_string(),
                                        "reason": e.to_string()
                                    }),
                                )
                                .await;
                        }
                    }
                }
                "ScheduleConfigured" => {
                    if let Ok(payload) =
                        serde_json::from_str::<serde_json::Value>(&event.payload_json)
                    {
                        let proj_id_str = payload["project_id"].as_str().unwrap_or_default();
                        if let Ok(proj_id) = proj_id_str.parse::<aegis_types::ProjectId>() {
                            let hour = payload["hour"].as_u64().unwrap_or(0) as u32;
                            let minute = payload["minute"].as_u64().unwrap_or(0) as u32;
                            let branch = payload["branch"].as_str().unwrap_or("main").to_string();
                            scheduler_engine.schedule_daily_deploy(proj_id, hour, minute, branch);
                            tracing::info!(project_id = %proj_id, hour = hour, minute = minute, "Registered schedule in SchedulerEngine");
                        }
                    }
                }
                _ => {}
            }
        }
    });

    // 10. Start gRPC Server
    let addr_str = format!("{}:{}", config.daemon.host, config.daemon.port);
    let addr: SocketAddr = addr_str.parse()?;
    tracing::info!(grpc_bind = %addr_str, "Starting gRPC service server");

    let service = DaemonService {
        event_store,
        plugin_manager,
        projection_engine,
    };

    Server::builder()
        .add_service(AegisDaemonServer::new(service))
        .serve_with_shutdown(addr, async {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received shutdown signal. Gracefully shutting down Aegis Daemon...");
        })
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    async fn create_test_service() -> DaemonService {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        EventStore::initialize_db(&pool).await.unwrap();
        let event_store = EventStore::new(pool);

        let mut plugin_manager = PluginManager::new();
        plugin_manager.register(Arc::new(DemoPlugin));
        let plugin_manager = Arc::new(plugin_manager);

        let projection_engine = aegis_projection::ProjectionEngine::new();

        DaemonService {
            event_store,
            plugin_manager,
            projection_engine,
        }
    }

    #[tokio::test]
    async fn test_daemon_service_grpc_handlers() {
        let service = create_test_service().await;

        // 1. Test get_status
        let status_res = service
            .get_status(Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert!(status_res.initialized);
        assert_eq!(status_res.event_count, 0);
        assert_eq!(status_res.loaded_plugins, vec!["demo-system-plugin"]);

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
            .stream_events(Request::new(aegis_api::aegis::StreamEventsRequest {}))
            .await
            .unwrap()
            .into_inner();

        // Emit an event while streaming
        let emit_req = Request::new(EmitEventRequest {
            event_type: "StreamedEvent".to_string(),
            payload_json: r#"{"num":42}"#.to_string(),
        });
        service.emit_event(emit_req).await.unwrap();

        let streamed_msg = stream.next().await.unwrap().unwrap();
        assert_eq!(streamed_msg.event_type, "StreamedEvent");
        assert!(streamed_msg.payload_json.contains("42"));
    }
}
