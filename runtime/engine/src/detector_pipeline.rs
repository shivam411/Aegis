use aegis_config::{BuildSection, DeploySection, ProjectFile, ProjectSection};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedConfig {
    pub project_name: String,
    pub runtime_engine: String,
    /// Dependency installation, run before `build_command`.
    pub install_command: Option<String>,
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

        let (runtime, install, build, start, health, port): (
            &str,
            Option<&str>,
            String,
            String,
            &str,
            u16,
        ) = if project_path.join("pom.xml").exists() || project_path.join("build.gradle").exists() {
            let build_cmd = if project_path.join("build.gradle").exists() {
                "./gradlew build"
            } else {
                "mvn package"
            };
            (
                "Java",
                None,
                build_cmd.to_string(),
                "java -jar build/libs/app.jar".to_string(),
                "/health",
                8080,
            )
        } else if project_path.join("package.json").exists() {
            let install = if project_path.join("package-lock.json").exists() {
                "npm ci --no-audit --no-fund"
            } else {
                "npm install --no-audit --no-fund"
            };
            (
                "Node.js",
                Some(install),
                "npm run build --if-present".to_string(),
                "npm start".to_string(),
                "/health",
                3000,
            )
        } else if project_path.join("Cargo.toml").exists() {
            let bin = cargo_package_name(project_path).unwrap_or_else(|| "app".to_string());
            (
                "Rust",
                None,
                "cargo build --release".to_string(),
                format!("./target/release/{}", bin),
                "/health",
                8080,
            )
        } else if project_path.join("go.mod").exists() {
            (
                "Go",
                Some("go mod download"),
                "go build -o app".to_string(),
                "./app".to_string(),
                "/health",
                8080,
            )
        } else if project_path.join("requirements.txt").exists()
            || project_path.join("pyproject.toml").exists()
        {
            (
                "Python",
                None,
                "pip install -r requirements.txt".to_string(),
                "python app.py".to_string(),
                "/health",
                5000,
            )
        } else if project_path.join("Dockerfile").exists() {
            (
                "Docker",
                None,
                "docker build -t app .".to_string(),
                "docker run -p 8080:8080 app".to_string(),
                "/health",
                8080,
            )
        } else {
            (
                "Generic",
                None,
                "echo 'Build complete'".to_string(),
                "./run.sh".to_string(),
                "/health",
                8080,
            )
        };

        DetectedConfig {
            project_name: name,
            runtime_engine: runtime.to_string(),
            install_command: install.map(str::to_string),
            build_command: build,
            start_command: start,
            health_endpoint: health.to_string(),
            default_port: port,
        }
    }

    /// Builds the project part of `aegis.toml` from detected settings.
    pub fn to_project_file(&self, config: &DetectedConfig, project_id_str: &str) -> ProjectFile {
        ProjectFile {
            project: ProjectSection {
                id: Some(project_id_str.to_string()),
                name: Some(config.project_name.clone()),
                runtime: Some(config.runtime_engine.clone()),
            },
            build: BuildSection {
                install_command: config.install_command.clone(),
                build_command: Some(config.build_command.clone()),
                test_command: None,
                start_command: Some(config.start_command.clone()),
                timeout_secs: None,
            },
            deploy: DeploySection {
                strategy: Some("GracefulSwitch".to_string()),
                port: Some(config.default_port),
                health_check_url: Some(format!(
                    "http://127.0.0.1:{}{}",
                    config.default_port, config.health_endpoint
                )),
                health_check_timeout_secs: Some(30),
                max_retained_versions: Some(2),
                drain_timeout_secs: None,
                restart_policy: None,
            },
            env: Default::default(),
        }
    }

    /// Generates zero-boilerplate aegis.toml configuration content.
    pub fn generate_toml(&self, config: &DetectedConfig, project_id_str: &str) -> String {
        let file = self.to_project_file(config, project_id_str);
        format!(
            "# Generated automatically by Aegis Detector Pipeline\n{}",
            toml::to_string_pretty(&file).unwrap_or_default()
        )
    }
}

/// Reads `[package].name` from Cargo.toml, which is the default binary name.
fn cargo_package_name(project_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(project_path.join("Cargo.toml")).ok()?;
    let value: toml::Value = toml::from_str(&content).ok()?;
    value
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
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
        assert_eq!(config.start_command, "./target/release/test");
    }

    #[test]
    fn test_detector_pipeline_node() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("package.json"), "{}").unwrap();

        let pipeline = DetectorPipeline::new();
        let config = pipeline.detect_all(temp.path());

        assert_eq!(config.runtime_engine, "Node.js");
        assert_eq!(config.build_command, "npm run build --if-present");
        assert_eq!(
            config.install_command.as_deref(),
            Some("npm install --no-audit --no-fund")
        );
    }

    #[test]
    fn test_detector_pipeline_other_runtimes_and_toml() {
        let pipeline = DetectorPipeline::new();

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

        // Round-trips through the typed parser, including awkward names.
        let mut odd = cfg_go.clone();
        odd.project_name = "my \"quoted\" app".to_string();
        let parsed = ProjectFile::parse(&pipeline.generate_toml(&odd, "proj-1")).unwrap();
        assert_eq!(parsed.project.name.as_deref(), Some("my \"quoted\" app"));
        assert_eq!(
            parsed.build.install_command.as_deref(),
            Some("go mod download")
        );
        assert_eq!(parsed.deploy.port, Some(8080));
    }
}
