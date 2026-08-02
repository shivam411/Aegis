use aegis_plugins::Plugin;
use aegis_types::Event;
use async_trait::async_trait;

pub struct SlackPlugin {
    pub webhook_url: String,
}

impl SlackPlugin {
    pub fn new(webhook_url: String) -> Self {
        Self { webhook_url }
    }
}

#[async_trait]
impl Plugin for SlackPlugin {
    fn name(&self) -> &str {
        "slack-notifications-plugin"
    }

    async fn on_init(&self) -> Result<(), anyhow::Error> {
        tracing::info!(webhook_url = %self.webhook_url, "Slack Plugin initialized");
        Ok(())
    }

    async fn on_event(&self, event: &Event) -> Result<(), anyhow::Error> {
        match event.event_type.as_str() {
            "DeploymentQueued" | "DeploymentStarted" | "DeploymentCompleted" | "DeploymentFailed" | "RollbackTriggered" => {
                tracing::info!(
                    event_type = %event.event_type,
                    event_id = %event.id,
                    "Posting Slack notification alert"
                );
            }
            _ => {}
        }
        Ok(())
    }
}
