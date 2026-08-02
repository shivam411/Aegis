use std::path::PathBuf;
use tokio::process::Command;

pub struct GitRepository {
    pub url: String,
    pub path: PathBuf,
}

impl GitRepository {
    pub fn new(url: String, path: PathBuf) -> Self {
        Self { url, path }
    }

    /// Clones the repository if missing, or fetches latest commits.
    pub async fn sync(&self, branch: &str) -> Result<(), anyhow::Error> {
        if !self.path.exists() {
            tracing::info!(url = %self.url, path = %self.path.display(), "Cloning repository");
            let status = Command::new("git")
                .args(["clone", "--branch", branch, &self.url, self.path.to_str().unwrap()])
                .status()
                .await?;
            if !status.success() {
                anyhow::bail!("git clone failed");
            }
        } else {
            tracing::info!(path = %self.path.display(), "Fetching repository updates");
            let _ = Command::new("git")
                .args(["fetch", "origin"])
                .current_dir(&self.path)
                .status()
                .await;
        }
        Ok(())
    }

    /// Fetches the latest HEAD commit SHA.
    pub async fn get_head_commit(&self) -> Result<String, anyhow::Error> {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.path)
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("Failed to get git HEAD commit");
        }
        let sha = String::from_utf8(output.stdout)?.trim().to_string();
        Ok(sha)
    }
}
