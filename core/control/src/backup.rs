//! Backups and offline restores.
//!
//! A backup is one `.tar.gz` archive (tar keeps Unix modes and symlinks,
//! which release directories need; gzip streams, so archives are written
//! and verified without holding them in memory):
//!
//! ```text
//! manifest.json                     always the first entry
//! aegis.db                          consistent snapshot (VACUUM INTO)
//! secrets.key                       without it, stored secrets are unreadable
//! certs/...                         ACME certificates and account key
//! projects/<id>/current.json        only with releases
//! projects/<id>/releases/<v>/...    only with releases
//! artifacts/...                     only with releases (release metadata)
//! ```
//!
//! The manifest lists every entry with its size and SHA-256. Restores check
//! the archive against it, refuse anything outside the layout above, and are
//! done while the daemon is stopped (see [`restore`] and [`DataDirLock`]).

use crate::{ControlError, ControlPlane, ControlResult};
use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub const MANIFEST: &str = "manifest.json";
pub const DB_ENTRY: &str = "aegis.db";
pub const KEY_ENTRY: &str = "secrets.key";
pub const CERTS_DIR: &str = "certs";
pub const PROJECTS_DIR: &str = "projects";
pub const ARTIFACTS_DIR: &str = "artifacts";
/// Archive layout version; restores refuse newer formats.
pub const FORMAT: u32 = 1;
/// The lock file the daemon holds in its data directory while it runs.
pub const LOCK_FILE: &str = "aegis.lock";

const NAME_PREFIX: &str = "aegis-backup-";
const NAME_SUFFIX: &str = ".tar.gz";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

/// Where backups are kept: `<data_dir>/backups`.
pub fn backups_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("backups")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub aegis_version: String,
    pub created_at: String,
    /// The newest database migration applied in the snapshot.
    pub schema_version: i64,
    pub includes_releases: bool,
    pub contents: Vec<ManifestEntry>,
}

impl Manifest {
    pub fn file_count(&self) -> usize {
        self.contents
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .count()
    }

    fn has(&self, path: &str) -> bool {
        self.contents.iter().any(|e| e.path == path)
    }
}

/// A backup in the backups directory.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub created_at: String,
    pub includes_releases: bool,
    pub aegis_version: String,
    pub file_count: usize,
    /// Set when the archive's manifest can't be read.
    pub problem: Option<String>,
}

/// Backup settings (from `[backup]` in the daemon config).
#[derive(Debug, Clone)]
pub struct BackupSettings {
    /// Backups kept in the backups directory.
    pub keep: usize,
    /// Default for backups that don't say.
    pub include_releases: bool,
    /// Recorded in each manifest; restores of other versions need `--force`.
    pub aegis_version: String,
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            keep: 7,
            include_releases: true,
            aegis_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

// ----------------------------------------------------------------------
// Names and retention
// ----------------------------------------------------------------------

/// `aegis-backup-20261016T120000123Z.tar.gz`: sorts chronologically.
pub fn new_backup_name(now: DateTime<Utc>) -> String {
    format!(
        "{}{}{}",
        NAME_PREFIX,
        now.format("%Y%m%dT%H%M%S%3fZ"),
        NAME_SUFFIX
    )
}

/// Accepts only names Aegis generates, so a name can never be a path.
pub fn validate_backup_name(name: &str) -> Result<(), String> {
    let stem = name
        .strip_prefix(NAME_PREFIX)
        .and_then(|rest| rest.strip_suffix(NAME_SUFFIX));
    let valid = matches!(stem, Some(s) if !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if valid {
        Ok(())
    } else {
        Err(format!("'{}' is not a backup name", name))
    }
}

/// The backups in `dir`, oldest first.
pub fn backup_files(dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if validate_backup_name(&name).is_ok() && entry.file_type()?.is_file() {
            found.push((name, entry.path()));
        }
    }
    found.sort();
    Ok(found)
}

/// Deletes the oldest backups beyond `keep`. Returns the deleted names.
pub fn apply_retention(dir: &Path, keep: usize) -> io::Result<Vec<String>> {
    let files = backup_files(dir)?;
    let excess = files.len().saturating_sub(keep.max(1));
    let mut removed = Vec::new();
    for (name, path) in files.into_iter().take(excess) {
        fs::remove_file(&path)?;
        removed.push(name);
    }
    Ok(removed)
}

/// Details of one archive, read from its manifest.
pub fn backup_info(path: &Path) -> io::Result<BackupInfo> {
    let meta = fs::metadata(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(match read_manifest(path) {
        Ok(m) => BackupInfo {
            name,
            path: path.to_path_buf(),
            size_bytes: meta.len(),
            created_at: m.created_at.clone(),
            includes_releases: m.includes_releases,
            aegis_version: m.aegis_version.clone(),
            file_count: m.file_count(),
            problem: None,
        },
        Err(e) => BackupInfo {
            name,
            path: path.to_path_buf(),
            size_bytes: meta.len(),
            created_at: meta
                .modified()
                .map(|t| DateTime::<Utc>::from(t).to_rfc3339())
                .unwrap_or_default(),
            includes_releases: false,
            aegis_version: String::new(),
            file_count: 0,
            problem: Some(format!("{:#}", e)),
        },
    })
}

/// All backups in `dir`, newest first.
pub fn list_backups(dir: &Path) -> io::Result<Vec<BackupInfo>> {
    let mut list = Vec::new();
    for (_, path) in backup_files(dir)?.into_iter().rev() {
        list.push(backup_info(&path)?);
    }
    Ok(list)
}

// ----------------------------------------------------------------------
// Creating archives
// ----------------------------------------------------------------------

/// A file or directory to put in the archive.
struct Source {
    archive_path: String,
    disk: PathBuf,
    kind: EntryKind,
    mode: u32,
    mtime: u64,
    target: Option<String>,
}

fn source(archive_path: String, disk: &Path) -> anyhow::Result<Source> {
    let meta = fs::symlink_metadata(disk)?;
    let kind = if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else if meta.is_dir() {
        EntryKind::Dir
    } else if meta.is_file() {
        EntryKind::File
    } else {
        anyhow::bail!("{} is not a file or directory", disk.display());
    };
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    };
    #[cfg(not(unix))]
    let mode = if kind == EntryKind::Dir { 0o755 } else { 0o644 };
    let target = match kind {
        EntryKind::Symlink => Some(
            fs::read_link(disk)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Non-UTF-8 symlink target in {}", disk.display()))?
                .to_string(),
        ),
        _ => None,
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(Source {
        archive_path,
        disk: disk.to_path_buf(),
        kind,
        mode,
        mtime,
        target,
    })
}

/// Adds `disk` (and, for a directory, everything below it) as `archive_path`.
fn walk(archive_path: &str, disk: &Path, out: &mut Vec<Source>) -> anyhow::Result<()> {
    let entry = source(archive_path.to_string(), disk)?;
    let is_dir = entry.kind == EntryKind::Dir;
    out.push(entry);
    if !is_dir {
        return Ok(());
    }
    let mut children: Vec<_> = fs::read_dir(disk)?.collect::<Result<_, _>>()?;
    children.sort_by_key(|e| e.file_name());
    for child in children {
        let name = child.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Non-UTF-8 file name in {}", disk.display()))?;
        walk(&format!("{}/{}", archive_path, name), &child.path(), out)?;
    }
    Ok(())
}

fn collect_sources(
    data_dir: &Path,
    db_snapshot: &Path,
    include_releases: bool,
) -> anyhow::Result<Vec<Source>> {
    let mut sources = vec![source(DB_ENTRY.into(), db_snapshot)?];
    sources[0].mode = 0o600;
    let key = data_dir.join(KEY_ENTRY);
    if key.exists() {
        sources.push(source(KEY_ENTRY.into(), &key)?);
    }
    let certs = data_dir.join(CERTS_DIR);
    if certs.is_dir() {
        walk(CERTS_DIR, &certs, &mut sources)?;
    }
    if include_releases {
        // Release metadata; rollbacks check it before switching.
        let artifacts = data_dir.join(ARTIFACTS_DIR);
        if artifacts.is_dir() {
            walk(ARTIFACTS_DIR, &artifacts, &mut sources)?;
        }
        let projects = data_dir.join(PROJECTS_DIR);
        if projects.is_dir() {
            let mut dirs: Vec<_> = fs::read_dir(&projects)?.collect::<Result<_, _>>()?;
            dirs.sort_by_key(|e| e.file_name());
            for dir in dirs {
                if !dir.file_type()?.is_dir() {
                    continue;
                }
                let Ok(id) = dir.file_name().into_string() else {
                    continue;
                };
                let base = format!("{}/{}", PROJECTS_DIR, id);
                let pointer = dir.path().join("current.json");
                if pointer.is_file() {
                    sources.push(source(format!("{}/current.json", base), &pointer)?);
                }
                let releases = dir.path().join("releases");
                if releases.is_dir() {
                    walk(&format!("{}/releases", base), &releases, &mut sources)?;
                }
            }
        }
    }
    for s in &sources {
        check_entry_path(&s.archive_path)?;
    }
    Ok(sources)
}

/// Counts and hashes what passes through.
struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    count: u64,
}

impl<R: Read> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            count: 0,
        }
    }

    fn finish(self) -> (u64, String) {
        (self.count, hex(&self.hasher.finalize()))
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let mut reader = HashingReader::new(File::open(path)?);
    io::copy(&mut reader, &mut io::sink())?;
    Ok(reader.finish())
}

