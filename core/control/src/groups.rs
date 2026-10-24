//! App groups: apps deployed from the same repository and branch (say a
//! front end, an API and an admin dashboard), deployed together at one
//! commit, in order.
//!
//! A group deployment resolves one commit, clones the repository once, and
//! builds each member from that checkout (each into its own release
//! directory, so releases stay isolated). With `only_changed`, a member is
//! skipped when none of the files changed between its live commit and the
//! target commit match its watch paths. Members deploy one after another;
//! when one fails the rest are not deployed, and (with
//! `rollback_on_failure`, the default) the members this run already switched
//! go back to the release they had before. Everything is recorded as events
//! (`GroupDeploymentStarted`, `GroupMemberSkipped`, `GroupMemberDeployed`,
//! `GroupDeploymentFinished`, `GroupDeploymentFailed`, ...).

use crate::git::{is_sha, touches, validate_watch_path, Git};
use crate::scan::normalize_repo;
use crate::{ControlError, ControlPlane, ControlResult, DeployRequest};
use aegis_projection::{GroupDeploymentState, GroupMember, GroupState, ProjectState};
use aegis_types::{DeploymentId, ProjectId};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Clone and fetch time limit.
const CLONE_TIMEOUT: Duration = Duration::from_secs(600);
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MEMBERS: usize = 32;
const MAX_WATCH_PATHS: usize = 32;

/// Name, source and behaviour of a group.
#[derive(Debug, Clone, Default)]
pub struct GroupSpec {
    pub name: String,
    pub repository_url: String,
    pub branch: String,
    /// Default: true.
    pub rollback_on_failure: Option<bool>,
}

/// A member to put in a group, in deploy order.
#[derive(Debug, Clone)]
pub struct MemberSpec {
    pub project_id: ProjectId,
    pub watch_paths: Vec<String>,
}

/// What a group rollback did for one member.
#[derive(Debug, Clone)]
pub struct MemberRollback {
    pub project_id: ProjectId,
    pub name: String,
    /// The release that is live now (`None`: the app was stopped, as it had
    /// no release before).
    pub version: Option<String>,
    pub error: Option<String>,
}

fn validate_name(name: &str) -> ControlResult<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ControlError::InvalidArgument(
            "A group name must be 1-64 characters".into(),
        ));
    }
    if name.contains('/') || name.chars().any(|c| c.is_control()) {
        return Err(ControlError::InvalidArgument(
            "A group name must not contain '/' or control characters".into(),
        ));
    }
    Ok(name.to_string())
}

impl ControlPlane {
    pub fn list_groups(&self) -> Vec<GroupState> {
        self.inner.projection.get_groups()
    }

    /// Finds a group by id or (unique) name.
    pub fn resolve_group(&self, key: &str) -> ControlResult<GroupState> {
        if let Some(g) = self.inner.projection.get_group(key) {
            return Ok(g);
        }
        let matches: Vec<_> = self
            .list_groups()
            .into_iter()
            .filter(|g| g.name == key)
            .collect();
        match matches.len() {
            1 => Ok(matches.into_iter().next().unwrap()),
            0 => Err(ControlError::NotFound(format!("Unknown group '{}'", key))),
            _ => Err(ControlError::InvalidArgument(format!(
                "More than one group is named '{}'; use its id",
                key
            ))),
        }
    }

    /// The group an app belongs to, if any.
    pub fn group_of(&self, project_id: &ProjectId) -> Option<GroupState> {
        self.inner.projection.group_of(project_id)
    }

    pub fn group_deployments(&self, group_id: &str) -> Vec<GroupDeploymentState> {
        self.inner.projection.get_group_deployments(group_id)
    }

    pub fn get_group_deployment(&self, id: &str) -> Option<GroupDeploymentState> {
        self.inner.projection.get_group_deployment(id)
    }

    fn group_running(&self, group: &GroupState) -> bool {
        group
            .last_deployment
            .as_ref()
            .and_then(|id| self.inner.projection.get_group_deployment(id))
            .is_some_and(|d| d.status == "Running")
    }

