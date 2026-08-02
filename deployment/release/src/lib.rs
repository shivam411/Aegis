use aegis_types::{ArtifactId, ProjectId, ReleaseId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod switcher;
pub use switcher::ReleaseSwitcher;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildMetadata {
    pub runtime: String,
    pub build_command: Option<String>,
    pub build_duration_ms: u64,
    pub tests_passed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Release {
    pub id: ReleaseId,
    pub project_id: ProjectId,
    pub version: String,
    pub commit_sha: String,
    pub commit_message: String,
    pub author: String,
    pub branch: String,
    pub artifacts: Vec<ArtifactId>,
    pub checksums: HashMap<String, String>,
    pub build_metadata: BuildMetadata,
    pub environment: HashMap<String, String>,
    pub created_at: String,
}

impl Release {
    pub fn new(
        project_id: ProjectId,
        version: String,
        commit_sha: String,
        commit_message: String,
        author: String,
        branch: String,
        runtime: String,
    ) -> Self {
        Self {
            id: ReleaseId::new(),
            project_id,
            version,
            commit_sha,
            commit_message,
            author,
            branch,
            artifacts: Vec::new(),
            checksums: HashMap::new(),
            build_metadata: BuildMetadata {
                runtime,
                build_command: None,
                build_duration_ms: 0,
                tests_passed: None,
            },
            environment: HashMap::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}
