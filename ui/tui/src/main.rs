use aegis_api::aegis::aegis_daemon_client::AegisDaemonClient;
use aegis_api::aegis::{StatusRequest, StreamEventsRequest};
use aegis_config::Config;
use std::path::Path;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    println!("┌────────────────────────────────────────────────────────┐");
    println!("│                AEGIS OPERATIONAL TUI                   │");
    println!("│             Terminal Deployment & Monitor              │");
    println!("└────────────────────────────────────────────────────────┘");

    let config = Config::load_or_default(Path::new("aegis.toml"));
    let addr = format!("http://{}:{}", config.daemon.host, config.daemon.port);

    println!("Connecting to Aegis Daemon at {}...", addr);

    match AegisDaemonClient::connect(addr.clone()).await {
        Ok(mut client) => {
            println!("[STATUS] Connected successfully to daemon.");
            if let Ok(status) = client.get_status(StatusRequest {}).await {
                let st = status.into_inner();
                println!("  Daemon Version: {}", st.version);
                println!("  Plugins Loaded: {:?}", st.loaded_plugins);
                println!("  Total Logged Events: {}", st.event_count);
            }

            println!("\n[LIVE EVENT MONITOR STREAMING]");
            if let Ok(stream) = client.stream_events(StreamEventsRequest {}).await {
                let mut rx = stream.into_inner();
                while let Ok(Some(event)) = rx.message().await {
                    println!(
                        "  ⚡ [{}] TYPE: {:<20} | PAYLOAD: {}",
                        event.created_at, event.event_type, event.payload_json
                    );
                }
            }
        }
        Err(e) => {
            println!(
                "[ERROR] Could not connect to Aegis daemon at {}: {}",
                addr, e
            );
            println!("Please ensure 'aegis-daemon' is running.");
        }
    }

    Ok(())
}