    fn check_spec(&self, spec: &GroupSpec, except: Option<&str>) -> ControlResult<GroupSpec> {
        let name = validate_name(&spec.name)?;
        if self
            .list_groups()
            .iter()
            .any(|g| g.name == name && Some(g.id.as_str()) != except)
        {
            return Err(ControlError::InvalidArgument(format!(
                "A group named '{}' already exists",
                name
            )));
        }
        let url = spec.repository_url.trim().to_string();
        if url.is_empty() {
            return Err(ControlError::InvalidArgument(
                "A group needs the repository its apps are deployed from".into(),
            ));
        }
        aegis_builder::validate_repository_url(&url)
            .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        let branch = match spec.branch.trim() {
            "" => "main".to_string(),
            b => b.to_string(),
        };
        aegis_builder::validate_git_ref(&branch)
            .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        Ok(GroupSpec {
            name,
            repository_url: url,
            branch,
            rollback_on_failure: spec.rollback_on_failure,
        })
    }

    /// Why an app can't be in this group, if it can't.
    fn member_problem(
        &self,
        group: &GroupSpec,
        group_id: &str,
        project: &ProjectState,
    ) -> Option<String> {
        if project.repository_url.is_empty() || project.source_dir.is_some() {
            return Some(format!(
                "'{}' is deployed from a folder; only apps deployed from {} can be in this group",
                project.name, group.repository_url
            ));
        }
        if normalize_repo(&project.repository_url) != normalize_repo(&group.repository_url) {
            return Some(format!(
                "'{}' is deployed from {}, not from the group's repository {}",
                project.name, project.repository_url, group.repository_url
            ));
        }
        if project.branch != group.branch {
            return Some(format!(
                "'{}' deploys branch '{}', not the group's branch '{}'",
                project.name, project.branch, group.branch
            ));
        }
        if let Some(other) = self.group_of(&project.id).filter(|g| g.id != group_id) {
            return Some(format!(
                "'{}' is already in group '{}'; an app can be in one group only",
                project.name, other.name
            ));
        }
        None
    }

    /// Creates a group, optionally with its members.
    pub async fn create_group(
        &self,
        spec: GroupSpec,
        members: Vec<MemberSpec>,
    ) -> ControlResult<GroupState> {
        let spec = self.check_spec(&spec, None)?;
        let id = aegis_types::EventId::new().to_string();
        let members = self.check_members(&spec, &id, members)?;
        self.record(
            "GroupCreated",
            json!({
                "group_id": id,
                "name": spec.name,
                "repository_url": spec.repository_url,
                "branch": spec.branch,
                "rollback_on_failure": spec.rollback_on_failure.unwrap_or(true),
            }),
        )
        .await?;
        if !members.is_empty() {
            self.record(
                "GroupMembersChanged",
                json!({"group_id": id, "members": members}),
            )
            .await?;
        }
        self.inner
            .projection
            .get_group(&id)
            .ok_or_else(|| ControlError::Internal(anyhow::anyhow!("Group was not recorded")))
    }

    /// Changes a group's name, repository, branch or failure behaviour. Its
    /// members must still match the repository and branch.
    pub async fn update_group(&self, id: &str, spec: GroupSpec) -> ControlResult<GroupState> {
        let group = self.resolve_group(id)?;
        let spec = self.check_spec(&spec, Some(&group.id))?;
        for member in &group.members {
            if let Some(project) = self.inner.projection.get_project(&member.project_id) {
                if let Some(problem) = self.member_problem(&spec, &group.id, &project) {
                    return Err(ControlError::FailedPrecondition(format!(
                        "{}; remove it from the group first",
                        problem
                    )));
                }
            }
        }
        self.record(
            "GroupUpdated",
            json!({
                "group_id": group.id,
                "name": spec.name,
                "repository_url": spec.repository_url,
                "branch": spec.branch,
                "rollback_on_failure": spec.rollback_on_failure.unwrap_or(group.rollback_on_failure),
            }),
        )
        .await?;
        self.resolve_group(&group.id)
    }

    pub async fn delete_group(&self, id: &str) -> ControlResult<()> {
        let group = self.resolve_group(id)?;
        if self.group_running(&group) {
            return Err(ControlError::FailedPrecondition(format!(
                "Group '{}' is being deployed; wait until it has finished",
                group.name
            )));
        }
        self.record(
            "GroupDeleted",
            json!({"group_id": group.id, "name": group.name}),
        )
        .await?;
        Ok(())
    }

