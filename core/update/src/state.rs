//! What the updater remembers across restarts (`<data_dir>/update/state.json`).

use crate::release::Release;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Persisted {
    /// The version that last ran with this data directory.
    pub last_version: Option<String>,
    /// The newest release an `UpdateAvailable` event was recorded for.
    pub notified_tag: Option<String>,
    pub last_check: Option<LastCheck>,
    pub last_update: Option<AppliedUpdate>,
    /// An update that was installed and awaits the restart.
    pub pending: Option<Pending>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LastCheck {
    pub checked_at: String,
    pub release: Release,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedUpdate {
    pub from_version: String,
    pub to_version: String,
    /// RFC 3339, when the new version first started.
    pub at: String,
    /// `in_place`, `helper` or `external` (install.sh, `aegis-daemon
    /// self-update`, a package manager...).
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub from_version: String,
    pub to_version: String,
    pub method: String,
    pub backup: Option<String>,
    pub actor: String,
    pub started_at: String,
}

pub(crate) fn path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

pub(crate) fn load(dir: &Path) -> Persisted {
    match std::fs::read(path(dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Ignoring unreadable update state");
            Persisted::default()
        }),
        Err(_) => Persisted::default(),
    }
}

/// Writes `value` to `dir/name` via a temporary file and a rename.
pub(crate) fn write_json_atomic(
    dir: &Path,
    name: &str,
    value: &impl Serialize,
) -> anyhow::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}.tmp", name, std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, dir.join(name)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

pub(crate) fn save(dir: &Path, state: &Persisted) {
    if let Err(e) = write_json_atomic(dir, "state.json", state) {
        tracing::warn!(error = %e, dir = %dir.display(), "Cannot save update state");
    }
}
