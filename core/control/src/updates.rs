//! "New changes available": how far each app's live release is behind its
//! source.
//!
//! - **Repository apps**: the branch is fetched into a per-repository cache
//!   (`<data_dir>/cache/repos/<hash>.git`, a bare, blob-less clone: commits
//!   and trees only) with the app's deploy key (or anonymously), and the
//!   live release's commit is compared with the branch head: how many
//!   commits behind, which ones, and which files changed (filtered by the
//!   app's watch paths when it is in a group).
//! - **Folder apps**: the folder's git HEAD is read from `.git` directly,
//!   and compared with the live release's commit; uncommitted changes are
//!   counted with `git status` (read-only).
//!
//! Checks run every few minutes (`[apps] update_check_minutes`), on demand
//! ("Check now") and after a webhook delivery. Remote access is rate limited
//! per repository and timed out; a failure is reported in the result and
//! never blocks anything else.

use crate::git::{is_sha, path_matches, touches, Git};
use crate::scan::normalize_repo;
use crate::{ControlError, ControlPlane, ControlResult};
use aegis_projection::{GroupState, ProjectState};
use aegis_types::ProjectId;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// A repository is fetched at most this often (a "Check now" within it
/// reuses the last fetch).
const MIN_FETCH_INTERVAL: Duration = Duration::from_secs(3);
const MAX_COMMITS: usize = 50;
const MAX_FILES: usize = 500;

/// One commit the live release doesn't have.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommitSummary {
    pub sha: String,
    pub author: String,
    /// ISO 8601.
    pub date: String,
    pub message: String,
}

/// How an app's live release compares with its source.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateStatus {
    pub checked_at: String,
    /// "git" or "folder".
    pub kind: &'static str,
    pub branch: Option<String>,
    /// The branch head (git), or the folder's HEAD.
    pub head: Option<String>,
    /// The commit of the live release, when it has one.
    pub live_commit: Option<String>,
    /// The app has a live release.
    pub deployed: bool,
    /// Commits on the branch (or in the folder) after the live commit.
    pub behind: Option<u32>,
    /// Newest first, at most 50.
    pub commits: Vec<CommitSummary>,
    /// Files changed since the live commit; only those matching the app's
    /// watch paths when it has some (at most 500).
    pub changed_files: Vec<String>,
    /// Some of the changes concern this app (its watch paths match, or it
    /// has none), or it was never deployed.
    pub relevant: bool,
    /// Folder apps: files with uncommitted changes (`None`: unknown).
    pub dirty: Option<u32>,
    pub up_to_date: bool,
    pub error: Option<String>,
}

impl UpdateStatus {
    fn new(kind: &'static str, project: &ProjectState, live: Option<String>) -> Self {
        Self {
            checked_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            kind,
            branch: (kind == "git").then(|| project.branch.clone()),
            head: None,
            deployed: project.current_release.is_some(),
            live_commit: live,
            behind: None,
            commits: Vec::new(),
            changed_files: Vec::new(),
            relevant: false,
            dirty: None,
            up_to_date: false,
            error: None,
        }
    }

    fn failed(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }
}

/// A group's apps compared with the group's branch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupUpdates {
    pub checked_at: String,
    pub head: Option<String>,
    /// Apps whose watch paths match a change (or that were never deployed).
    pub changed_apps: usize,
    pub members: Vec<(ProjectId, UpdateStatus)>,
    pub error: Option<String>,
}

/// The fetch state of one cached repository.
#[derive(Default)]
pub(crate) struct RepoCache {
    lock: Arc<tokio::sync::Mutex<Option<Instant>>>,
}

/// `<data_dir>/cache/repos/<hash>.git` for a repository URL.
fn cache_dir(data_dir: &Path, url: &str) -> PathBuf {
    let hash = Sha256::digest(normalize_repo(url).as_bytes());
    let name: String = hash.iter().take(10).map(|b| format!("{:02x}", b)).collect();
    data_dir
        .join("cache")
        .join("repos")
        .join(format!("{}.git", name))
}

impl ControlPlane {
    /// The last update check of an app, if one has run.
    pub fn cached_updates(&self, project_id: &ProjectId) -> Option<UpdateStatus> {
        self.inner.updates.lock().unwrap().get(project_id).cloned()
    }