    fn check_members(
        &self,
        spec: &GroupSpec,
        group_id: &str,
        members: Vec<MemberSpec>,
    ) -> ControlResult<Vec<GroupMember>> {
        if members.len() > MAX_MEMBERS {
            return Err(ControlError::InvalidArgument(format!(
                "A group has at most {} apps",
                MAX_MEMBERS
            )));
        }
        let mut out: Vec<GroupMember> = Vec::new();
        for member in members {
            let project = self
                .inner
                .projection
                .get_project(&member.project_id)
                .ok_or_else(|| {
                    ControlError::NotFound(format!("Unknown project {}", member.project_id))
                })?;
            if out.iter().any(|m| m.project_id == project.id) {
                return Err(ControlError::InvalidArgument(format!(
                    "'{}' is listed twice",
                    project.name
                )));
            }
            if let Some(problem) = self.member_problem(spec, group_id, &project) {
                return Err(ControlError::InvalidArgument(problem));
            }
            if member.watch_paths.len() > MAX_WATCH_PATHS {
                return Err(ControlError::InvalidArgument(format!(
                    "At most {} watch paths per app",
                    MAX_WATCH_PATHS
                )));
            }
            let mut watch_paths = Vec::new();
            for p in &member.watch_paths {
                validate_watch_path(p).map_err(ControlError::InvalidArgument)?;
                let p = p.trim().to_string();
                if !watch_paths.contains(&p) {
                    watch_paths.push(p);
                }
            }
            out.push(GroupMember {
                project_id: project.id,
                watch_paths,
            });
        }
        Ok(out)
    }

    /// Replaces a group's members (in deploy order, with their watch paths).
    pub async fn set_group_members(
        &self,
        id: &str,
        members: Vec<MemberSpec>,
    ) -> ControlResult<GroupState> {
        let group = self.resolve_group(id)?;
        if self.group_running(&group) {
            return Err(ControlError::FailedPrecondition(format!(
                "Group '{}' is being deployed; change its apps when it has finished",
                group.name
            )));
        }
        let spec = GroupSpec {
            name: group.name.clone(),
            repository_url: group.repository_url.clone(),
            branch: group.branch.clone(),
            rollback_on_failure: Some(group.rollback_on_failure),
        };
        let members = self.check_members(&spec, &group.id, members)?;
        self.record(
            "GroupMembersChanged",
            json!({"group_id": group.id, "members": members}),
        )
        .await?;
        self.resolve_group(&group.id)
    }