/// Creates `path` for writing, readable only by its owner.
fn create_private(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Creates a directory (and parents) readable only by its owner.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn header(kind: tar::EntryType, size: u64, mode: u32, mtime: u64) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_size(size);
    header.set_mode(mode);
    header.set_mtime(mtime);
    header
}

/// Writes the archive to `out` (which must not exist). Files are hashed
/// first for the manifest, then hashed again while being archived; a file
/// that changed in between fails the backup instead of producing an archive
/// that would not verify.
fn write_archive(
    out: &Path,
    mut manifest: Manifest,
    sources: &[Source],
) -> anyhow::Result<Manifest> {
    for s in sources {
        let (size, sha256) = match s.kind {
            EntryKind::File => {
                let (size, sha) = hash_file(&s.disk)?;
                (Some(size), Some(sha))
            }
            _ => (None, None),
        };
        manifest.contents.push(ManifestEntry {
            path: s.archive_path.clone(),
            kind: s.kind,
            size,
            sha256,
            target: s.target.clone(),
        });
    }
    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    let now = Utc::now().timestamp().max(0) as u64;

    let file = create_private(out)?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    builder.follow_symlinks(false);
    let mut h = header(
        tar::EntryType::Regular,
        manifest_json.len() as u64,
        0o600,
        now,
    );
    builder.append_data(&mut h, MANIFEST, manifest_json.as_slice())?;

    for (s, entry) in sources.iter().zip(&manifest.contents) {
        match s.kind {
            EntryKind::File => {
                let size = entry.size.unwrap_or(0);
                let mut h = header(tar::EntryType::Regular, size, s.mode, s.mtime);
                let mut reader = HashingReader::new(File::open(&s.disk)?.take(size));
                builder.append_data(&mut h, &s.archive_path, &mut reader)?;
                let (count, sha) = reader.finish();
                if count != size || Some(&sha) != entry.sha256.as_ref() {
                    anyhow::bail!("{} changed during the backup", s.disk.display());
                }
            }
            EntryKind::Dir => {
                let mut h = header(tar::EntryType::Directory, 0, s.mode, s.mtime);
                builder.append_data(&mut h, &s.archive_path, io::empty())?;
            }
            EntryKind::Symlink => {
                let mut h = header(tar::EntryType::Symlink, 0, 0o777, s.mtime);
                builder.append_link(
                    &mut h,
                    &s.archive_path,
                    s.target.as_deref().unwrap_or_default(),
                )?;
            }
        }
    }
    let file = builder.into_inner()?.finish()?;
    file.sync_all()?;
    Ok(manifest)
}