    fn watch_paths_of(&self, project_id: &ProjectId) -> Vec<String> {
        self.group_of(project_id)
            .and_then(|g| {
                g.members
                    .into_iter()
                    .find(|m| &m.project_id == project_id)
                    .map(|m| m.watch_paths)
            })
            .unwrap_or_default()
    }

    /// Fetches `branch` of `url` into the repository cache (at most every
    /// few seconds per repository) and returns the cache directory.
    async fn fetch_cached(
        &self,
        url: &str,
        branch: &str,
        ssh: Option<&aegis_ssh::SshAuth>,
    ) -> Result<PathBuf, String> {
        let dir = cache_dir(&self.inner.settings.data_dir, url);
        let lock = self
            .inner
            .repo_caches
            .lock()
            .unwrap()
            .entry(dir.clone())
            .or_default()
            .lock
            .clone();
        let mut last = lock.lock().await;
        let git = Git::new(ssh)?;
        let fresh = last.is_some_and(|t| t.elapsed() < MIN_FETCH_INTERVAL);
        if fresh
            && git
                .run(
                    &dir,
                    &["rev-parse", "--verify", &format!("refs/heads/{}", branch)],
                    GIT_TIMEOUT,
                )
                .await
                .is_ok()
        {
            return Ok(dir);
        }
        let parent = dir.parent().unwrap_or(Path::new("."));
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Cannot create {}: {}", parent.display(), e))?;
        if !dir.join("HEAD").is_file() {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            let text = dir.to_string_lossy().to_string();
            git.run(
                parent,
                &[
                    "clone",
                    "--bare",
                    "--filter=blob:none",
                    "--no-tags",
                    "--",
                    url,
                    &text,
                ],
                FETCH_TIMEOUT,
            )
            .await
            .map_err(|e| {
                let _ = std::fs::remove_dir_all(&dir);
                format!("Cannot read {}: {}", aegis_builder::redact_url(url), e)
            })?;
        } else {
            // The cache follows the app's current URL (a deploy key may have
            // turned an HTTPS GitHub URL into SSH).
            let _ = git
                .run(&dir, &["remote", "set-url", "origin", url], GIT_TIMEOUT)
                .await;
        }
        git.run(
            &dir,
            &[
                "fetch",
                "--filter=blob:none",
                "--no-tags",
                "--prune",
                "origin",
                &format!("+refs/heads/{0}:refs/heads/{0}", branch),
            ],
            FETCH_TIMEOUT,
        )
        .await
        .map_err(|e| format!("Fetching {} failed: {}", branch, e))?;
        *last = Some(Instant::now());
        Ok(dir)
    }

    /// Compares `live` with `head` in a repository: commits, changed files.
    async fn compare(
        &self,
        git: &Git,
        repo: &Path,
        live: &str,
        head: &str,
        watch: &[String],
        mut status: UpdateStatus,
    ) -> UpdateStatus {
        status.head = Some(head.to_string());
        if live == head {
            status.up_to_date = true;
            status.behind = Some(0);
            return status;
        }
        if git
            .run(
                repo,
                &["cat-file", "-e", &format!("{}^{{commit}}", live)],
                GIT_TIMEOUT,
            )
            .await
            .is_err()
        {
            status.relevant = true;
            return status.failed(format!(
                "The live commit {} is not in the branch's history (was the branch rewritten?)",
                &live[..12]
            ));
        }
        let range = format!("{}..{}", live, head);
        if let Ok(count) = git
            .run(repo, &["rev-list", "--count", &range], GIT_TIMEOUT)
            .await
        {
            status.behind = count.trim().parse().ok();
        }
        if let Ok(log) = git
            .run(
                repo,
                &[
                    "log",
                    &format!("--max-count={}", MAX_COMMITS),
                    "--format=%H%x1f%an%x1f%aI%x1f%s",
                    &range,
                ],
                GIT_TIMEOUT,
            )
            .await
        {
            status.commits = log
                .lines()
                .filter_map(|l| {
                    let mut parts = l.splitn(4, '\u{1f}');
                    Some(CommitSummary {
                        sha: parts.next()?.to_string(),
                        author: parts.next()?.to_string(),
                        date: parts.next()?.to_string(),
                        message: parts.next().unwrap_or("").to_string(),
                    })
                })
                .collect();
        }
        let files: Vec<String> = match git
            .run(
                repo,
                &["diff", "--name-only", "--no-renames", live, head],
                GIT_TIMEOUT,
            )
            .await
        {
            Ok(out) => out
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            Err(e) => return status.failed(format!("Cannot list changed files: {}", e)),
        };
        status.relevant = touches(watch, &files);
        status.changed_files = files
            .into_iter()
            .filter(|f| watch.is_empty() || watch.iter().any(|p| path_matches(p, f)))
            .take(MAX_FILES)
            .collect();
        status.up_to_date = status.behind == Some(0);
        status
    }

