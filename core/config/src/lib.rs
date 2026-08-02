use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn default_max_retained_versions() -> usize {
    2
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DaemonConfig {
    pub host: String,
    pub port: u16,
    pub database_path: PathBuf,
    pub log_level: String,
    #[serde(default = "default_max_retained_versions")]
    pub max_retained_versions: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub daemon: DaemonConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig {
                host: "127.0.0.1".to_string(),
                port: 50051,
                database_path: PathBuf::from("aegis.db"),
                log_level: "info".to_string(),
                max_retained_versions: 2,
            },
        }
    }
}

impl Config {
    /// Loads the configuration from the specified path.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, anyhow::Error> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Loads the configuration from the specified path or returns the default configuration if loading fails.
    pub fn load_or_default<P: AsRef<Path>>(path: P) -> Self {
        Self::load_from_file(path).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.daemon.host, "127.0.0.1");
        assert_eq!(config.daemon.port, 50051);
        assert_eq!(config.daemon.log_level, "info");
    }

    #[test]
    fn test_load_from_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let toml_content = r#"
[daemon]
host = "0.0.0.0"
port = 9000
database_path = "test.db"
log_level = "debug"
"#;
        temp_file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::load_from_file(temp_file.path()).unwrap();
        assert_eq!(config.daemon.host, "0.0.0.0");
        assert_eq!(config.daemon.port, 9000);
        assert_eq!(config.daemon.database_path.to_str().unwrap(), "test.db");
        assert_eq!(config.daemon.log_level, "debug");
    }
}

