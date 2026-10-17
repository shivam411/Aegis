//! Where releases come from (GitHub, or a loopback fake in tests), and
//! turning a release into verified binaries on disk.

use crate::checksum;
use crate::http::Fetcher;
use crate::install::{self, DAEMON_BINARY, SIBLING_BINARIES};
use crate::release::{self, Release, Target, CHECKSUMS_ASSET};
use crate::version::{parse_version, Version};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_API_BYTES: usize = 4 * 1024 * 1024;
const MAX_SUMS_BYTES: usize = 256 * 1024;
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;
const API_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct Source {
    fetcher: Fetcher,
    api_base: String,
    repo: String,
    /// The API base was overridden (tests): assets must come from it too.
    overridden: bool,
}

impl Source {
    /// `api_base`: `None` for GitHub, or a loopback override (validated by
    /// [`crate::validate_api_override`]).
    pub fn new(api_base: Option<String>, repo: &str, user_agent: &str) -> Self {
        let overridden = api_base.is_some();
        Self {
            fetcher: Fetcher::new(user_agent, overridden),
            api_base: api_base.unwrap_or_else(|| crate::DEFAULT_API_BASE.to_string()),
            repo: repo.to_string(),
            overridden,
        }
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    async fn release(&self, path: &str) -> anyhow::Result<Release> {
        let url = format!("{}/repos/{}/releases/{}", self.api_base, self.repo, path);
        let body = self
            .fetcher
            .get(
                &url,
                "application/vnd.github+json",
                MAX_API_BYTES,
                API_TIMEOUT,
            )
            .await?;
        release::parse_release(&String::from_utf8_lossy(&body))
    }

    /// The latest published, non-draft, non-prerelease release.
    pub async fn latest(&self) -> anyhow::Result<Release> {
        self.release("latest").await
    }

    pub async fn by_tag(&self, tag: &str) -> anyhow::Result<Release> {
        if parse_version(tag).is_none() {
            anyhow::bail!("'{}' is not a version tag", tag);
        }
        self.release(&format!("tags/{}", tag)).await
    }

    /// Release assets must be downloads of this repository's releases (or,
    /// in tests, served by the fake API).
    fn check_asset_url(&self, url: &str) -> anyhow::Result<()> {
        let allowed = if self.overridden {
            format!("{}/", self.api_base)
        } else {
            format!("https://github.com/{}/releases/download/", self.repo)
        };
        if !url.starts_with(&allowed) {
            anyhow::bail!("Unexpected download URL {} (expected {}…)", url, allowed);
        }
        Ok(())
    }

    async fn download(&self, url: &str, limit: usize) -> anyhow::Result<Vec<u8>> {
        self.check_asset_url(url)?;
        self.fetcher
            .get(url, "application/octet-stream", limit, DOWNLOAD_TIMEOUT)
            .await
    }

    /// Downloads this target's archive and the release's SHA256SUMS,
    /// verifies the archive and unpacks the binaries.
    pub async fn fetch_bundle(&self, release: &Release, target: &Target) -> anyhow::Result<Bundle> {
        let version = parse_version(&release.tag)
            .ok_or_else(|| anyhow::anyhow!("Release tag '{}' is not a version", release.tag))?;
        if !target.self_update_supported() {
            anyhow::bail!("Self-update is not supported on {}", target);
        }
        let archive_asset = release::select_asset(release, target).ok_or_else(|| {
            anyhow::anyhow!(
                "Release {} has no {} for this machine",
                release.tag,
                target.archive_name()
            )
        })?;
        let sums_asset = release.asset(CHECKSUMS_ASSET).ok_or_else(|| {
            anyhow::anyhow!(
                "Release {} has no {} file, so its downloads can't be verified; refusing to install it",
                release.tag,
                CHECKSUMS_ASSET
            )
        })?;
        let sums_text = self.download(&sums_asset.url, MAX_SUMS_BYTES).await?;
        let sums = checksum::parse_sums(&String::from_utf8_lossy(&sums_text))?;
        let archive = self.download(&archive_asset.url, MAX_ARCHIVE_BYTES).await?;
        checksum::verify(&archive, &archive_asset.name, &sums)?;
        let sha256 = checksum::to_hex(&checksum::sha256(&archive));
        let mut names = vec![DAEMON_BINARY];
        names.extend_from_slice(SIBLING_BINARIES);
        let binaries =
            tokio::task::spawn_blocking(move || crate::archive::extract(&archive, &names))
                .await
                .map_err(|e| anyhow::anyhow!("Unpacking failed: {}", e))??;
        if !binaries.contains_key(DAEMON_BINARY) {
            anyhow::bail!("{} has no {}", archive_asset.name, DAEMON_BINARY);
        }
        Ok(Bundle {
            version,
            tag: release.tag.clone(),
            archive_sha256: sha256,
            binaries,
        })
    }
}

/// Verified binaries from one release.
pub struct Bundle {
    pub version: Version,
    pub tag: String,
    /// Hex SHA-256 of the archive (as listed in SHA256SUMS).
    pub archive_sha256: String,
    pub binaries: HashMap<String, Vec<u8>>,
}

/// Binaries written next to their targets, ready to be swapped in.
pub struct Staged {
    files: Vec<(PathBuf, PathBuf)>,
}

impl Staged {
    pub fn daemon(&self) -> &Path {
        &self.files[0].1
    }

    /// Swaps every staged binary in, keeping `.previous` copies.
    pub fn commit(mut self) -> anyhow::Result<Vec<PathBuf>> {
        let mut replaced = Vec::new();
        for (target, staged) in std::mem::take(&mut self.files) {
            install::commit(&target, &staged)?;
            replaced.push(target);
        }
        Ok(replaced)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        for (_, staged) in &self.files {
            let _ = std::fs::remove_file(staged);
        }
    }
}

impl Bundle {
    /// Writes the daemon binary next to `exe`, and the CLI/TUI next to
    /// theirs when they are installed in the same directory. Nothing is
    /// replaced yet; dropping the result removes the staged files.
    pub fn stage(&self, exe: &Path) -> anyhow::Result<Staged> {
        let mut staged = Staged { files: Vec::new() };
        let daemon = &self.binaries[DAEMON_BINARY];
        staged
            .files
            .push((exe.to_path_buf(), install::stage(exe, daemon)?));
        for name in SIBLING_BINARIES {
            let target = exe.with_file_name(name);
            if let (Some(bytes), true) = (self.binaries.get(*name), target.is_file()) {
                staged
                    .files
                    .push((target.clone(), install::stage(&target, bytes)?));
            }
        }
        Ok(staged)
    }
}
