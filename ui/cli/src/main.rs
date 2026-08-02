use aegis_api::aegis::aegis_daemon_client::AegisDaemonClient;
use aegis_api::aegis::{EmitEventRequest, StatusRequest, StreamEventsRequest};
use aegis_config::Config;
use clap::{Parser, Subcommand};
use std::path::Path;

#[derive(Parser)]
#[command(name = "aegis")]
#[command(about = "Aegis operational terminal platform CLI", long_about = None)]
struct Cli {
    #[arg(short, long, help = "Path to config file")]
    config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check daemon status and loaded plugins
    Status,
    /// Initialize a project and auto-detect runtime
    Init {
        /// Project name
        #[arg(short, long)]
        name: Option<String>,
        /// Repository URL
        #[arg(short, long)]
        repo: Option<String>,
    },
    /// Trigger build and deployment for a project
    Deploy {
        /// Project ID
        project_id: String,
        /// Target Git branch
        #[arg(short, long, default_value = "main")]
        branch: String,
    },
    /// Rollback a project to a previous release version
    Rollback {
        /// Project ID
        project_id: String,
        /// Release version to roll back to
        version: String,
    },
    /// List all active projects and running processes
    List,
    /// Stream logs for a process or system
    Logs {
        /// Target process ID
        process_id: Option<String>,
    },
    /// Stop a running process
    Stop {
        /// Process ID to stop
        process_id: String,
    },
    /// Configure a daily auto-deployment schedule at specific hours
    Schedule {
        /// Target project ID
        project_id: String,
        /// Target deployment hour (0-23)
        #[arg(short, long)]
        hour: u32,
        /// Target deployment minute (0-59)
        #[arg(short, long, default_value = "0")]
        minute: u32,
        /// Target Git branch
        #[arg(short, long, default_value = "main")]
        branch: String,
    },
    /// Restart a process
    Restart {
        /// Process ID to restart
        process_id: String,
    },
    /// Manually emit an operational event to the daemon
    EmitEvent {
        /// The event type (e.g. RepositoryAdded, DeploymentStarted)
        event_type: String,
        /// The event payload in JSON format
        payload_json: String,
    },
    /// Stream operational events in real-time from the daemon
    StreamEvents,
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    // Load configuration to find daemon port
    let config_path = cli.config.unwrap_or_else(|| "aegis.toml".to_string());
    let config = Config::load_or_default(Path::new(&config_path));

    let addr = format!("http://{}:{}", config.daemon.host, config.daemon.port);

    // Connect to gRPC client
    let mut client = AegisDaemonClient::connect(addr.clone())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to Aegis daemon at {}: {}", addr, e))?;