/// Removes a file when dropped (temporary files of a backup in progress).
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Snapshots the database behind `pool` with `VACUUM INTO` (consistent,
/// safe while the daemon runs) and archives it with the files from
/// `data_dir` into `out`, which must not exist yet.
pub async fn create_backup_file(
    pool: &sqlx::SqlitePool,
    data_dir: &Path,
    out: &Path,
    include_releases: bool,
    aegis_version: &str,
) -> anyhow::Result<Manifest> {
    if out.exists() {
        anyhow::bail!("{} already exists", out.display());
    }
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = out
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} is not a file path", out.display()))?
        .to_string_lossy()
        .to_string();
    // The snapshot holds password hashes: create it private and empty
    // (VACUUM INTO accepts an empty file) instead of letting SQLite create it.
    let snapshot = TempFile(parent.join(format!(".{}.db.partial", file_name)));
    let _ = fs::remove_file(&snapshot.0);
    create_private(&snapshot.0)?;
    let snapshot_path = snapshot
        .0
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Non-UTF-8 path {}", snapshot.0.display()))?
        .to_string();
    sqlx::query("VACUUM INTO ?")
        .bind(&snapshot_path)
        .execute(pool)
        .await?;
    let schema_version = aegis_event_store::EventStore::schema_version(pool).await?;

    let manifest = Manifest {
        format: FORMAT,
        aegis_version: aegis_version.to_string(),
        created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        schema_version,
        includes_releases: include_releases,
        contents: Vec::new(),
    };
    let data_dir = data_dir.to_path_buf();
    let out = out.to_path_buf();
    let partial = TempFile(parent.join(format!(".{}.partial", file_name)));
    let _ = fs::remove_file(&partial.0);
    tokio::task::spawn_blocking(move || -> anyhow::Result<Manifest> {
        let sources = collect_sources(&data_dir, &snapshot.0, include_releases)?;
        let manifest = write_archive(&partial.0, manifest, &sources)?;
        if out.exists() {
            anyhow::bail!("{} already exists", out.display());
        }
        fs::rename(&partial.0, &out)?;
        drop(snapshot);
        Ok(manifest)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Backup task failed: {}", e))?
}

// ----------------------------------------------------------------------
// Reading and verifying archives
// ----------------------------------------------------------------------

/// Checks that an archive path is relative, has no `.`/`..` parts and is
/// one of the paths a backup contains.
fn check_entry_path(path: &str) -> anyhow::Result<()> {
    let parts: Vec<&str> = path.split('/').collect();
    let well_formed = !path.is_empty()
        && !path.contains('\\')
        && !path.contains('\0')
        && parts
            .iter()
            .all(|p| !p.is_empty() && *p != "." && *p != "..")
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
    let allowed = matches!(
        parts.as_slice(),
        [DB_ENTRY]
            | [KEY_ENTRY]
            | [CERTS_DIR, ..]
            | [ARTIFACTS_DIR, ..]
            | [PROJECTS_DIR, _, "current.json"]
            | [PROJECTS_DIR, _, "releases", ..]
    );
    if !well_formed || !allowed {
        anyhow::bail!("Refusing archive entry '{}'", path);
    }
    Ok(())
}

/// Symlinks may only point inside their own release directory.
fn check_symlink(path: &str, target: &str) -> anyhow::Result<()> {
    let parts: Vec<&str> = path.split('/').collect();
    // projects/<id>/releases/<version>/... (the link itself is below <version>)
    let inside_release = parts.len() > 4 && parts[0] == PROJECTS_DIR && parts[2] == "releases";
    let relative = !target.is_empty()
        && !target.starts_with('/')
        && !target.contains('\\')
        && !target.contains('\0')
        && !Path::new(target)
            .components()
            .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)));
    if inside_release && relative {
        let mut resolved: Vec<&str> = parts[..parts.len() - 1].to_vec();
        let mut escaped = false;
        for part in target.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if resolved.len() <= 4 {
                        escaped = true;
                        break;
                    }
                    resolved.pop();
                }
                other => resolved.push(other),
            }
        }
        if !escaped {
            return Ok(());
        }
    }
    anyhow::bail!(
        "Refusing symlink '{}' -> '{}': links must stay inside their release",
        path,
        target
    )
}

fn entry_path(entry: &tar::Entry<impl Read>) -> anyhow::Result<String> {
    let path = entry.path()?;
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Non-UTF-8 path in archive"))?;
    Ok(text.trim_end_matches('/').to_string())
}

fn parse_manifest(entry: &mut tar::Entry<impl Read>) -> anyhow::Result<Manifest> {
    if entry_path(entry)? != MANIFEST {
        anyhow::bail!("Not an Aegis backup: the first entry is not {}", MANIFEST);
    }
    let mut bytes = Vec::new();
    entry.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        anyhow::bail!("The manifest is too large");
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("Invalid {}: {}", MANIFEST, e))?;
    if manifest.format != FORMAT {
        anyhow::bail!(
            "Backup format {} is not supported by this version of Aegis (expected {})",
            manifest.format,
            FORMAT
        );
    }
    Ok(manifest)
}

/// Reads only the manifest (the archive's first entry).
pub fn read_manifest(archive: &Path) -> anyhow::Result<Manifest> {
    let mut ar = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    let mut entries = ar.entries()?;
    let mut first = entries
        .next()
        .ok_or_else(|| anyhow::anyhow!("The archive is empty"))??;
    parse_manifest(&mut first)
}

/// Checks every entry against the manifest without writing anything.
pub fn verify_archive(archive: &Path) -> anyhow::Result<Manifest> {
    process_archive(archive, None)
}

/// Verifies the archive and extracts it into `dest`, which must be an empty
/// directory. On error, `dest` may hold a partial extraction.
pub fn extract_archive(archive: &Path, dest: &Path) -> anyhow::Result<Manifest> {
    process_archive(archive, Some(dest))
}

/// Creates the missing directories between `root` and `path`'s parent,
/// refusing to go through anything that isn't a real directory (a symlink
/// extracted earlier must never redirect a write).
fn prepare_parent(root: &Path, path: &str) -> anyhow::Result<PathBuf> {
    let mut current = root.to_path_buf();
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => anyhow::bail!(
                "Refusing archive entry '{}': goes through a non-directory",
                path
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(current.join(parts[parts.len() - 1]))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn process_archive(archive: &Path, dest: Option<&Path>) -> anyhow::Result<Manifest> {
    let mut ar = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    let mut entries = ar.entries()?;
    let mut first = entries
        .next()
        .ok_or_else(|| anyhow::anyhow!("The archive is empty"))??;
    let manifest = parse_manifest(&mut first)?;
    let mut expected: HashMap<&str, &ManifestEntry> = HashMap::new();
    for entry in &manifest.contents {
        check_entry_path(&entry.path)?;
        if expected.insert(entry.path.as_str(), entry).is_some() {
            anyhow::bail!("The manifest lists '{}' twice", entry.path);
        }
    }
    if !expected.contains_key(DB_ENTRY) {
        anyhow::bail!("The archive has no database");
    }

    for entry in entries {
        let mut entry = entry?;
        let path = entry_path(&entry)?;
        check_entry_path(&path)?;
        let want = expected
            .remove(path.as_str())
            .ok_or_else(|| anyhow::anyhow!("'{}' is not listed in the manifest", path))?;
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().unwrap_or(0o644);
        let target = dest.map(|d| prepare_parent(d, &path)).transpose()?;
        if kind.is_file() {
            if want.kind != EntryKind::File {
                anyhow::bail!("'{}' is a file but the manifest says otherwise", path);
            }
            let mut reader = HashingReader::new(&mut entry);
            match &target {
                Some(target) => {
                    let mut file = create_private(target)?;
                    io::copy(&mut reader, &mut file)?;
                    file.sync_all()?;
                }
                None => {
                    io::copy(&mut reader, &mut io::sink())?;
                }
            }
            let (size, sha) = reader.finish();
            if Some(size) != want.size || Some(&sha) != want.sha256.as_ref() {
                anyhow::bail!("Checksum mismatch for '{}': the archive is damaged", path);
            }
            if let Some(target) = &target {
                let private = path == DB_ENTRY || path == KEY_ENTRY;
                set_mode(target, if private { 0o600 } else { mode })?;
            }
        } else if kind.is_dir() {
            if want.kind != EntryKind::Dir {
                anyhow::bail!("'{}' is a directory but the manifest says otherwise", path);
            }
            if let Some(target) = &target {
                match fs::symlink_metadata(target) {
                    Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                    Ok(_) => anyhow::bail!("Refusing archive entry '{}'", path),
                    Err(_) => fs::create_dir(target)?,
                }
                set_mode(target, mode | 0o700)?;
            }
        } else if kind.is_symlink() {
            let link = entry
                .link_name()?
                .and_then(|l| l.to_str().map(str::to_string))
                .ok_or_else(|| anyhow::anyhow!("Symlink '{}' has no valid target", path))?;
            if want.kind != EntryKind::Symlink || want.target.as_deref() != Some(link.as_str()) {
                anyhow::bail!("Symlink '{}' does not match the manifest", path);
            }
            check_symlink(&path, &link)?;
            if let Some(target) = &target {
                make_symlink(&link, target)?;
            }
        } else {
            anyhow::bail!("Refusing archive entry '{}': unsupported type", path);
        }
    }
    if let Some(missing) = expected.keys().min() {
        anyhow::bail!(
            "The archive is incomplete: '{}' (and {} more) listed in the manifest is missing",
            missing,
            expected.len() - 1
        );
    }
    Ok(manifest)
}

#[cfg(unix)]
fn make_symlink(link: &str, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(not(unix))]
fn make_symlink(_link: &str, at: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("Cannot restore symlink {} on this platform", at.display()),
    ))
}

