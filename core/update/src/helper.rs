//! The privileged side of self-update.
//!
//! With install.sh's server install the binaries are root-owned and the
//! daemon runs as the unprivileged `aegis` user under `ProtectSystem=strict`,
//! so it can't replace them. Instead it writes `<data_dir>/update/request.json`
//! holding only the version it wants. The root-owned `aegis-update.path` unit
//! notices the file and starts `aegis-update.service`, which runs
//! `aegis-daemon self-update --from-request`: it deletes the request, then
//! downloads and verifies that release itself (it trusts nothing the daemon
//! wrote except the version string, and only installs a version newer than
//! its own), swaps the binaries and restarts the daemon. On failure it
//! writes `result.json` for the daemon to report.
//!
//! The same [`self_update`] runs for `sudo aegis-daemon self-update`.

use crate::install;
use crate::release::{Release, Target};
use crate::source::Source;
use crate::version::{is_major_jump, parse_version, Version};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const REQUEST_FILE: &str = "request.json";
pub const RESULT_FILE: &str = "result.json";
const MAX_REQUEST_BYTES: u64 = 4096;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFile {
    version: String,
}

/// Daemon side: asks the helper to install `tag`.
pub fn write_request(dir: &Path, tag: &str) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(dir.join(RESULT_FILE));
    crate::state::write_json_atomic(
        dir,
        REQUEST_FILE,
        &RequestFile {
            version: tag.to_string(),
        },
    )
}

/// Helper side: takes the request (the file is removed whether or not it
/// is valid, so a bad request can't re-trigger the path unit). Returns the
/// requested tag. Symlinks and oversized files are rejected unread.
pub fn take_request(dir: &Path) -> anyhow::Result<Option<String>> {
    let path = dir.join(REQUEST_FILE);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let content = if meta.file_type().is_file() && meta.len() <= MAX_REQUEST_BYTES {
        let mut text = String::new();
        std::fs::File::open(&path)?
            .take(MAX_REQUEST_BYTES)
            .read_to_string(&mut text)
            .ok();
        Some(text)
    } else {
        None
    };
    std::fs::remove_file(&path)?;
    let tag = content
        .and_then(|text| serde_json::from_str::<RequestFile>(&text).ok())
        .map(|r| r.version)
        .filter(|v| v.len() <= 64 && parse_version(v).is_some())
        .ok_or_else(|| anyhow::anyhow!("Ignoring an invalid update request"))?;
    Ok(Some(tag))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperResult {
    pub ok: bool,
    pub message: String,
}

/// Helper side: reports the outcome. The file is created fresh (never
/// through an existing file or symlink in the daemon-writable directory)
/// and renamed into place.
pub fn write_result(dir: &Path, ok: bool, message: &str) -> anyhow::Result<()> {
    crate::state::write_json_atomic(
        dir,
        RESULT_FILE,
        &HelperResult {
            ok,
            message: message.to_string(),
        },
    )
}

/// Daemon side: the helper's report, if any (removed once read).
pub fn take_result(dir: &Path) -> Option<HelperResult> {
    let path = dir.join(RESULT_FILE);
    let bytes = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_slice(&bytes).ok()
}

/// What `self_update` installed.
#[derive(Debug)]
pub struct Installed {
    pub from: Version,
    pub to: Version,
    pub tag: String,
    pub archive_sha256: String,
    pub replaced: Vec<PathBuf>,
}

/// Options for [`self_update`].
pub struct SelfUpdate<'a> {
    pub source: &'a Source,
    /// The binary to replace (normally `std::env::current_exe()`).
    pub exe: &'a Path,
    pub target: Target,
    pub current: Version,
    /// A specific tag; the latest release when `None`.
    pub wanted: Option<&'a str>,
    /// Allow reinstalls, downgrades, pre-releases and major-version jumps.
    pub force: bool,
    /// Run the new binary's `--version` before swapping it in.
    pub verify_runs: bool,
}

