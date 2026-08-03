use thiserror::Error;

#[derive(Debug, Error)]
pub enum AegisError {
    #[error("Configuration Error: {reason}")]
    Config { reason: String },

    #[error("Project Error [{id}]: {message}")]
    Project { id: String, message: String },

    #[error("Deployment Error [{id}]: {stage} failed - {reason}")]
    Deployment { id: String, stage: String, reason: String },

    #[error("Runtime Error [{runtime}]: {message}")]
    Runtime { runtime: String, message: String },

    #[error("Build Pipeline Error: stage {stage} failed with code {exit_code:?}")]
    Build { stage: String, exit_code: Option<i32> },

    #[error("EventStore Persistence Error: {0}")]
    EventStore(String),

    #[error("Artifact Store Error: {0}")]
    ArtifactStore(String),

    #[error("Health Check Failed for target '{target}': {reason}")]
    HealthCheck { target: String, reason: String },

    #[error("Plugin Error [{plugin_name}]: {message}")]
    Plugin { plugin_name: String, message: String },

    #[error("gRPC Network Error: {0}")]
    Network(String),

    #[error("Validation Error: {0}")]
    Validation(String),

    #[error("Internal System Error: {0}")]
    Internal(String),
}