// ----------------------------------------------------------------------
// Offline restore
// ----------------------------------------------------------------------

/// Holds `<data_dir>/aegis.lock` locked (with the holder's pid in it) so
/// that a restore and a running daemon exclude each other. The lock is
/// released when this is dropped or the process exits.
pub struct DataDirLock {
    _file: File,
}

impl DataDirLock {
    pub fn acquire(data_dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(data_dir)?;
        let path = data_dir.join(LOCK_FILE);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                let pid = fs::read_to_string(&path).unwrap_or_default();
                let pid = pid.trim();
                anyhow::bail!(
                    "{} is locked by a running Aegis daemon{}",
                    data_dir.display(),
                    if pid.is_empty() {
                        String::new()
                    } else {
                        format!(" (pid {})", pid)
                    }
                );
            }
            Err(fs::TryLockError::Error(e)) => {
                return Err(anyhow::anyhow!("Cannot lock {}: {}", path.display(), e))
            }
        }
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(Self { _file: file })
    }
}

/// Where a restore puts things.
#[derive(Debug, Clone)]
pub struct RestoreTarget {
    pub data_dir: PathBuf,
    /// `daemon.database_path` (may be outside the data directory).
    pub database_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RestoreOptions {
    /// Restore an archive made by a different Aegis version.
    pub force: bool,
    /// This build's version.
    pub aegis_version: String,
    /// The newest migration this build knows.
    pub latest_schema_version: i64,
}

#[derive(Debug)]
pub struct RestoreReport {
    pub manifest: Manifest,
    /// The previous database, key and directories.
    pub moved_aside: PathBuf,
    /// What was restored (paths relative to the archive).
    pub restored: Vec<String>,
}

/// Moves a file or directory, copying a file across filesystems.
fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if fs::symlink_metadata(from).is_ok_and(|m| m.is_file()) => {
            tracing::debug!(error = %e, "rename failed; copying instead");
            fs::copy(from, to)?;
            fs::remove_file(from)
        }
        Err(e) => Err(e),
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Restores `archive` into `target`. Call only while the daemon is stopped
/// (hold a [`DataDirLock`]). The archive is verified and extracted into a
/// staging directory first; only then is the current database, key,
/// certificates and (if the archive has them) release directories moved to
/// `<data_dir>/pre-restore-<timestamp>/` and replaced. If a step fails, the
/// moves are undone.
pub fn restore(
    archive: &Path,
    target: &RestoreTarget,
    options: &RestoreOptions,
) -> anyhow::Result<RestoreReport> {
    let manifest = read_manifest(archive)?;
    if manifest.schema_version > options.latest_schema_version {
        anyhow::bail!(
            "This backup's database (schema {}) is newer than this Aegis build supports ({}). \
             Install Aegis {} or newer to restore it.",
            manifest.schema_version,
            options.latest_schema_version,
            manifest.aegis_version
        );
    }
    if manifest.aegis_version != options.aegis_version && !options.force {
        anyhow::bail!(
            "This backup was made by Aegis {} and this is {}. Pass --force to restore it anyway.",
            manifest.aegis_version,
            options.aegis_version
        );
    }

    let data_dir = &target.data_dir;
    fs::create_dir_all(data_dir)?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    let staging = data_dir.join(format!(".restore-staging-{}", stamp));
    create_private_dir(&staging)?;
    let manifest = match extract_archive(archive, &staging) {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(e.context("The backup failed verification; nothing was changed"));
        }
    };

    // (staged, live, name in the aside directory)
    let mut plan: Vec<(Option<PathBuf>, PathBuf, PathBuf)> = vec![(
        Some(staging.join(DB_ENTRY)),
        target.database_path.clone(),
        PathBuf::from(DB_ENTRY),
    )];
    for suffix in ["-wal", "-shm", "-journal"] {
        plan.push((
            None,
            sidecar(&target.database_path, suffix),
            PathBuf::from(format!("{}{}", DB_ENTRY, suffix)),
        ));
    }
    if manifest.has(KEY_ENTRY) {
        plan.push((
            Some(staging.join(KEY_ENTRY)),
            data_dir.join(KEY_ENTRY),
            PathBuf::from(KEY_ENTRY),
        ));
    }
    if manifest.has(CERTS_DIR) {
        plan.push((
            Some(staging.join(CERTS_DIR)),
            data_dir.join(CERTS_DIR),
            PathBuf::from(CERTS_DIR),
        ));
    }
    if manifest.includes_releases && manifest.has(ARTIFACTS_DIR) {
        plan.push((
            Some(staging.join(ARTIFACTS_DIR)),
            data_dir.join(ARTIFACTS_DIR),
            PathBuf::from(ARTIFACTS_DIR),
        ));
    }
    if manifest.includes_releases {
        let staged_projects = staging.join(PROJECTS_DIR);
        if staged_projects.is_dir() {
            let mut ids: Vec<_> = fs::read_dir(&staged_projects)?
                .map(|e| e.map(|e| e.file_name()))
                .collect::<Result<_, _>>()?;
            ids.sort();
            for id in ids {
                for part in ["current.json", "releases"] {
                    let rel = Path::new(PROJECTS_DIR).join(&id).join(part);
                    let staged = staging.join(&rel);
                    if exists(&staged) {
                        plan.push((Some(staged), data_dir.join(&rel), rel));
                    }
                }
            }
        }
    }