/// Why `release` must not be installed over `current` without `force`.
pub fn refusal(release: &Release, current: &Version, force: bool) -> Option<String> {
    let Some(version) = parse_version(&release.tag) else {
        return Some(format!("Release tag '{}' is not a version", release.tag));
    };
    if release.draft {
        return Some(format!("{} is a draft release", release.tag));
    }
    if force {
        return None;
    }
    if release.prerelease || !version.is_stable() {
        return Some(format!(
            "{} is a pre-release; use --force to install it",
            release.tag
        ));
    }
    if version <= *current {
        return Some(format!(
            "{} is not newer than the running version {}; use --force to reinstall or downgrade",
            release.tag, current
        ));
    }
    if is_major_jump(current, &version) {
        return Some(format!(
            "{} is a new major version; read its release notes, then use --force",
            release.tag
        ));
    }
    None
}

/// Downloads, verifies and installs a release over `exe` (and the CLI/TUI
/// next to it), keeping `.previous` copies. Does not restart anything.
pub async fn self_update(opts: SelfUpdate<'_>) -> anyhow::Result<Installed> {
    let release = match opts.wanted {
        Some(tag) => opts.source.by_tag(tag).await?,
        None => opts.source.latest().await?,
    };
    if let Some(reason) = refusal(&release, &opts.current, opts.force) {
        anyhow::bail!(reason);
    }
    let bundle = opts.source.fetch_bundle(&release, &opts.target).await?;
    let staged = bundle.stage(opts.exe)?;
    if opts.verify_runs {
        install::check_runs(staged.daemon(), &bundle.version).await?;
    }
    let replaced = staged.commit()?;
    Ok(Installed {
        from: opts.current,
        to: bundle.version,
        tag: bundle.tag,
        archive_sha256: bundle.archive_sha256,
        replaced,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_validated_and_consumed() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(take_request(dir.path()).unwrap(), None);
        write_request(dir.path(), "v0.3.0").unwrap();
        assert_eq!(take_request(dir.path()).unwrap().as_deref(), Some("v0.3.0"));
        assert!(!dir.path().join(REQUEST_FILE).exists());
        for bad in [
            r#"{"version": "latest"}"#,
            r#"{"version": "v1.0.0", "url": "https://evil.example"}"#,
            "not json",
        ] {
            std::fs::write(dir.path().join(REQUEST_FILE), bad).unwrap();
            assert!(take_request(dir.path()).is_err(), "{bad}");
            assert!(!dir.path().join(REQUEST_FILE).exists());
        }
        std::fs::write(dir.path().join(REQUEST_FILE), vec![b' '; 10_000]).unwrap();
        assert!(take_request(dir.path()).is_err());
    }

    #[test]
    fn results_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(take_result(dir.path()), None);
        write_result(dir.path(), false, "checksum mismatch").unwrap();
        let r = take_result(dir.path()).unwrap();
        assert!(!r.ok);
        assert_eq!(r.message, "checksum mismatch");
        assert_eq!(take_result(dir.path()), None);
    }

    #[test]
    fn never_downgrades_without_force() {
        let rel = |tag: &str, pre: bool| Release {
            tag: tag.into(),
            name: tag.into(),
            html_url: String::new(),
            published_at: None,
            prerelease: pre,
            draft: false,
            assets: vec![],
        };
        let cur = parse_version("1.2.0").unwrap();
        assert_eq!(refusal(&rel("v1.3.0", false), &cur, false), None);
        assert!(refusal(&rel("v1.2.0", false), &cur, false).is_some());
        assert!(refusal(&rel("v1.1.0", false), &cur, false)
            .unwrap()
            .contains("not newer"));
        assert!(refusal(&rel("v1.3.0-rc.1", false), &cur, false).is_some());
        assert!(refusal(&rel("v1.3.0", true), &cur, false).is_some());
        assert!(refusal(&rel("v2.0.0", false), &cur, false)
            .unwrap()
            .contains("major"));
        // --force allows all of those.
        for (tag, pre) in [("v1.1.0", false), ("v1.3.0-rc.1", true), ("v2.0.0", false)] {
            assert_eq!(refusal(&rel(tag, pre), &cur, true), None, "{tag}");
        }
        assert!(refusal(&rel("nightly", false), &cur, true).is_some());
        let mut draft = rel("v1.3.0", false);
        draft.draft = true;
        assert!(refusal(&draft, &cur, true).is_some());
    }
}
