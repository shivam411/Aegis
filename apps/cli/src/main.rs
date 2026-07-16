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
        Commands::EmitEvent {
            event_type,
            payload_json,
        } => {
            // Verify payload is valid JSON
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