    let aside = data_dir.join(format!("pre-restore-{}", stamp));
    create_private_dir(&aside)?;
    // Each completed move, to undo on failure.
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut restored = Vec::new();
    let mut result = Ok(());
    for (staged, live, aside_name) in &plan {
        if exists(live) {
            let to = aside.join(aside_name);
            if let Err(e) = move_path(live, &to) {
                result = Err(anyhow::anyhow!("Moving {} aside: {}", live.display(), e));
                break;
            }
            done.push((live.clone(), to));
        }
        if let Some(staged) = staged {
            if let Err(e) = move_path(staged, live) {
                result = Err(anyhow::anyhow!("Restoring {}: {}", live.display(), e));
                break;
            }
            done.push((staged.clone(), live.clone()));
            restored.push(aside_name.to_string_lossy().replace('\\', "/"));
        }
    }
    if let Err(e) = result {
        let mut undo_failed = false;
        for (from, to) in done.iter().rev() {
            if let Err(undo) = move_path(to, from) {
                tracing::error!(from = %to.display(), to = %from.display(), error = %undo, "Undo failed");
                undo_failed = true;
            }
        }
        let _ = fs::remove_dir_all(&staging);
        if !undo_failed {
            let _ = fs::remove_dir(&aside);
        }
        return Err(e.context(if undo_failed {
            format!(
                "Restore failed and could not be fully undone; the previous files are in {}",
                aside.display()
            )
        } else {
            "Restore failed; the previous state was put back".to_string()
        }));
    }
    let _ = fs::remove_dir_all(&staging);
    set_mode(&target.database_path, 0o600)?;
    if manifest.has(KEY_ENTRY) {
        set_mode(&data_dir.join(KEY_ENTRY), 0o600)?;
    }
    #[cfg(unix)]
    match_owner(data_dir, &plan);
    if fs::read_dir(&aside).is_ok_and(|mut d| d.next().is_none()) {
        let _ = fs::remove_dir(&aside);
    }
    Ok(RestoreReport {
        manifest,
        moved_aside: aside,
        restored,
    })
}

/// When run as root (e.g. `sudo aegis-daemon admin restore`), hands the
/// restored files to the data directory's owner, the daemon's user.
#[cfg(unix)]
fn match_owner(data_dir: &Path, plan: &[(Option<PathBuf>, PathBuf, PathBuf)]) {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let Ok(meta) = fs::metadata(data_dir) else {
        return;
    };
    let (uid, gid) = (meta.uid(), meta.gid());
    fn chown_tree(path: &Path, uid: u32, gid: u32) {
        let _ = std::os::unix::fs::lchown(path, Some(uid), Some(gid));
        if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.flatten() {
                    chown_tree(&entry.path(), uid, gid);
                }
            }
        }
    }
    for (staged, live, _) in plan {
        if staged.is_some() {
            chown_tree(live, uid, gid);
        }
    }
}

// ----------------------------------------------------------------------
// Schedule
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupSchedule {
    /// Every so often, counted from the newest backup.
    Every(Duration),
    /// Once a day at this UTC time (hour, minute).
    DailyAt(u32, u32),
}

/// When the next scheduled backup is due, given the newest existing one.
/// A missed run (the daemon was down) is due immediately.
pub fn next_backup_at(
    schedule: BackupSchedule,
    now: DateTime<Utc>,
    last: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    match schedule {
        BackupSchedule::Every(every) => {
            let every = ChronoDuration::from_std(every).unwrap_or(ChronoDuration::hours(24));
            match last {
                Some(last) if last + every > now => last + every,
                _ => now,
            }
        }
        BackupSchedule::DailyAt(hour, minute) => {
            let time = NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or_default();
            let today = now.date_naive().and_time(time).and_utc();
            let slot = if today <= now {
                today
            } else {
                today - ChronoDuration::days(1)
            };
            // `slot` is the most recent scheduled time; run now if it was missed.
            if last.is_none_or(|last| last < slot) {
                now
            } else {
                slot + ChronoDuration::days(1)
            }
        }
    }
}

// ----------------------------------------------------------------------
// Control plane
// ----------------------------------------------------------------------

impl ControlPlane {
    pub fn backups_dir(&self) -> PathBuf {
        backups_dir(&self.inner.settings.data_dir)
    }

    pub fn backup_settings(&self) -> &BackupSettings {
        &self.inner.settings.backup
    }

    /// Backups, newest first.
    pub fn list_backups(&self) -> ControlResult<Vec<BackupInfo>> {
        Ok(list_backups(&self.backups_dir()).map_err(anyhow::Error::from)?)
    }

    /// The path of an existing backup.
    pub fn backup_file(&self, name: &str) -> ControlResult<PathBuf> {
        validate_backup_name(name).map_err(ControlError::InvalidArgument)?;
        let path = self.backups_dir().join(name);
        if !path.is_file() {
            return Err(ControlError::NotFound(format!("No backup named {}", name)));
        }
        Ok(path)
    }

    /// Creates a backup now and applies retention. `trigger` says why
    /// ("manual", "scheduled") and is recorded in the event.
    pub async fn create_backup(
        &self,
        include_releases: Option<bool>,
        trigger: &str,
    ) -> ControlResult<BackupInfo> {
        let settings = &self.inner.settings.backup;
        let include_releases = include_releases.unwrap_or(settings.include_releases);
        // One at a time; a second request waits for the first.
        let _guard = self.inner.backup_lock.lock().await;
        let dir = self.backups_dir();
        let created = async {
            create_private_dir(&dir)?;
            let out = dir.join(new_backup_name(Utc::now()));
            create_backup_file(
                self.inner.store.pool(),
                &self.inner.settings.data_dir,
                &out,
                include_releases,
                &settings.aegis_version,
            )
            .await?;
            Ok::<_, anyhow::Error>(backup_info(&out)?)
        }
        .await;
        let info = match created {
            Ok(info) => info,
            Err(e) => {
                tracing::error!(error = format!("{:#}", e), "Backup failed");
                self.record(
                    "BackupFailed",
                    json!({"trigger": trigger, "include_releases": include_releases,
                           "error": format!("{:#}", e)}),
                )
                .await?;
                return Err(ControlError::Internal(e));
            }
        };
        tracing::info!(name = %info.name, size = info.size_bytes, "Backup created");
        self.record(
            "BackupCreated",
            json!({"name": info.name, "size_bytes": info.size_bytes,
                   "includes_releases": info.includes_releases,
                   "file_count": info.file_count, "trigger": trigger}),
        )
        .await?;
        match apply_retention(&dir, settings.keep) {
            Ok(removed) => {
                for name in removed {
                    self.record(
                        "BackupDeleted",
                        json!({"name": name, "reason": "retention"}),
                    )
                    .await?;
                }
            }
            Err(e) => tracing::warn!(error = %e, "Failed to delete old backups"),
        }
        Ok(info)
    }