    /// Starts a deployment of the whole group (see the module docs); follow
    /// it with [`ControlPlane::get_group_deployment`] or the events.
    pub async fn deploy_group(
        &self,
        id: &str,
        commit: Option<String>,
        only_changed: bool,
        trigger: &str,
    ) -> ControlResult<String> {
        let group = self.resolve_group(id)?;
        if group.members.is_empty() {
            return Err(ControlError::FailedPrecondition(format!(
                "Group '{}' has no apps yet",
                group.name
            )));
        }
        if let Some(c) = &commit {
            aegis_builder::validate_git_ref(c)
                .map_err(|e| ControlError::InvalidArgument(e.to_string()))?;
        }
        let lock = self.group_lock(&group.id);
        let Ok(guard) = lock.clone().try_lock_owned() else {
            return Err(ControlError::FailedPrecondition(format!(
                "Group '{}' is already being deployed or rolled back",
                group.name
            )));
        };
        for member in &group.members {
            if let Some(d) = self.active_deployment_of(&member.project_id) {
                return Err(ControlError::FailedPrecondition(format!(
                    "A deployment of a member app is in progress ({}); try again when it has finished",
                    d
                )));
            }
        }
        let run_id = aegis_types::EventId::new().to_string();
        self.record(
            "GroupDeploymentStarted",
            json!({
                "group_id": group.id,
                "group_deployment_id": run_id,
                "name": group.name,
                "commit": commit,
                "only_changed": only_changed,
                "trigger": trigger,
                "members": group.members.iter().map(|m| m.project_id.to_string()).collect::<Vec<_>>(),
            }),
        )
        .await?;
        let this = self.clone();
        let id = run_id.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let result = this
                .run_group_deployment(&group, &id, commit, only_changed)
                .await;
            if let Err(reason) = result {
                let _ = this
                    .record(
                        "GroupDeploymentFailed",
                        json!({"group_id": group.id, "group_deployment_id": id, "reason": reason}),
                    )
                    .await;
            }
        });
        Ok(run_id)
    }

    fn group_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .group_locks
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    fn active_deployment_of(&self, project_id: &ProjectId) -> Option<DeploymentId> {
        self.inner
            .projection
            .get_deployments()
            .into_iter()
            .find(|d| {
                &d.project_id == project_id && matches!(d.status.as_str(), "Queued" | "InProgress")
            })
            .map(|d| d.id)
    }

    /// The first member's deploy key: the group's apps share the repository.
    pub(crate) fn group_ssh(&self, group: &GroupState) -> Option<aegis_ssh::SshAuth> {
        group
            .members
            .iter()
            .find_map(|m| self.ssh_auth(&m.project_id))
    }

    /// The URL to clone a group's repository from (over SSH with a key).
    pub(crate) fn group_clone_url(&self, group: &GroupState, has_key: bool) -> String {
        match crate::sources::with_deploy_key_url(
            aegis_builder::SourceSpec::Git {
                url: group.repository_url.clone(),
                reference: group.branch.clone(),
            },
            has_key,
        ) {
            aegis_builder::SourceSpec::Git { url, .. } => url,
            aegis_builder::SourceSpec::LocalDir(_) => group.repository_url.clone(),
        }
    }

    /// Runs a group deployment. `Err` is the reason it failed before any
    /// member was deployed (recorded by the caller).
    async fn run_group_deployment(
        &self,
        group: &GroupState,
        run_id: &str,
        commit: Option<String>,
        only_changed: bool,
    ) -> Result<(), String> {
        let scratch = self
            .inner
            .settings
            .data_dir
            .join("tmp")
            .join(format!("group-{}", run_id));
        let result = self
            .group_deployment_in(group, run_id, commit, only_changed, &scratch)
            .await;
        let _ = tokio::fs::remove_dir_all(&scratch).await;
        result
    }

    async fn group_deployment_in(
        &self,
        group: &GroupState,
        run_id: &str,
        commit: Option<String>,
        only_changed: bool,
        scratch: &Path,
    ) -> Result<(), String> {
        // 1. One clone for the whole group, at one commit.
        let ssh = self.group_ssh(group);
        let git = Git::new(ssh.as_ref())?;
        let url = self.group_clone_url(group, ssh.is_some());
        let checkout = scratch.join("checkout");
        tokio::fs::create_dir_all(scratch)
            .await
            .map_err(|e| format!("Cannot create {}: {}", scratch.display(), e))?;
        let checkout_text = checkout.to_string_lossy().to_string();
        git.run(
            scratch,
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                &format!("--branch={}", group.branch),
                "--",
                &url,
                &checkout_text,
            ],
            CLONE_TIMEOUT,
        )
        .await
        .map_err(|e| format!("Cloning {} failed: {}", aegis_builder::redact_url(&url), e))?;
        let target = match &commit {
            Some(c) => c.clone(),
            None => "HEAD".to_string(),
        };
        let sha = match git
            .run(
                &checkout,
                &["rev-parse", "--verify", &format!("{}^{{commit}}", target)],
                GIT_TIMEOUT,
            )
            .await
        {
            Ok(s) => s.trim().to_string(),
            Err(_) if commit.is_some() => {
                // A commit outside the branch's history: fetch it.
                git.run(
                    &checkout,
                    &["fetch", "--filter=blob:none", "origin", &target],
                    CLONE_TIMEOUT,
                )
                .await
                .map_err(|e| format!("Commit {} not found: {}", target, e))?;
                git.run(
                    &checkout,
                    &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
                    GIT_TIMEOUT,
                )
                .await
                .map_err(|e| format!("Commit {} not found: {}", target, e))?
                .trim()
                .to_string()
            }
            Err(e) => return Err(format!("Cannot resolve {}: {}", target, e)),
        };
        git.run(
            &checkout,
            &["checkout", "-q", "--detach", &sha],
            CLONE_TIMEOUT,
        )
        .await
        .map_err(|e| format!("Checking out {} failed: {}", sha, e))?;
        let _ = self
            .record(
                "GroupCheckoutReady",
                json!({"group_id": group.id, "group_deployment_id": run_id, "commit": sha,
                       "checkout": checkout_text}),
            )
            .await;

        // 2. Members in order.
        let mut switched: Vec<(ProjectId, Option<String>)> = Vec::new();
        for member in &group.members {
            if self
                .inner
                .shutting_down
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err("The daemon is shutting down".into());
            }
            let Some(project) = self.inner.projection.get_project(&member.project_id) else {
                continue;
            };
            let pid = project.id.to_string();
            let live_commit = self.live_commit(&project);
            if only_changed {
                if let Some(reason) = self
                    .unchanged_reason(
                        &git,
                        &checkout,
                        live_commit.as_deref(),
                        &sha,
                        &member.watch_paths,
                    )
                    .await
                {
                    let _ = self
                        .record(
                            "GroupMemberSkipped",
                            json!({"group_id": group.id, "group_deployment_id": run_id,
                                   "project_id": pid, "reason": reason}),
                        )
                        .await;
                    continue;
                }
            }
            let dir = match &project.source_subdir {
                Some(sub) => checkout.join(sub),
                None => checkout.clone(),
            };
            let previous = project.current_release.clone();
            let queued = self
                .queue_deployment(DeployRequest {
                    project_id: project.id,
                    branch: Some(group.branch.clone()),
                    source_dir: Some(dir),
                    trigger: format!("group:{}", group.name),
                    ..Default::default()
                })
                .await;
            let outcome = match queued {
                Ok(id) => {
                    let _ = self
                        .record(
                            "GroupMemberDeploying",
                            json!({"group_id": group.id, "group_deployment_id": run_id,
                                   "project_id": pid, "deployment_id": id.to_string(),
                                   "previous_version": previous}),
                        )
                        .await;
                    self.wait_deployment(id).await.map(|v| (id, v))
                }
                Err(e) => Err(e.to_string()),
            };
            match outcome {
                Ok((id, version)) => {
                    let _ = self
                        .record(
                            "GroupMemberDeployed",
                            json!({"group_id": group.id, "group_deployment_id": run_id,
                                   "project_id": pid, "deployment_id": id.to_string(),
                                   "version": version, "previous_version": previous,
                                   "commit": sha}),
                        )
                        .await;
                    switched.push((project.id, previous));
                }
                Err(reason) => {
                    let _ = self
                        .record(
                            "GroupMemberFailed",
                            json!({"group_id": group.id, "group_deployment_id": run_id,
                                   "project_id": pid, "reason": reason}),
                        )
                        .await;
                    let mut rolled_back = Vec::new();
                    if group.rollback_on_failure && !switched.is_empty() {
                        for result in self.roll_back_members(&switched).await {
                            if result.error.is_none() {
                                rolled_back.push(result.project_id.to_string());
                            }
                        }
                        let _ = self
                            .record(
                                "GroupMembersRolledBack",
                                json!({"group_id": group.id, "group_deployment_id": run_id,
                                       "project_ids": rolled_back, "reason": "failure"}),
                            )
                            .await;
                    }
                    let _ = self
                        .record(
                            "GroupDeploymentFailed",
                            json!({"group_id": group.id, "group_deployment_id": run_id,
                                   "failed_project_id": pid,
                                   "reason": format!("{} failed: {}", project.name, reason),
                                   "rolled_back": rolled_back,
                                   "not_deployed": group.members.iter()
                                       .skip_while(|m| m.project_id != project.id).skip(1)
                                       .map(|m| m.project_id.to_string()).collect::<Vec<_>>()}),
                        )
                        .await;
                    return Ok(());
                }
            }
        }
        let _ = self
            .record(
                "GroupDeploymentFinished",
                json!({"group_id": group.id, "group_deployment_id": run_id, "commit": sha,
                       "deployed": switched.iter().map(|(p, _)| p.to_string()).collect::<Vec<_>>()}),
            )
            .await;
        Ok(())
    }

    /// The commit a project's live release was built from, when known.
    pub(crate) fn live_commit(&self, project: &ProjectState) -> Option<String> {
        let version = project.current_release.as_ref()?;
        self.inner
            .projection
            .find_release(&project.id, version)
            .map(|r| r.commit_sha)
            .filter(|s| is_sha(s))
    }

    /// `Some(reason)` when nothing this member watches changed between its
    /// live commit and `target`.
    async fn unchanged_reason(
        &self,
        git: &Git,
        checkout: &Path,
        live: Option<&str>,
        target: &str,
        watch: &[String],
    ) -> Option<String> {
        let live = live?;
        if live == target {
            return Some(format!(
                "Already live at {}",
                &target[..12.min(target.len())]
            ));
        }
        let files = changed_files(git, checkout, live, target).await.ok()?;
        (!touches(watch, &files)).then(|| {
            if watch.is_empty() {
                "No files changed".to_string()
            } else {
                format!(
                    "None of the {} changed file(s) match its watch paths ({})",
                    files.len(),
                    watch.join(", ")
                )
            }
        })
    }

    /// Waits until a deployment has finished; `Ok(version)` when it went live.
    async fn wait_deployment(&self, id: DeploymentId) -> Result<Option<String>, String> {
        loop {
            match self.inner.projection.get_deployment(&id) {
                Some(d) if d.status == "Success" => return Ok(d.version),
                Some(d) if !matches!(d.status.as_str(), "Queued" | "InProgress") => {
                    return Err(d.error.unwrap_or_else(|| d.status.clone()))
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Puts members back on the releases they had (newest first); a member
    /// that had none is stopped.
    async fn roll_back_members(
        &self,
        switched: &[(ProjectId, Option<String>)],
    ) -> Vec<MemberRollback> {
        let mut results = Vec::new();
        for (project_id, previous) in switched.iter().rev() {
            let name = self
                .inner
                .projection
                .get_project(project_id)
                .map(|p| p.name)
                .unwrap_or_default();
            let result = match previous {
                Some(version) => self
                    .rollback(*project_id, Some(version.clone()))
                    .await
                    .map(Some),
                None => self
                    .process_action(&project_id.to_string(), crate::ProcessAction::Stop)
                    .await
                    .map(|_| None),
            };
            results.push(match result {
                Ok(version) => MemberRollback {
                    project_id: *project_id,
                    name,
                    version,
                    error: None,
                },
                Err(e) => MemberRollback {
                    project_id: *project_id,
                    name,
                    version: None,
                    error: Some(e.to_string()),
                },
            });
        }
        results
    }

    /// Rolls back the group's latest deployment: every member it switched
    /// goes back to the release it had before.
    pub async fn rollback_group(&self, id: &str) -> ControlResult<Vec<MemberRollback>> {
        let group = self.resolve_group(id)?;
        let lock = self.group_lock(&group.id);
        let Ok(_guard) = lock.try_lock() else {
            return Err(ControlError::FailedPrecondition(format!(
                "Group '{}' is being deployed or rolled back",
                group.name
            )));
        };
        let run = self
            .group_deployments(&group.id)
            .into_iter()
            .rev()
            .find(|d| d.members.iter().any(|m| m.status == "deployed"))
            .ok_or_else(|| {
                ControlError::FailedPrecondition(format!(
                    "Group '{}' has no group deployment to roll back",
                    group.name
                ))
            })?;
        let switched: Vec<(ProjectId, Option<String>)> = run
            .members
            .iter()
            .filter(|m| m.status == "deployed")
            .map(|m| (m.project_id, m.previous_version.clone()))
            .collect();
        self.record(
            "GroupRollbackStarted",
            json!({"group_id": group.id, "group_deployment_id": run.id,
                   "project_ids": switched.iter().map(|(p, _)| p.to_string()).collect::<Vec<_>>()}),
        )
        .await?;
        let results = self.roll_back_members(&switched).await;
        let ok: Vec<String> = results
            .iter()
            .filter(|r| r.error.is_none())
            .map(|r| r.project_id.to_string())
            .collect();
        self.record(
            "GroupMembersRolledBack",
            json!({"group_id": group.id, "group_deployment_id": run.id, "project_ids": ok,
                   "reason": "requested",
                   "failed": results.iter().filter_map(|r| Some(json!({"project_id": r.project_id.to_string(), "error": r.error.clone()?}))).collect::<Vec<_>>()}),
        )
        .await?;
        Ok(results)
    }

    /// Marks group deployments that were running when the daemon stopped as
    /// failed (their member deployments are failed by recovery too).
    pub(crate) async fn fail_interrupted_group_deployments(&self) {
        for run in self.inner.projection.running_group_deployments() {
            let _ = self
                .record(
                    "GroupDeploymentFailed",
                    json!({"group_id": run.group_id, "group_deployment_id": run.id,
                           "reason": "Interrupted: the daemon stopped before this group deployment finished"}),
                )
                .await;
        }
        let scratch = self.inner.settings.data_dir.join("tmp");
        if let Ok(mut entries) = tokio::fs::read_dir(&scratch).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.file_name().to_string_lossy().starts_with("group-") {
                    let _ = tokio::fs::remove_dir_all(entry.path()).await;
                }
            }
        }
    }
}

/// Files changed between two commits of a checkout (paths from its root).
pub(crate) async fn changed_files(
    git: &Git,
    checkout: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<String>, String> {
    git.run(
        checkout,
        &["cat-file", "-e", &format!("{}^{{commit}}", from)],
        GIT_TIMEOUT,
    )
    .await?;
    let out = git
        .run(
            checkout,
            &["diff", "--name-only", "--no-renames", from, to],
            GIT_TIMEOUT,
        )
        .await?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}
