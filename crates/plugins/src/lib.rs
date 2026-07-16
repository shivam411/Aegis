use async_trait::async_trait;
use std::sync::Arc;
use aegis_event_store::Event;

#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    async fn on_init(&self) -> Result<(), anyhow::Error>;
    async fn on_event(&self, event: &Event) -> Result<(), anyhow::Error>;
}

pub struct PluginManager {
    plugins: Vec<Arc<dyn Plugin>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self { plugins: Vec::new() }
    }

    pub fn register(&mut self, plugin: Arc<dyn Plugin>) {
        tracing::info!(plugin = %plugin.name(), "Registering plugin");
        self.plugins.push(plugin);
    }

    pub async fn initialize_all(&self) -> Result<(), anyhow::Error> {
        for plugin in &self.plugins {
            tracing::info!(plugin = %plugin.name(), "Initializing plugin");
            plugin.on_init().await?;
        }
        Ok(())
    }

    pub fn get_loaded_plugins(&self) -> Vec<String> {
        self.plugins.iter().map(|p| p.name().to_string()).collect()
    }

    /// Spawns a background task that listens to the event store receiver and dispatches events to registered plugins.
    pub fn start_event_loop(&self, mut rx: tokio::sync::broadcast::Receiver<Event>) {
        let plugins = self.plugins.clone();
        tokio::spawn(async move {
            tracing::info!("Plugin manager event loop started");
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        for plugin in &plugins {
                            let plugin = plugin.clone();
                            let event = event.clone();
                            tokio::spawn(async move {
                                if let Err(e) = plugin.on_event(&event).await {
                                    tracing::error!(
                                        plugin = %plugin.name(),
                                        event_id = %event.id,
                                        error = %e,
                                        "Error dispatching event to plugin"
                                    );
                                }
                            });
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("Plugin manager event stream closed, stopping event loop");
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped = %skipped, "Plugin manager event stream lagged");
                    }
                }
            }
        });
    }
}
