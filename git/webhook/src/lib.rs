use aegis_event_bus::EventBus;
use aegis_types::{DeploymentId, Event, ProjectId, ReleaseId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct GitHubPushPayload {
    pub ref_branch: Option<String>,
    pub repository_url: String,
    pub commit_sha: String,
    pub commit_message: Option<String>,
    pub author: Option<String>,
}

pub struct WebhookHandler<'a> {
    pub event_bus: &'a EventBus,
}

impl<'a> WebhookHandler<'a> {
    pub fn new(event_bus: &'a EventBus) -> Self {
        Self { event_bus }
    }

    /// Processes an incoming Git push webhook payload and enqueues a deployment event.
    pub fn handle_push(&self, project_id: ProjectId, payload: GitHubPushPayload) -> Result<Event, anyhow::Error> {
        let deployment_id = DeploymentId::new();
        let release_id = ReleaseId::new();

        let payload_json = serde_json::json!({
            "deployment_id": deployment_id.to_string(),
            "project_id": project_id.to_string(),
            "release_id": release_id.to_string(),
            "commit_sha": payload.commit_sha,
            "repository_url": payload.repository_url,
            "strategy": "Immediate",
        })
        .to_string();

        let mut event = Event::new("DeploymentQueued", payload_json);
        event.aggregate_type = "Deployment".to_string();
        event.aggregate_id = deployment_id.to_string();

        self.event_bus.publish(event.clone());
        tracing::info!(deployment_id = %deployment_id, project_id = %project_id, "Webhook push processed and deployment queued");

        Ok(event)
    }
}
