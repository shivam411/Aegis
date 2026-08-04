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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_git_repository_struct_and_head_commit() {
        let current_dir = std::env::current_dir().unwrap();
        let repo = GitRepository::new("https://github.com/shivam411/Aegis.git".to_string(), current_dir.clone());

        assert_eq!(repo.url, "https://github.com/shivam411/Aegis.git");
        assert_eq!(repo.path, current_dir);

        let sha_res = repo.get_head_commit().await;
        assert!(sha_res.is_ok());
        let sha = sha_res.unwrap();
        assert_eq!(sha.len(), 40);

        // Test sync on existing repository path (runs git fetch origin)
        assert!(repo.sync("main").await.is_ok());

        // Test get_head_commit failure on invalid directory
        let temp = tempfile::TempDir::new().unwrap();
        let invalid_repo = GitRepository::new("https://invalid.url".to_string(), temp.path().to_path_buf());
        assert!(invalid_repo.get_head_commit().await.is_err());
    }
}
