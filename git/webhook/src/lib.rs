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
    pub fn handle_push(
        &self,
        project_id: ProjectId,
        payload: GitHubPushPayload,
    ) -> Result<Event, anyhow::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_webhook_handler_push() {
        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();
        let handler = WebhookHandler::new(&event_bus);

        let proj_id = ProjectId::new();
        let payload = GitHubPushPayload {
            ref_branch: Some("refs/heads/main".to_string()),
            repository_url: "https://github.com/shivam411/Aegis".to_string(),
            commit_sha: "abc1234def5678".to_string(),
            commit_message: Some("Test commit".to_string()),
            author: Some("Dev".to_string()),
        };

        let event = handler.handle_push(proj_id, payload).unwrap();
        assert_eq!(event.event_type, "DeploymentQueued");
        assert_eq!(event.aggregate_type, "Deployment");

        let recv_event = rx.recv().await.unwrap();
        assert_eq!(recv_event.id, event.id);
        assert!(recv_event.payload_json.contains("abc1234def5678"));
    }
}
