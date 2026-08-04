use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedConfig {
    pub project_name: String,
    pub runtime_engine: String,
    pub build_command: String,
    pub start_command: String,
    pub health_endpoint: String,
    pub default_port: u16,
}

pub struct DetectorPipeline;

impl DetectorPipeline {
    pub fn new() -> Self {
        Self
    }

    /// Executes the detection pipeline: ProjectScanner -> RuntimeDetector -> BuildDetector -> HealthDetector -> PortDetector -> ConfigGenerator
    pub fn detect_all(&self, project_path: &Path) -> DetectedConfig {
        let name = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("app")
            .to_string();

        let (runtime, build, start, health, port) = if project_path.join("pom.xml").exists()
            || project_path.join("build.gradle").exists()
        {
            let build_cmd = if project_path.join("build.gradle").exists() {
                "./gradlew build"
            } else {
                "mvn package"
            };
            (
                "Java",
                build_cmd,
                "java -jar build/libs/app.jar",
                "/health",
                8080,
            )
        } else if project_path.join("package.json").exists() {
            ("Node.js", "npm run build", "npm start", "/health", 3000)
        } else if project_path.join("Cargo.toml").exists() {
            (
                "Rust",
                "cargo build --release",
                "./target/release/app",
                "/health",
                8080,
            )
        } else if project_path.join("go.mod").exists() {
            ("Go", "go build -o app", "./app", "/health", 8080)
        } else if project_path.join("requirements.txt").exists()
            || project_path.join("pyproject.toml").exists()
        {
            (
                "Python",
                "pip install -r requirements.txt",
                "python app.py",
                "/health",
                5000,
            )
        } else if project_path.join("Dockerfile").exists() {
            (
                "Docker",
                "docker build -t app .",
                "docker run -p 8080:8080 app",
                "/health",
                8080,
            )
        } else {
            (
                "Generic",
                "echo 'Build complete'",
                "./run.sh",
                "/health",
                8080,
            )
        };

        DetectedConfig {
            project_name: name,
            runtime_engine: runtime.to_string(),
            build_command: build.to_string(),
            start_command: start.to_string(),
            health_endpoint: health.to_string(),
            default_port: port,
        }
    }

    /// Generates zero-boilerplate aegis.toml configuration content.
    pub fn generate_toml(&self, config: &DetectedConfig, project_id_str: &str) -> String {
        format!(
            "# Generated automatically by Aegis Detector Pipeline\n[project]\nid = \"{}\"\nname = \"{}\"\nruntime = \"{}\"\n\n[build]\nbuild_command = \"{}\"\nstart_command = \"{}\"\n\n[deploy]\nstrategy = \"GracefulSwitch\"\nhealth_check_url = \"http://127.0.0.1:{}{}\"\nhealth_check_timeout_secs = 5\nmax_retained_versions = 2\n",
            project_id_str, config.project_name, config.runtime_engine, config.build_command, config.start_command, config.default_port, config.health_endpoint
        )
    }
}

impl Default for DetectorPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_detector_pipeline_rust() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname=\"test\"").unwrap();

        let pipeline = DetectorPipeline::new();
        let config = pipeline.detect_all(temp.path());

        assert_eq!(config.runtime_engine, "Rust");
        assert_eq!(config.build_command, "cargo build --release");
    }

    #[test]
    fn test_detector_pipeline_node() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("package.json"), "{}").unwrap();

        let pipeline = DetectorPipeline::new();
        let config = pipeline.detect_all(temp.path());

        assert_eq!(config.runtime_engine, "Node.js");
        assert_eq!(config.build_command, "npm run build");
    }

    #[test]
    fn test_detector_pipeline_other_runtimes_and_toml() {
        let pipeline = DetectorPipeline::default();

        // Java
        let temp_java = TempDir::new().unwrap();
        std::fs::write(temp_java.path().join("pom.xml"), "<project></project>").unwrap();
        let cfg_java = pipeline.detect_all(temp_java.path());
        assert_eq!(cfg_java.runtime_engine, "Java");
        assert_eq!(cfg_java.default_port, 8080);

        // Go
        let temp_go = TempDir::new().unwrap();
        std::fs::write(temp_go.path().join("go.mod"), "module app").unwrap();
        let cfg_go = pipeline.detect_all(temp_go.path());
        assert_eq!(cfg_go.runtime_engine, "Go");

        // Python
        let temp_py = TempDir::new().unwrap();
        std::fs::write(temp_py.path().join("pyproject.toml"), "").unwrap();
        let cfg_py = pipeline.detect_all(temp_py.path());
        assert_eq!(cfg_py.runtime_engine, "Python");

        // Docker
        let temp_doc = TempDir::new().unwrap();
        std::fs::write(temp_doc.path().join("Dockerfile"), "FROM alpine").unwrap();
        let cfg_doc = pipeline.detect_all(temp_doc.path());
        assert_eq!(cfg_doc.runtime_engine, "Docker");

        // Generic
        let temp_gen = TempDir::new().unwrap();
        let cfg_gen = pipeline.detect_all(temp_gen.path());
        assert_eq!(cfg_gen.runtime_engine, "Generic");

        // Test generate_toml
        let toml_str = pipeline.generate_toml(&cfg_go, "proj-1234");
        assert!(toml_str.contains("proj-1234"));
        assert!(toml_str.contains("runtime = \"Go\""));
        assert!(toml_str.contains("strategy = \"GracefulSwitch\""));
    }
}
