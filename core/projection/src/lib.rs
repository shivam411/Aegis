use aegis_types::{ArtifactId, DeploymentId, Event, ProcessId, ProjectId, ReleaseId, ServerId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectState {
    pub id: ProjectId,
    pub name: String,
    pub repository_url: String,
    pub branch: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseState {
    pub id: ReleaseId,
    pub project_id: ProjectId,
    pub version: String,
    pub commit_sha: String,
    pub artifacts: Vec<ArtifactId>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeploymentState {
    pub id: DeploymentId,
    pub project_id: ProjectId,
    pub release_id: ReleaseId,
    pub strategy: String,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProcessState {
    pub id: ProcessId,
    pub project_id: ProjectId,
    pub pid: Option<u32>,
    pub status: String,
    pub restart_count: u32,
    pub last_start: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerState {
    pub id: ServerId,
    pub hostname: String,
    pub ip_address: String,
    pub status: String,
    pub last_heartbeat: String,
}

pub trait Projection: Send + Sync {
    fn apply_event(&mut self, event: &Event);
}

#[derive(Clone, Default)]
pub struct ProjectionEngine {
    projects: Arc<RwLock<HashMap<ProjectId, ProjectState>>>,
    releases: Arc<RwLock<HashMap<ReleaseId, ReleaseState>>>,
    deployments: Arc<RwLock<HashMap<DeploymentId, DeploymentState>>>,
    processes: Arc<RwLock<HashMap<ProcessId, ProcessState>>>,
    servers: Arc<RwLock<HashMap<ServerId, ServerState>>>,
}

impl ProjectionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replays a historic sequence of events to reconstruct current state caches.
    pub fn replay_from_store(&self, events: &[Event]) {
        for event in events {
            self.apply_event(event);
        }
    }

    /// Processes an individual domain event and updates the relevant in-memory projection models.
    pub fn apply_event(&self, event: &Event) {
        let payload: serde_json::Value = match serde_json::from_str(&event.payload_json) {
            Ok(val) => val,
            Err(e) => {
                tracing::warn!(event_id = %event.id, error = %e, "Failed to parse event JSON for projection");
                return;
            }
        };

        match event.event_type.as_str() {
            "ProjectCreated" => {
                if let (Some(id_str), Some(name), Some(repo_url)) = (
                    payload.get("project_id").and_then(|v| v.as_str()),
                    payload.get("name").and_then(|v| v.as_str()),
                    payload.get("repository_url").and_then(|v| v.as_str()),
                ) {
                    if let Ok(id) = id_str.parse::<ProjectId>() {
                        let branch = payload
                            .get("branch")
                            .and_then(|v| v.as_str())
                            .unwrap_or("main")
                            .to_string();
                        let state = ProjectState {
                            id,
                            name: name.to_string(),
                            repository_url: repo_url.to_string(),
                            branch,
                            status: "Active".to_string(),
                            created_at: event.created_at.clone(),
                        };
                        self.projects.write().unwrap().insert(id, state);
                    }
                }
            }
            "DeploymentQueued" | "DeploymentStarted" => {
                if let (Some(dep_id_str), Some(proj_id_str), Some(rel_id_str)) = (
                    payload.get("deployment_id").and_then(|v| v.as_str()),
                    payload.get("project_id").and_then(|v| v.as_str()),
                    payload.get("release_id").and_then(|v| v.as_str()),
                ) {
                    if let (Ok(dep_id), Ok(proj_id), Ok(rel_id)) = (
                        dep_id_str.parse::<DeploymentId>(),
                        proj_id_str.parse::<ProjectId>(),
                        rel_id_str.parse::<ReleaseId>(),
                    ) {
                        let strategy = payload
                            .get("strategy")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Immediate")
                            .to_string();
                        let status = if event.event_type == "DeploymentQueued" {
                            "Queued"
                        } else {
                            "InProgress"
                        };
                        let state = DeploymentState {
                            id: dep_id,
                            project_id: proj_id,
                            release_id: rel_id,
                            strategy,
                            status: status.to_string(),
                            created_at: event.created_at.clone(),
                            completed_at: None,
                        };
                        self.deployments.write().unwrap().insert(dep_id, state);
                    }
                }
            }
            "DeploymentCompleted" | "DeploymentFailed" => {
                if let Some(dep_id_str) = payload.get("deployment_id").and_then(|v| v.as_str()) {
                    if let Ok(dep_id) = dep_id_str.parse::<DeploymentId>() {
                        if let Some(dep) = self.deployments.write().unwrap().get_mut(&dep_id) {
                            dep.status = if event.event_type == "DeploymentCompleted" {
                                "Success".to_string()
                            } else {
                                "Failed".to_string()
                            };
                            dep.completed_at = Some(event.created_at.clone());
                        }
                    }
                }
            }
            "ProcessStarted" => {
                if let (Some(proc_id_str), Some(proj_id_str), Some(pid)) = (
                    payload.get("process_id").and_then(|v| v.as_str()),
                    payload.get("project_id").and_then(|v| v.as_str()),
                    payload.get("pid").and_then(|v| v.as_u64()),
                ) {
                    if let (Ok(proc_id), Ok(proj_id)) = (
                        proc_id_str.parse::<ProcessId>(),
                        proj_id_str.parse::<ProjectId>(),
                    ) {
                        let state = ProcessState {
                            id: proc_id,
                            project_id: proj_id,
                            pid: Some(pid as u32),
                            status: "Running".to_string(),
                            restart_count: 0,
                            last_start: Some(event.created_at.clone()),
                        };
                        self.processes.write().unwrap().insert(proc_id, state);
                    }
                }
            }
            "ProcessStopped" => {
                if let Some(proc_id_str) = payload.get("process_id").and_then(|v| v.as_str()) {
                    if let Ok(proc_id) = proc_id_str.parse::<ProcessId>() {
                        if let Some(proc) = self.processes.write().unwrap().get_mut(&proc_id) {
                            proc.status = "Stopped".to_string();
                            proc.pid = None;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Read Queries for CQRS Read Path
    pub fn get_projects(&self) -> Vec<ProjectState> {
        self.projects.read().unwrap().values().cloned().collect()
    }

    pub fn get_project(&self, id: &ProjectId) -> Option<ProjectState> {
        self.projects.read().unwrap().get(id).cloned()
    }

    pub fn get_deployments(&self) -> Vec<DeploymentState> {
        self.deployments.read().unwrap().values().cloned().collect()
    }

    pub fn get_processes(&self) -> Vec<ProcessState> {
        self.processes.read().unwrap().values().cloned().collect()
    }

    pub fn get_releases(&self) -> Vec<ReleaseState> {
        self.releases.read().unwrap().values().cloned().collect()
    }

    pub fn get_servers(&self) -> Vec<ServerState> {
        self.servers.read().unwrap().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_projection_engine_event_folding() {
        let engine = ProjectionEngine::new();
        let proj_id = ProjectId::new();

        let payload = serde_json::json!({
            "project_id": proj_id.to_string(),
            "name": "aegis-app",
            "repository_url": "https://github.com/aegis/app",
            "branch": "main"
        })
        .to_string();
        let mut event = Event::new("ProjectCreated", payload);
        event.created_at = "2026-08-02T12:00:00Z".to_string();

        engine.apply_event(&event);

        let projects = engine.get_projects();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "aegis-app");
        assert_eq!(projects[0].id, proj_id);

        let single_proj = engine.get_project(&proj_id);
        assert!(single_proj.is_some());
        assert_eq!(single_proj.unwrap().name, "aegis-app");
    }

    #[test]
    fn test_projection_engine_deployment_and_process_events() {
        let engine = ProjectionEngine::new();
        let proj_id = ProjectId::new();
        let rel_id = aegis_types::ReleaseId::new();
        let dep_id = aegis_types::DeploymentId::new();
        let proc_id = aegis_types::ProcessId::new();

        // 1. DeploymentQueued
        let payload1 = serde_json::json!({
            "deployment_id": dep_id.to_string(),
            "project_id": proj_id.to_string(),
            "release_id": rel_id.to_string(),
            "strategy": "GracefulSwitch"
        })
        .to_string();
        let event1 = Event::new("DeploymentQueued", payload1);
        engine.apply_event(&event1);

        let deps = engine.get_deployments();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].status, "Queued");

        // 2. DeploymentCompleted
        let payload2 = serde_json::json!({
            "deployment_id": dep_id.to_string(),
        })
        .to_string();
        let event2 = Event::new("DeploymentCompleted", payload2);
        engine.apply_event(&event2);

        let deps = engine.get_deployments();
        assert_eq!(deps[0].status, "Success");
        assert!(deps[0].completed_at.is_some());

        // 3. ProcessStarted
        let payload3 = serde_json::json!({
            "process_id": proc_id.to_string(),
            "project_id": proj_id.to_string(),
            "pid": 1234
        })
        .to_string();
        let event3 = Event::new("ProcessStarted", payload3);
        engine.apply_event(&event3);

        let procs = engine.get_processes();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].status, "Running");
        assert_eq!(procs[0].pid, Some(1234));

        // 4. ProcessStopped
        let payload4 = serde_json::json!({
            "process_id": proc_id.to_string(),
        })
        .to_string();
        let event4 = Event::new("ProcessStopped", payload4);
        engine.apply_event(&event4);

        let procs = engine.get_processes();
        assert_eq!(procs[0].status, "Stopped");
        assert_eq!(procs[0].pid, None);

        // Check empty collections getters
        assert!(engine.get_releases().is_empty());
        assert!(engine.get_servers().is_empty());
    }

    #[test]
    fn test_projection_engine_replay_and_invalid_json() {
        let engine = ProjectionEngine::new();
        let invalid_event = Event::new("ProjectCreated", "{ invalid_json }");
        engine.apply_event(&invalid_event);
        assert!(engine.get_projects().is_empty());

        let proj_id = ProjectId::new();
        let payload = serde_json::json!({
            "project_id": proj_id.to_string(),
            "name": "replayed-app",
            "repository_url": "https://github.com/aegis/replayed"
        })
        .to_string();
        let event = Event::new("ProjectCreated", payload);

        engine.replay_from_store(&[event]);
        assert_eq!(engine.get_projects().len(), 1);
        assert_eq!(engine.get_projects()[0].name, "replayed-app");
    }
}