    match cli.command {
        Commands::Status => {
            let response = client.get_status(StatusRequest {}).await?.into_inner();
            println!("Aegis Daemon Status:");
            println!("  Initialized: {}", response.initialized);
            println!("  Version:     {}", response.version);
            println!("  Plugins ({}):", response.loaded_plugins.len());
            for plugin in response.loaded_plugins {
                println!("    - {}", plugin);
            }
            println!("  Total Events: {}", response.event_count);
        }
        Commands::Init { name, repo } => {
            let project_id = aegis_types::ProjectId::new();
            let proj_name = name.unwrap_or_else(|| "unnamed-project".to_string());
            let repo_url = repo.unwrap_or_else(|| "https://github.com/aegis/project".to_string());

            println!("Initializing Aegis Project '{}' ({})", proj_name, project_id);
            let response = client
                .emit_event(EmitEventRequest {
                    event_type: "ProjectCreated".to_string(),
                    payload_json: serde_json::json!({
                        "project_id": project_id.to_string(),
                        "name": proj_name,
                        "repository_url": repo_url,
                        "branch": "main",
                    })
                    .to_string(),
                })
                .await?
                .into_inner();

            if response.success {
                println!("Project created successfully! (Event ID: {})", response.event_id);
            }
        }
        Commands::Deploy { project_id, branch } => {
            let deployment_id = aegis_types::DeploymentId::new();
            let release_id = aegis_types::ReleaseId::new();

            println!("Deploying project {} (Branch: {})", project_id, branch);
            let response = client
                .emit_event(EmitEventRequest {
                    event_type: "DeploymentQueued".to_string(),
                    payload_json: serde_json::json!({
                        "deployment_id": deployment_id.to_string(),
                        "project_id": project_id,
                        "release_id": release_id.to_string(),
                        "branch": branch,
                        "strategy": "Immediate",
                    })
                    .to_string(),
                })
                .await?
                .into_inner();

            if response.success {
                println!("Deployment queued successfully! (Deployment ID: {})", deployment_id);
            }
        }
        Commands::Rollback { project_id, version } => {
            println!("Triggering rollback for project {} to version {}", project_id, version);
            let response = client
                .emit_event(EmitEventRequest {
                    event_type: "RollbackTriggered".to_string(),
                    payload_json: serde_json::json!({
                        "project_id": project_id,
                        "target_version": version,
                    })
                    .to_string(),
                })
                .await?
                .into_inner();

            if response.success {
                println!("Rollback triggered successfully!");
            }
        }
        Commands::List => {
            println!("Querying Aegis active projects and processes...");
            let response = client.get_status(StatusRequest {}).await?.into_inner();
            println!("Daemon Version: {}", response.version);
            println!("Total Events Logged: {}", response.event_count);
        }
        Commands::Logs { process_id } => {
            if let Some(id) = process_id {
                println!("Streaming logs for process {}...", id);
            } else {
                println!("Streaming all system operational logs...");
            }
            let mut stream = client.stream_events(StreamEventsRequest {}).await?.into_inner();
            while let Some(event) = stream.message().await? {
                println!("[{}] TYPE: {} | PAYLOAD: {}", event.created_at, event.event_type, event.payload_json);
            }
        }
        Commands::Stop { process_id } => {
            println!("Stopping process {}...", process_id);
            let response = client
                .emit_event(EmitEventRequest {
                    event_type: "ProcessStopped".to_string(),
                    payload_json: serde_json::json!({ "process_id": process_id }).to_string(),
                })
                .await?
                .into_inner();

            if response.success {
                println!("Stop signal sent to process {}", process_id);
            }
        }
        Commands::Schedule {
            project_id,
            hour,
            minute,
            branch,
        } => {
            println!(
                "Configuring daily auto-deployment schedule for project {} at {:02}:{:02} (Branch: {})",
                project_id, hour, minute, branch
            );
            let response = client
                .emit_event(EmitEventRequest {
                    event_type: "ScheduleConfigured".to_string(),
                    payload_json: serde_json::json!({
                        "project_id": project_id,
                        "target_hour": hour,
                        "target_minute": minute,
                        "branch": branch,
                    })
                    .to_string(),
                })
                .await?
                .into_inner();

            if response.success {
                println!("Daily auto-deployment schedule configured successfully!");
            }
        }
        Commands::Restart { process_id } => {
            println!("Restarting process {}...", process_id);
            let response = client
                .emit_event(EmitEventRequest {
                    event_type: "ProcessRestarted".to_string(),
                    payload_json: serde_json::json!({ "process_id": process_id }).to_string(),
                })
                .await?
                .into_inner();

            if response.success {
                println!("Restart signal sent to process {}", process_id);
            }
        }
        Commands::EmitEvent {
            event_type,
            payload_json,
        } => {
            let _: serde_json::Value = serde_json::from_str(&payload_json)
                .map_err(|e| anyhow::anyhow!("Payload is not valid JSON: {}", e))?;

            let response = client
                .emit_event(EmitEventRequest {
                    event_type: event_type.clone(),
                    payload_json,
                })
                .await?
                .into_inner();

            if response.success {
                println!("Successfully emitted event '{}' (ID: {})", event_type, response.event_id);
            } else {
                println!("Failed to emit event");
            }
        }
        Commands::StreamEvents => {
            println!("Listening for operational events from daemon at {}...", addr);
            let mut stream = client
                .stream_events(StreamEventsRequest {})
                .await?
                .into_inner();

            while let Some(event) = stream.message().await? {
                println!("[{}] TYPE: {} | PAYLOAD: {}", event.created_at, event.event_type, event.payload_json);
            }
        }
    }

    Ok(())
}
