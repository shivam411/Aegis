use std::path::{Path, PathBuf};
use aegis_types::{ArtifactId, ReleaseId};

pub struct ArtifactStore {
    base_dir: PathBuf,
}

impl ArtifactStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub async fn store(
        &self,
        _release_id: &ReleaseId,
        _source_path: &Path,
    ) -> Result<ArtifactId, anyhow::Error> {
        Ok(ArtifactId::new())
    }

    pub async fn get(&self, _release_id: &ReleaseId) -> Result<PathBuf, anyhow::Error> {
        Ok(self.base_dir.clone())
    }
}
