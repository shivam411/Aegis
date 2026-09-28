pub mod detector_pipeline;
pub mod project_config;
pub use detector_pipeline::{DetectedConfig, DetectorPipeline};
pub use project_config::ResolvedProjectConfig;

use async_trait::async_trait;
use std::path::Path;

/// Identifies a project's language runtime from the files in its directory.
///
/// Build, install and start commands come from `aegis.toml` (with defaults
/// supplied by [`DetectorPipeline`]); process lifecycle is owned by the
/// process supervisor. This trait only answers "what kind of project is this?".
#[async_trait]
pub trait Runtime: Send + Sync {
    fn name(&self) -> &str;
    async fn detect(&self, path: &Path) -> bool;
}

macro_rules! file_marker_runtime {
    ($ty:ident, $name:expr, [$($marker:expr),+]) => {
        pub struct $ty;

        #[async_trait]
        impl Runtime for $ty {
            fn name(&self) -> &str {
                $name
            }

            async fn detect(&self, path: &Path) -> bool {
                false $(|| path.join($marker).exists())+
            }
        }
    };
}

file_marker_runtime!(NodeRuntime, "Node.js", ["package.json"]);
file_marker_runtime!(RustRuntime, "Rust", ["Cargo.toml"]);
file_marker_runtime!(GoRuntime, "Go", ["go.mod"]);
file_marker_runtime!(
    PythonRuntime,
    "Python",
    ["requirements.txt", "pyproject.toml"]
);

// ----------------------------------------------------
// Generic Fallback Runtime
// ----------------------------------------------------
pub struct GenericRuntime;

#[async_trait]
impl Runtime for GenericRuntime {
    fn name(&self) -> &str {
        "Generic"
    }

    async fn detect(&self, _path: &Path) -> bool {
        true
    }
}

// ----------------------------------------------------
// Auto-Detection Router
// ----------------------------------------------------
pub struct RuntimeDetector {
    runtimes: Vec<Box<dyn Runtime>>,
}

impl RuntimeDetector {
    pub fn new() -> Self {
        Self {
            runtimes: vec![
                Box::new(NodeRuntime),
                Box::new(RustRuntime),
                Box::new(GoRuntime),
                Box::new(PythonRuntime),
                Box::new(GenericRuntime),
            ],
        }
    }

    pub async fn detect_runtime(&self, project_path: &Path) -> &dyn Runtime {
        for runtime in &self.runtimes {
            if runtime.detect(project_path).await {
                return runtime.as_ref();
            }
        }
        &GenericRuntime
    }
}

impl Default for RuntimeDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_runtime_auto_detection() {
        let detector = RuntimeDetector::new();
        let current_dir = std::env::current_dir().unwrap();
        // Aegis repository contains Cargo.toml -> RustRuntime
        let detected = detector.detect_runtime(&current_dir).await;
        assert_eq!(detected.name(), "Rust");

        let temp = tempfile::TempDir::new().unwrap();
        // 1. Node detection
        std::fs::write(temp.path().join("package.json"), "{}").unwrap();
        assert_eq!(detector.detect_runtime(temp.path()).await.name(), "Node.js");

        // 2. Go detection
        let temp_go = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_go.path().join("go.mod"), "module test").unwrap();
        assert_eq!(detector.detect_runtime(temp_go.path()).await.name(), "Go");

        // 3. Python detection
        let temp_py = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_py.path().join("requirements.txt"), "").unwrap();
        assert_eq!(
            detector.detect_runtime(temp_py.path()).await.name(),
            "Python"
        );

        // 4. Generic fallback
        let temp_gen = tempfile::TempDir::new().unwrap();
        assert_eq!(
            detector.detect_runtime(temp_gen.path()).await.name(),
            "Generic"
        );
    }
}
