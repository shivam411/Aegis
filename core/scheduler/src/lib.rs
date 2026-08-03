use aegis_event_bus::EventBus;
use aegis_types::{DeploymentId, Event, ProjectId, ReleaseId};
use chrono::{Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduledTask {
    pub project_id: ProjectId,
    pub target_hour: u32,   // 0 - 23
    pub target_minute: u32, // 0 - 59
    pub branch: String,
    pub enabled: bool,
    pub last_run_date: Option<String>,
}

#[derive(Clone, Default)]
pub struct SchedulerEngine {
    tasks: Arc<RwLock<HashMap<ProjectId, ScheduledTask>>>,
}

impl SchedulerEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or updates a daily auto-deployment schedule for a project.
    pub fn schedule_daily_deploy(
        &self,
        project_id: ProjectId,
        target_hour: u32,
        target_minute: u32,
        branch: String,
    ) -> ScheduledTask {
        let task = ScheduledTask {
            project_id,
            target_hour: target_hour % 24,
            target_minute: target_minute % 60,
            branch,
            enabled: true,
            last_run_date: None,
        };

        self.tasks.write().unwrap().insert(project_id, task.clone());
        tracing::info!(
            project_id = %project_id,
            hour = task.target_hour,
            minute = task.target_minute,
            "Scheduled daily auto-deployment"
        );
        task
    }

    /// Cancels schedule for a project.
    pub fn remove_schedule(&self, project_id: &ProjectId) -> bool {
        self.tasks.write().unwrap().remove(project_id).is_some()
    }

    /// Returns list of all active schedules.
    pub fn get_schedules(&self) -> Vec<ScheduledTask> {
        self.tasks.read().unwrap().values().cloned().collect()
    }

    /// Evaluates scheduled tasks against the current hour and minute.
    pub fn evaluate_and_trigger(
        &self,
        event_bus: &EventBus,
        current_hour: u32,
        current_minute: u32,
        today_date_str: &str,
    ) -> usize {
        let mut tasks = self.tasks.write().unwrap();
        let mut triggered_count = 0;

        for task in tasks.values_mut() {
            if !task.enabled {
                continue;
            }

            if task.target_hour == current_hour && task.target_minute == current_minute {
                let already_ran = task
                    .last_run_date
                    .as_deref()
                    .map(|d| d == today_date_str)
                    .unwrap_or(false);

                if !already_ran {
                    task.last_run_date = Some(today_date_str.to_string());
                    triggered_count += 1;

                    let deployment_id = DeploymentId::new();
                    let release_id = ReleaseId::new();
                    let payload = serde_json::json!({
                        "deployment_id": deployment_id.to_string(),
                        "project_id": task.project_id.to_string(),
                        "release_id": release_id.to_string(),
                        "branch": task.branch,
                        "strategy": "GracefulSwitch",
                        "trigger_source": "ScheduledAutoDeploy",
                    })
                    .to_string();

                    let mut event = Event::new("DeploymentQueued", payload);
                    event.aggregate_type = "Deployment".to_string();
                    event.aggregate_id = deployment_id.to_string();

                    event_bus.publish(event);
                    tracing::info!(
                        project_id = %task.project_id,
                        hour = current_hour,
                        minute = current_minute,
                        "Triggered scheduled daily auto-deployment"
                    );
                }
            }
        }

        triggered_count
    }

    /// Spawns a background Tokio loop evaluating scheduled tasks every 30 seconds.
    pub fn start_scheduler_loop(&self, event_bus: EventBus) {
        let engine = self.clone();
        tokio::spawn(async move {
            tracing::info!("Daily Auto-Deployment Scheduler loop started");
            loop {
                let now = Utc::now();
                let hour = now.hour();
                let minute = now.minute();
                let today_date_str = now.format("%Y-%m-%d").to_string();

                engine.evaluate_and_trigger(&event_bus, hour, minute, &today_date_str);

                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scheduled_auto_deploy() {
        let engine = SchedulerEngine::new();
        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let proj_id = ProjectId::new();
        engine.schedule_daily_deploy(proj_id, 2, 0, "main".to_string());

        let triggered = engine.evaluate_and_trigger(&event_bus, 2, 0, "2026-08-02");
        assert_eq!(triggered, 1);

        let event = rx.recv().await.unwrap();
        assert_eq!(event.event_type, "DeploymentQueued");
        assert!(event.payload_json.contains("ScheduledAutoDeploy"));

        // Second evaluation on same day should not re-trigger
        let re_triggered = engine.evaluate_and_trigger(&event_bus, 2, 0, "2026-08-02");
        assert_eq!(re_triggered, 0);
    }
}