    /// Checks one repository app against its branch, using `repo` (an
    /// already fetched cache) when given.
    async fn check_git(&self, project: &ProjectState, cache: Option<&Path>) -> UpdateStatus {
        let live = self.live_commit(project);
        let status = UpdateStatus::new("git", project, live.clone());
        let ssh = self.ssh_auth(&project.id);
        let url = self.clone_url_of(project, ssh.is_some());
        let repo = match cache {
            Some(dir) => dir.to_path_buf(),
            None => match self.fetch_cached(&url, &project.branch, ssh.as_ref()).await {
                Ok(dir) => dir,
                Err(e) => return status.failed(e),
            },
        };
        let git = match Git::new(None) {
            Ok(g) => g,
            Err(e) => return status.failed(e),
        };
        let head = match git
            .run(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("refs/heads/{}", project.branch),
                ],
                GIT_TIMEOUT,
            )
            .await
        {
            Ok(h) => h.trim().to_string(),
            Err(_) => {
                return status.failed(format!("The repository has no branch '{}'", project.branch))
            }
        };
        let Some(live) = live else {
            let mut status = status;
            status.head = Some(head);
            // Never deployed (or built before commits were recorded).
            status.relevant = true;
            return status;
        };
        let watch = self.watch_paths_of(&project.id);
        self.compare(&git, &repo, &live, &head, &watch, status)
            .await
    }

    /// Checks one server-folder app against the folder's git HEAD.
    async fn check_folder(&self, project: &ProjectState) -> UpdateStatus {
        let live = self.live_commit(project);
        let mut status = UpdateStatus::new("folder", project, live.clone());
        let Some(dir) = project.source_dir.as_ref().map(PathBuf::from) else {
            return status.failed("The app has no source folder".into());
        };
        let Some(head) = read_folder_head(&dir) else {
            status.error = Some(format!(
                "{} is not in a git repository, so changes can't be listed",
                dir.display()
            ));
            status.relevant = !status.deployed;
            return status;
        };
        let git = match Git::new(None) {
            Ok(g) => g,
            Err(e) => return status.failed(e),
        };
        if let Ok(out) = git
            .run(
                &dir,
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=normal",
                    "--",
                    ".",
                ],
                GIT_TIMEOUT,
            )
            .await
        {
            status.dirty = Some(out.lines().filter(|l| !l.is_empty()).count() as u32);
        }
        let Some(live) = live else {
            status.head = Some(head);
            status.relevant = true;
            return status;
        };
        let mut status = self.compare(&git, &dir, &live, &head, &[], status).await;
        // Files outside the app's folder don't concern it.
        if let Some(prefix) = git
            .run(&dir, &["rev-parse", "--show-prefix"], GIT_TIMEOUT)
            .await
            .ok()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
        {
            status.changed_files.retain(|f| f.starts_with(&prefix));
            status.relevant = !status.changed_files.is_empty();
        }
        if status.dirty.unwrap_or(0) > 0 {
            status.up_to_date = false;
        }
        status
    }

    /// The URL an app's repository is read from (SSH with a deploy key).
    pub(crate) fn clone_url_of(&self, project: &ProjectState, has_key: bool) -> String {
        match crate::sources::with_deploy_key_url(
            aegis_builder::SourceSpec::Git {
                url: project.repository_url.clone(),
                reference: project.branch.clone(),
            },
            has_key,
        ) {
            aegis_builder::SourceSpec::Git { url, .. } => url,
            aegis_builder::SourceSpec::LocalDir(_) => project.repository_url.clone(),
        }
    }

    async fn check_project(&self, project: &ProjectState) -> UpdateStatus {
        let status = if project.source_dir.is_some() {
            self.check_folder(project).await
        } else if !project.repository_url.is_empty() {
            self.check_git(project, None).await
        } else {
            UpdateStatus::new("folder", project, None)
                .failed("The app has no source configured".into())
        };
        self.inner
            .updates
            .lock()
            .unwrap()
            .insert(project.id, status.clone());
        status
    }

    /// Checks an app for new commits now (see the module docs).
    pub async fn check_updates(&self, project_id: &ProjectId) -> ControlResult<UpdateStatus> {
        let project = self
            .inner
            .projection
            .get_project(project_id)
            .ok_or_else(|| ControlError::NotFound(format!("Unknown project {}", project_id)))?;
        Ok(self.check_project(&project).await)
    }

    /// The app's last check, or a new one when there is none yet.
    pub async fn updates(&self, project_id: &ProjectId) -> ControlResult<UpdateStatus> {
        match self.cached_updates(project_id) {
            Some(status) => Ok(status),
            None => self.check_updates(project_id).await,
        }
    }

    fn group_updates_from_cache(&self, group: &GroupState) -> Option<GroupUpdates> {
        let cache = self.inner.updates.lock().unwrap();
        let members: Vec<(ProjectId, UpdateStatus)> = group
            .members
            .iter()
            .map(|m| Some((m.project_id, cache.get(&m.project_id)?.clone())))
            .collect::<Option<_>>()?;
        Some(summarize(members, None))
    }

    /// Checks every app of a group against the group's branch (one fetch).
    pub async fn check_group_updates(&self, group_id: &str) -> ControlResult<GroupUpdates> {
        let group = self.resolve_group(group_id)?;
        let ssh = self.group_ssh(&group);
        let url = self.group_clone_url(&group, ssh.is_some());
        let fetched = self.fetch_cached(&url, &group.branch, ssh.as_ref()).await;
        let mut members = Vec::new();
        for member in &group.members {
            let Some(project) = self.inner.projection.get_project(&member.project_id) else {
                continue;
            };
            let status = match &fetched {
                Ok(dir) => self.check_git(&project, Some(dir)).await,
                Err(e) => {
                    UpdateStatus::new("git", &project, self.live_commit(&project)).failed(e.clone())
                }
            };
            self.inner
                .updates
                .lock()
                .unwrap()
                .insert(project.id, status.clone());
            members.push((project.id, status));
        }
        Ok(summarize(members, fetched.err()))
    }

    /// The group's last check, or a new one when an app has none yet.
    pub async fn group_updates(&self, group_id: &str) -> ControlResult<GroupUpdates> {
        let group = self.resolve_group(group_id)?;
        match self.group_updates_from_cache(&group) {
            Some(updates) => Ok(updates),
            None => self.check_group_updates(&group.id).await,
        }
    }

    /// Starts a check of an app in the background (e.g. after a push).
    pub fn check_updates_soon(&self, project_id: ProjectId) {
        let this = self.clone();
        tokio::spawn(async move {
            let _ = this.check_updates(&project_id).await;
        });
    }

    /// Starts a check of a group in the background (e.g. after a push).
    pub fn check_group_updates_soon(&self, group_id: &str) {
        let this = self.clone();
        let id = group_id.to_string();
        tokio::spawn(async move {
            let _ = this.check_group_updates(&id).await;
        });
    }

    /// Checks every app now: groups once per group, other apps one by one
    /// (each repository is fetched once thanks to the cache's rate limit).
    pub async fn check_all_updates(&self) {
        let mut done = std::collections::HashSet::new();
        for group in self.list_groups() {
            if let Ok(updates) = self.check_group_updates(&group.id).await {
                done.extend(updates.members.into_iter().map(|(id, _)| id));
            }
        }
        for project in self.inner.projection.get_projects() {
            if !done.contains(&project.id) {
                self.check_project(&project).await;
            }
        }
    }

    /// Runs [`ControlPlane::check_all_updates`] every `interval` (from the
    /// first minute after start).
    pub(crate) fn spawn_update_checker(&self) {
        let interval = self.inner.settings.update_check_interval;
        if interval.is_zero() {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60).min(interval)).await;
            loop {
                let Some(inner) = weak.upgrade() else { return };
                let this = ControlPlane { inner };
                if this
                    .inner
                    .shutting_down
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return;
                }
                this.check_all_updates().await;
                drop(this);
                tokio::time::sleep(interval).await;
            }
        });
    }
}