    pub async fn delete_backup(&self, name: &str) -> ControlResult<()> {
        let path = self.backup_file(name)?;
        let _guard = self.inner.backup_lock.lock().await;
        fs::remove_file(&path).map_err(anyhow::Error::from)?;
        self.record("BackupDeleted", json!({"name": name, "reason": "manual"}))
            .await?;
        Ok(())
    }

    /// Runs scheduled backups until the process exits.
    pub fn spawn_backup_scheduler(&self, schedule: BackupSchedule) {
        let control = self.clone();
        tokio::spawn(async move {
            // Let startup (recovering apps) finish before a catch-up backup.
            tokio::time::sleep(Duration::from_secs(60)).await;
            loop {
                let last = control
                    .list_backups()
                    .ok()
                    .and_then(|list| {
                        list.into_iter()
                            .find_map(|b| DateTime::parse_from_rfc3339(&b.created_at).ok())
                    })
                    .map(|t| t.with_timezone(&Utc));
                let now = Utc::now();
                let due = next_backup_at(schedule, now, last);
                if due > now {
                    let wait = (due - now).to_std().unwrap_or(Duration::from_secs(60));
                    // Re-check at least hourly (clock changes, manual backups).
                    tokio::time::sleep(wait.min(Duration::from_secs(3600))).await;
                    continue;
                }
                if control.create_backup(None, "scheduled").await.is_err() {
                    // Recorded as BackupFailed; don't retry in a tight loop.
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use tempfile::TempDir;

    async fn file_pool(path: &Path) -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();
        aegis_event_store::EventStore::initialize_db(&pool)
            .await
            .unwrap();
        pool
    }

    /// A data directory with a key, certificates and one release.
    fn populate(data: &Path) {
        fs::write(data.join(KEY_ENTRY), b"0123456789abcdef0123456789abcdef").unwrap();
        fs::create_dir_all(data.join("certs/example.com")).unwrap();
        fs::write(data.join("certs/example.com/cert.pem"), b"CERT").unwrap();
        let release = data.join("projects/p1/releases/v1");
        fs::create_dir_all(release.join("bin")).unwrap();
        fs::create_dir_all(release.join("empty")).unwrap();
        fs::write(release.join("bin/app.sh"), b"echo hi\n").unwrap();
        fs::write(
            data.join("projects/p1/current.json"),
            b"{\"active_version\":\"v1\"}",
        )
        .unwrap();
        fs::create_dir_all(data.join("artifacts/releases/r1")).unwrap();
        fs::write(data.join("artifacts/releases/r1/metadata.json"), b"{}").unwrap();
        // Logs are not part of backups.
        fs::create_dir_all(data.join("projects/p1/logs")).unwrap();
        fs::write(data.join("projects/p1/logs/x.log"), b"log").unwrap();
    }

    async fn event_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM events")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn archive_round_trip() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path().join("data");
        fs::create_dir_all(&data).unwrap();
        populate(&data);
        #[cfg(unix)]
        std::os::unix::fs::symlink("bin/app.sh", data.join("projects/p1/releases/v1/start"))
            .unwrap();
        let pool = file_pool(&tmp.path().join("aegis.db")).await;
        let store = aegis_event_store::EventStore::new(pool.clone());
        store
            .append_event("ProjectCreated", json!({"name": "a"}))
            .await
            .unwrap();

        let out = backups_dir(&data).join(new_backup_name(Utc::now()));
        let manifest = create_backup_file(&pool, &data, &out, true, "9.9.9")
            .await
            .unwrap();
        assert!(manifest.includes_releases);
        assert_eq!(manifest.aegis_version, "9.9.9");
        assert_eq!(
            manifest.schema_version,
            aegis_event_store::EventStore::latest_schema_version()
        );
        let paths: Vec<&str> = manifest.contents.iter().map(|e| e.path.as_str()).collect();
        for want in [
            DB_ENTRY,
            KEY_ENTRY,
            "certs/example.com/cert.pem",
            "projects/p1/current.json",
            "projects/p1/releases/v1/bin/app.sh",
            "projects/p1/releases/v1/empty",
            "artifacts/releases/r1/metadata.json",
        ] {
            assert!(paths.contains(&want), "{want} missing from {paths:?}");
        }
        assert!(!paths.iter().any(|p| p.contains("logs")));
        assert_eq!(read_manifest(&out).unwrap(), manifest);
        assert_eq!(verify_archive(&out).unwrap(), manifest);
        // Temporary files are gone; the archive is private.
        let names: Vec<_> = fs::read_dir(backups_dir(&data))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&out).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let dest = tmp.path().join("extracted");
        fs::create_dir(&dest).unwrap();
        extract_archive(&out, &dest).unwrap();
        assert_eq!(
            fs::read(dest.join("projects/p1/releases/v1/bin/app.sh")).unwrap(),
            b"echo hi\n"
        );
        assert!(dest.join("projects/p1/releases/v1/empty").is_dir());
        assert_eq!(
            fs::read(dest.join(KEY_ENTRY)).unwrap(),
            fs::read(data.join(KEY_ENTRY)).unwrap()
        );
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(dest.join("projects/p1/releases/v1/start")).unwrap(),
            Path::new("bin/app.sh")
        );
        let restored = file_pool(&dest.join(DB_ENTRY)).await;
        assert_eq!(event_count(&restored).await, 1);

        // Without releases: database, key and certificates only.
        let out2 = backups_dir(&data).join("aegis-backup-db-only.tar.gz");
        let m2 = create_backup_file(&pool, &data, &out2, false, "9.9.9")
            .await
            .unwrap();
        assert!(!m2.includes_releases);
        assert!(m2
            .contents
            .iter()
            .all(|e| !e.path.starts_with("projects") && !e.path.starts_with("artifacts")));
        verify_archive(&out2).unwrap();
    }

    /// Builds an archive by hand. Paths are written raw, so they can be
    /// ones `tar::Builder` would refuse.
    fn craft(
        path: &Path,
        manifest: &Manifest,
        entries: &[(&str, tar::EntryType, &str, Option<&str>)],
    ) {
        let file = File::create(path).unwrap();
        let mut builder = tar::Builder::new(GzEncoder::new(file, Compression::default()));
        let json = serde_json::to_vec(manifest).unwrap();
        let mut h = header(tar::EntryType::Regular, json.len() as u64, 0o600, 0);
        builder
            .append_data(&mut h, MANIFEST, json.as_slice())
            .unwrap();
        for (name, kind, data, link) in entries {
            let mut h = header(*kind, data.len() as u64, 0o644, 0);
            let raw = &mut h.as_old_mut().name;
            raw.fill(0);
            raw[..name.len()].copy_from_slice(name.as_bytes());
            if let Some(link) = link {
                let raw = &mut h.as_old_mut().linkname;
                raw.fill(0);
                raw[..link.len()].copy_from_slice(link.as_bytes());
            }
            h.set_cksum();
            builder.append(&h, data.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    fn file_entry(path: &str, data: &[u8]) -> ManifestEntry {
        let mut hasher = Sha256::new();
        hasher.update(data);
        ManifestEntry {
            path: path.into(),
            kind: EntryKind::File,
            size: Some(data.len() as u64),
            sha256: Some(hex(&hasher.finalize())),
            target: None,
        }
    }

    fn manifest_with(contents: Vec<ManifestEntry>) -> Manifest {
        Manifest {
            format: FORMAT,
            aegis_version: "test".into(),
            created_at: Utc::now().to_rfc3339(),
            schema_version: 1,
            includes_releases: true,
            contents,
        }
    }

    #[test]
    fn verification_rejects_bad_archives() {
        let tmp = TempDir::new().unwrap();
        let archive = tmp.path().join("a.tar.gz");
        let reg = tar::EntryType::Regular;
        let check = |manifest: Manifest,
                     entries: &[(&str, tar::EntryType, &str, Option<&str>)],
                     want: &str| {
            craft(&archive, &manifest, entries);
            let err = format!("{:#}", verify_archive(&archive).unwrap_err());
            assert!(err.contains(want), "expected '{want}' in: {err}");
            // Extraction refuses too, and writes nothing outside `dest`.
            let dest = tmp.path().join("dest");
            let _ = fs::remove_dir_all(&dest);
            fs::create_dir(&dest).unwrap();
            assert!(extract_archive(&archive, &dest).is_err());
            assert!(!tmp.path().join("evil").exists());
        };

        // A valid crafted archive passes.
        craft(
            &archive,
            &manifest_with(vec![file_entry(DB_ENTRY, b"db")]),
            &[(DB_ENTRY, reg, "db", None)],
        );
        verify_archive(&archive).unwrap();

        // Checksum mismatch.
        check(
            manifest_with(vec![file_entry(DB_ENTRY, b"db")]),
            &[(DB_ENTRY, reg, "xx", None)],
            "Checksum mismatch",
        );
        // Path traversal and absolute paths.
        for bad in [
            "../evil",
            "certs/../../evil",
            "/evil",
            "projects/p/releases/../../../evil",
        ] {
            check(
                manifest_with(vec![file_entry(DB_ENTRY, b"db")]),
                &[(DB_ENTRY, reg, "db", None), (bad, reg, "x", None)],
                "Refusing archive entry",
            );
        }
        // Entries outside the backup layout.
        check(
            manifest_with(vec![file_entry(DB_ENTRY, b"db")]),
            &[(DB_ENTRY, reg, "db", None), ("aegis.lock", reg, "x", None)],
            "Refusing archive entry",
        );
        // A file not in the manifest.
        check(
            manifest_with(vec![file_entry(DB_ENTRY, b"db")]),
            &[(DB_ENTRY, reg, "db", None), ("certs/x", reg, "x", None)],
            "not listed in the manifest",
        );
        // A file in the manifest but not in the archive.
        check(
            manifest_with(vec![
                file_entry(DB_ENTRY, b"db"),
                file_entry(KEY_ENTRY, b"k"),
            ]),
            &[(DB_ENTRY, reg, "db", None)],
            "incomplete",
        );
        // Symlinks leaving their release, and hard links.
        let link = |target: &str| ManifestEntry {
            path: "projects/p/releases/v/l".into(),
            kind: EntryKind::Symlink,
            size: None,
            sha256: None,
            target: Some(target.into()),
        };
        for target in ["../../../../evil", "/etc/passwd", "../other-version/x"] {
            check(
                manifest_with(vec![file_entry(DB_ENTRY, b"db"), link(target)]),
                &[
                    (DB_ENTRY, reg, "db", None),
                    (
                        "projects/p/releases/v/l",
                        tar::EntryType::Symlink,
                        "",
                        Some(target),
                    ),
                ],
                "Refusing symlink",
            );
        }
        check(
            manifest_with(vec![file_entry(DB_ENTRY, b"db"), link("x")]),
            &[
                (DB_ENTRY, reg, "db", None),
                (
                    "projects/p/releases/v/l",
                    tar::EntryType::Link,
                    "",
                    Some("x"),
                ),
            ],
            "unsupported type",
        );
        // Not a backup at all.
        craft(&archive, &manifest_with(vec![]), &[]);
        assert!(format!("{:#}", verify_archive(&archive).unwrap_err()).contains("no database"));
        fs::write(&archive, b"not gzip").unwrap();
        assert!(read_manifest(&archive).is_err());
    }

    #[test]
    fn symlink_targets_stay_inside_the_release() {
        let at = "projects/p/releases/v1/node_modules/.bin/tool";
        assert!(check_symlink(at, "../tool/cli.js").is_ok());
        assert!(check_symlink(at, "../../lib/x").is_ok());
        assert!(check_symlink(at, "../../../../v2/x").is_err());
        assert!(check_symlink(at, "/usr/bin/env").is_err());
        assert!(check_symlink("certs/x", "y").is_err());
    }

    #[test]
    fn names_and_retention() {
        let name = new_backup_name(Utc::now());
        assert!(validate_backup_name(&name).is_ok(), "{name}");
        for bad in [
            "",
            "aegis-backup-.tar.gz",
            "../aegis-backup-1.tar.gz",
            "aegis-backup-1/../../x.tar.gz",
            "aegis-backup-1.tar.gz/..",
            "aegis-backup-a b.tar.gz",
            "aegis-backup-1.tar",
            "other.tar.gz",
            "aegis-backup-%2e%2e.tar.gz",
        ] {
            assert!(validate_backup_name(bad).is_err(), "{bad}");
        }

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        assert!(apply_retention(&dir.join("missing"), 3).unwrap().is_empty());
        for i in 1..=5 {
            fs::write(
                dir.join(format!("aegis-backup-2026010{i}T000000000Z.tar.gz")),
                b"x",
            )
            .unwrap();
        }
        fs::write(dir.join("unrelated.txt"), b"x").unwrap();
        fs::write(dir.join(".aegis-backup-x.tar.gz.partial"), b"x").unwrap();
        let removed = apply_retention(dir, 2).unwrap();
        assert_eq!(
            removed,
            vec![
                "aegis-backup-20260101T000000000Z.tar.gz",
                "aegis-backup-20260102T000000000Z.tar.gz",
                "aegis-backup-20260103T000000000Z.tar.gz",
            ]
        );
        let left: Vec<String> = backup_files(dir)
            .unwrap()
            .into_iter()
            .map(|f| f.0)
            .collect();
        assert_eq!(
            left,
            vec![
                "aegis-backup-20260104T000000000Z.tar.gz",
                "aegis-backup-20260105T000000000Z.tar.gz",
            ]
        );
        assert!(dir.join("unrelated.txt").exists());
        // Unreadable archives are still listed (so they can be deleted).
        let list = list_backups(dir).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].name.contains("0105"));
        assert!(list[0].problem.is_some());
    }

    #[test]
    fn schedule() {
        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let now = t("2026-10-16T12:00:00Z");
        let every = BackupSchedule::Every(Duration::from_secs(6 * 3600));
        assert_eq!(next_backup_at(every, now, None), now);
        assert_eq!(
            next_backup_at(every, now, Some(t("2026-10-16T10:00:00Z"))),
            t("2026-10-16T16:00:00Z")
        );
        assert_eq!(
            next_backup_at(every, now, Some(t("2026-10-15T10:00:00Z"))),
            now
        );

        let daily = BackupSchedule::DailyAt(3, 30);
        // Today's run happened: next is tomorrow.
        assert_eq!(
            next_backup_at(daily, now, Some(t("2026-10-16T03:30:05Z"))),
            t("2026-10-17T03:30:00Z")
        );
        // Today's run was missed: now.
        assert_eq!(
            next_backup_at(daily, now, Some(t("2026-10-15T03:30:05Z"))),
            now
        );
        // Before today's slot, with yesterday's done: today's slot.
        let early = t("2026-10-16T01:00:00Z");
        assert_eq!(
            next_backup_at(daily, early, Some(t("2026-10-15T03:30:01Z"))),
            t("2026-10-16T03:30:00Z")
        );
    }

    #[test]
    fn data_dir_lock_is_exclusive() {
        let tmp = TempDir::new().unwrap();
        let lock = DataDirLock::acquire(tmp.path()).unwrap();
        let err = DataDirLock::acquire(tmp.path()).err().unwrap().to_string();
        assert!(err.contains("locked by a running Aegis daemon"), "{err}");
        drop(lock);
        DataDirLock::acquire(tmp.path()).unwrap();
    }

    #[tokio::test]
    async fn restore_round_trip() {
        let tmp = TempDir::new().unwrap();
        let data = tmp.path().join("data");
        fs::create_dir_all(&data).unwrap();
        populate(&data);
        let db = tmp.path().join("db").join("aegis.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let pool = file_pool(&db).await;
        let store = aegis_event_store::EventStore::new(pool.clone());
        store.append_event("Before", json!({})).await.unwrap();
        let archive = tmp.path().join("backup.tar.gz");
        create_backup_file(&pool, &data, &archive, true, "1.0.0")
            .await
            .unwrap();

        // Things change after the backup.
        store.append_event("After", json!({})).await.unwrap();
        pool.close().await;
        fs::write(data.join(KEY_ENTRY), b"new key").unwrap();
        fs::write(data.join("projects/p1/releases/v1/bin/app.sh"), b"changed").unwrap();
        fs::create_dir_all(data.join("projects/p1/releases/v2")).unwrap();
        fs::write(data.join("artifacts/releases/r1/metadata.json"), b"changed").unwrap();

        let target = RestoreTarget {
            data_dir: data.clone(),
            database_path: db.clone(),
        };
        let options = RestoreOptions {
            force: false,
            aegis_version: "2.0.0".into(),
            latest_schema_version: aegis_event_store::EventStore::latest_schema_version(),
        };
        // Another version needs --force.
        let err = restore(&archive, &target, &options)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--force"), "{err}");
        // A newer schema is refused even with --force.
        let newer = RestoreOptions {
            force: true,
            latest_schema_version: 1,
            ..options.clone()
        };
        assert!(restore(&archive, &target, &newer)
            .unwrap_err()
            .to_string()
            .contains("newer"));

        let report = restore(
            &archive,
            &target,
            &RestoreOptions {
                force: true,
                ..options
            },
        )
        .unwrap();
        assert!(report.restored.contains(&DB_ENTRY.to_string()));
        let pool = file_pool(&db).await;
        let types: Vec<String> = sqlx::query_scalar("SELECT event_type FROM events")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(types, vec!["Before"]);
        assert_eq!(
            fs::read(data.join(KEY_ENTRY)).unwrap(),
            b"0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            fs::read(data.join("projects/p1/releases/v1/bin/app.sh")).unwrap(),
            b"echo hi\n"
        );
        assert!(!data.join("projects/p1/releases/v2").exists());
        // Logs were left alone; the previous state was moved aside.
        assert!(data.join("projects/p1/logs/x.log").exists());
        let aside = &report.moved_aside;
        assert_eq!(fs::read(aside.join(KEY_ENTRY)).unwrap(), b"new key");
        assert!(aside.join(DB_ENTRY).exists());
        assert!(aside.join("projects/p1/releases/v2").is_dir());
        assert_eq!(
            fs::read(data.join("artifacts/releases/r1/metadata.json")).unwrap(),
            b"{}"
        );
        // No staging directory is left behind.
        let leftovers: Vec<_> = fs::read_dir(&data)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".restore-staging"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // A damaged archive changes nothing.
        pool.close().await;
        let bytes = fs::read(&archive).unwrap();
        fs::write(&archive, &bytes[..bytes.len() / 2]).unwrap();
        let before = fs::read(&db).unwrap();
        let err = restore(
            &archive,
            &target,
            &RestoreOptions {
                force: true,
                aegis_version: "1.0.0".into(),
                latest_schema_version: i64::MAX,
            },
        )
        .unwrap_err();
        assert!(
            format!("{:#}", err).contains("nothing was changed"),
            "{err:#}"
        );
        assert_eq!(fs::read(&db).unwrap(), before);
    }
}
