//! Replacing the installed binaries, and working out whether this process
//! may do that itself.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Binaries shipped in a release archive that are replaced when they sit
/// next to the daemon (the daemon itself must be present).
pub const DAEMON_BINARY: &str = "aegis-daemon";
pub const SIBLING_BINARIES: &[&str] = &["aegis-cli", "aegis-tui"];

/// Where `install.sh --server` installs the root-owned helper's path unit.
pub const HELPER_PATH_UNIT: &str = "/etc/systemd/system/aegis-update.path";

/// How an update can be applied on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallMode {
    /// The daemon can write the directory its binary is in: it replaces the
    /// binary and restarts itself.
    InPlace,
    /// The binary is root-owned, but install.sh's `aegis-update.path` unit is
    /// installed: the daemon writes a request and a root-owned one-shot
    /// service downloads, verifies and installs the release, then restarts
    /// the daemon.
    Helper,
    /// Neither: an administrator runs the command shown.
    Manual,
    /// Not on this platform (Windows releases are zip files).
    Unsupported,
}

impl InstallMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InPlace => "in_place",
            Self::Helper => "helper",
            Self::Manual => "manual",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Whether this process can create (and so rename) files in `dir`. Tried
/// for real: permissions, read-only mounts (systemd's ProtectSystem=) and
/// ACLs all count.
pub fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".aegis-write-test-{}", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// How an update can be installed for the binary at `exe`.
pub fn detect_mode(exe: &Path, helper_unit: &Path, self_update_supported: bool) -> InstallMode {
    if !self_update_supported {
        return InstallMode::Unsupported;
    }
    match exe.parent() {
        Some(dir) if exe.is_file() && dir_writable(dir) => InstallMode::InPlace,
        _ if helper_unit.is_file() => InstallMode::Helper,
        _ => InstallMode::Manual,
    }
}

/// `<dir>/<name>.previous`: the binary that was replaced, for rollback.
pub fn previous_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".previous");
    target.with_file_name(name)
}

fn staged_path(target: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(format!(".new-{}", std::process::id()));
    target.with_file_name(name)
}

/// Writes `contents` next to `target` (same directory, so the final rename is
/// atomic), executable and flushed to disk.
pub fn stage(target: &Path, contents: &[u8]) -> anyhow::Result<PathBuf> {
    let staged = staged_path(target);
    let _ = fs::remove_file(&staged);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&staged)?;
        file.write_all(contents)?;
        file.sync_all()
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&staged);
        anyhow::bail!("Cannot write {}: {}", staged.display(), e);
    }
    Ok(staged)
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Moves a staged file over `target`, keeping the old one as
/// `<target>.previous`. On Unix the swap is a single rename, so `target`
/// always exists (old or new binary); the running process keeps its open
/// copy of the old one.
pub fn commit(target: &Path, staged: &Path) -> anyhow::Result<PathBuf> {
    let previous = previous_path(target);
    let _ = fs::remove_file(&previous);
    #[cfg(unix)]
    {
        if target.exists() && fs::hard_link(target, &previous).is_err() {
            fs::copy(target, &previous).map_err(|e| {
                anyhow::anyhow!(
                    "Cannot keep {} as {}: {}",
                    target.display(),
                    previous.display(),
                    e
                )
            })?;
        }
        fs::rename(staged, target)
            .map_err(|e| anyhow::anyhow!("Cannot replace {}: {}", target.display(), e))?;
    }
    #[cfg(not(unix))]
    {
        // A running .exe can be renamed but not overwritten.
        if target.exists() {
            fs::rename(target, &previous)
                .map_err(|e| anyhow::anyhow!("Cannot move {} aside: {}", target.display(), e))?;
        }
        if let Err(e) = fs::rename(staged, target) {
            let _ = fs::rename(&previous, target);
            anyhow::bail!("Cannot replace {}: {}", target.display(), e);
        }
    }
    if let Some(dir) = target.parent() {
        sync_dir(dir);
    }
    Ok(previous)
}

/// Stages then commits: atomically replaces `target`, keeping
/// `<target>.previous`.
pub fn replace_binary(target: &Path, contents: &[u8]) -> anyhow::Result<PathBuf> {
    let staged = stage(target, contents)?;
    commit(target, &staged).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })
}

/// Runs `<binary> --version` and checks that it reports `expected`: proves
/// the new binary runs on this machine (architecture, libc) before it
/// replaces the working one.
pub async fn check_runs(binary: &Path, expected: &crate::Version) -> anyhow::Result<()> {
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(binary)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("The new binary did not answer --version within 20s"))?
    .map_err(|e| anyhow::anyhow!("The new binary does not run on this machine: {}", e))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let reported = text
        .split_whitespace()
        .nth(1)
        .and_then(crate::parse_version);
    if !output.status.success() || reported.as_ref() != Some(expected) {
        anyhow::bail!(
            "The new binary reports '{}' instead of version {}",
            text.trim(),
            expected
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_atomically_and_keeps_the_previous_binary() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("aegis-daemon");
        fs::write(&target, b"old").unwrap();
        let previous = replace_binary(&target, b"new").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(previous, dir.path().join("aegis-daemon.previous"));
        assert_eq!(fs::read(&previous).unwrap(), b"old");
        // Again: the previous binary is the one just replaced.
        replace_binary(&target, b"newer").unwrap();
        assert_eq!(fs::read(&previous).unwrap(), b"new");
        // No staging files left behind.
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn detects_the_install_mode() {
        let dir = tempfile::TempDir::new().unwrap();
        let exe = dir.path().join("aegis-daemon");
        fs::write(&exe, b"x").unwrap();
        let unit = dir.path().join("aegis-update.path");
        assert_eq!(detect_mode(&exe, &unit, true), InstallMode::InPlace);
        assert_eq!(detect_mode(&exe, &unit, false), InstallMode::Unsupported);
        // A directory that isn't there can't be written.
        let missing = dir.path().join("gone").join("aegis-daemon");
        assert!(!dir_writable(&dir.path().join("gone")));
        assert_eq!(detect_mode(&missing, &unit, true), InstallMode::Manual);
        fs::write(&unit, b"[Path]").unwrap();
        assert_eq!(detect_mode(&missing, &unit, true), InstallMode::Helper);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_binary_in_a_read_only_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let exe = bin.join("aegis-daemon");
        fs::write(&exe, b"old").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
        // root ignores directory permissions; nothing to check then.
        if dir_writable(&bin) {
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let unit = dir.path().join("no-such-unit");
        assert_eq!(detect_mode(&exe, &unit, true), InstallMode::Manual);
        assert!(replace_binary(&exe, b"new").is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