fn summarize(members: Vec<(ProjectId, UpdateStatus)>, error: Option<String>) -> GroupUpdates {
    GroupUpdates {
        checked_at: members
            .iter()
            .map(|(_, s)| s.checked_at.clone())
            .min()
            .unwrap_or_else(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            }),
        head: members.iter().find_map(|(_, s)| s.head.clone()),
        changed_apps: members
            .iter()
            .filter(|(_, s)| s.relevant && !s.up_to_date)
            .count(),
        members,
        error,
    }
}

/// The git directory of a work tree that contains `dir` (`.git` may be a
/// directory, or a file pointing elsewhere as in worktrees and submodules).
fn find_git_dir(dir: &Path) -> Option<PathBuf> {
    let mut current = Some(dir);
    while let Some(d) = current {
        let dot_git = d.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            let path = Path::new(target);
            return Some(if path.is_absolute() {
                path.to_path_buf()
            } else {
                d.join(path)
            });
        }
        current = d.parent();
    }
    None
}

/// The commit `ref_name` points to, from loose refs or packed-refs.
fn read_ref(git_dir: &Path, ref_name: &str) -> Option<String> {
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|c| {
            let p = Path::new(c.trim());
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                git_dir.join(p)
            }
        });
    for dir in [Some(git_dir.to_path_buf()), common].into_iter().flatten() {
        if let Ok(text) = std::fs::read_to_string(dir.join(ref_name)) {
            let sha = text.trim();
            if is_sha(sha) {
                return Some(sha.to_string());
            }
        }
        if let Ok(packed) = std::fs::read_to_string(dir.join("packed-refs")) {
            for line in packed.lines() {
                if let Some((sha, name)) = line.split_once(' ') {
                    if name.trim() == ref_name && is_sha(sha) {
                        return Some(sha.to_string());
                    }
                }
            }
        }
    }
    None
}

/// The commit checked out in the work tree containing `dir`, read from
/// `.git` directly (no git process, so no "dubious ownership" errors).
pub fn read_folder_head(dir: &Path) -> Option<String> {
    let git_dir = find_git_dir(dir)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if is_sha(head) {
        return Some(head.to_string());
    }
    read_ref(&git_dir, head.strip_prefix("ref:")?.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_head_is_read_from_dot_git() {
        let root = tempfile::TempDir::new().unwrap();
        let git = root.path().join(".git");
        std::fs::create_dir_all(git.join("refs/heads")).unwrap();
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let c = "c".repeat(40);
        // A loose branch ref, from a subfolder of the work tree.
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("refs/heads/main"), format!("{a}\n")).unwrap();
        let sub = root.path().join("apps/web");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(read_folder_head(&sub), Some(a.clone()));
        // A packed ref.
        std::fs::write(git.join("HEAD"), "ref: refs/heads/develop\n").unwrap();
        std::fs::write(
            git.join("packed-refs"),
            format!("# pack-refs with: peeled fully-peeled sorted\n{b} refs/heads/develop\n"),
        )
        .unwrap();
        assert_eq!(read_folder_head(root.path()), Some(b));
        // Detached.
        std::fs::write(git.join("HEAD"), format!("{c}\n")).unwrap();
        assert_eq!(read_folder_head(root.path()), Some(c));
        // A worktree: .git is a file pointing at the real git dir.
        let wt = tempfile::TempDir::new().unwrap();
        std::fs::write(
            wt.path().join(".git"),
            format!("gitdir: {}\n", git.display()),
        )
        .unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(read_folder_head(wt.path()), Some(a));
        // Not a repository.
        let plain = tempfile::TempDir::new().unwrap();
        let _ = read_folder_head(plain.path());
    }

    #[test]
    fn cache_dirs_are_per_repository() {
        let data = Path::new("/data");
        assert_eq!(
            cache_dir(data, "https://github.com/acme/web.git"),
            cache_dir(data, "git@github.com:acme/web.git")
        );
        assert_ne!(
            cache_dir(data, "https://github.com/acme/web.git"),
            cache_dir(data, "https://github.com/acme/api.git")
        );
    }
}
