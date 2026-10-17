//! The daemon's updater: periodic checks, the cached result, and applying an
//! update from the dashboard/API (backup first, verified install, restart).

use crate::helper;
use crate::install::{self, InstallMode};
use crate::release::{Release, Target};
use crate::signature::{self, Keyring, SignaturePolicy};
use crate::source::Source;
use crate::state::{self, AppliedUpdate, LastCheck, Pending, Persisted};
use crate::version::{is_major_jump, parse_version, Version};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

/// What the daemon provides to the updater.
// async-trait marks the boxed futures #[must_use] (clippy::double_must_use).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait UpdateHooks: Send + Sync + 'static {
    /// Records an audit event (no secrets in `payload`).
    async fn record(&self, event_type: &str, payload: serde_json::Value);
    /// Takes a backup; returns its name.
    async fn backup(&self) -> anyhow::Result<String>;
}

#[derive(Debug, Clone)]
pub struct UpdateSettings {
    /// `[update] check`: when false the updater makes no network requests.
    pub check: bool,
    pub interval: Duration,
    /// Delay before the first periodic check after start.
    pub initial_delay: Duration,
    pub repo: String,
    /// Loopback fake API for tests; `None` for GitHub.
    pub api_base: Option<String>,
    pub current_version: String,
    /// The daemon binary to replace.
    pub exe: PathBuf,
    /// `<data_dir>/update`: state, and the helper's request/result files.
    pub state_dir: PathBuf,
    /// The helper's path unit; its presence selects [`InstallMode::Helper`].
    pub helper_unit: PathBuf,
    /// The release archive to install; `None` when there is none for this
    /// machine.
    pub target: Option<Target>,
    /// Run the new binary's `--version` before swapping it in.
    pub verify_runs: bool,
    /// Shown when the daemon can't install updates itself.
    pub manual_command: String,
    /// How long to wait for the helper before reporting a failure.
    pub helper_timeout: Duration,
    /// The keys release signatures are checked against: the compiled-in
    /// release keys (tests use their own). Empty: nothing can be installed
    /// from the dashboard.
    pub keyring: Keyring,
}

impl UpdateSettings {
    pub fn new(current_version: &str, exe: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            check: true,
            interval: Duration::from_secs(12 * 3600),
            initial_delay: Duration::from_secs(60),
            repo: crate::DEFAULT_REPO.to_string(),
            api_base: None,
            current_version: current_version.to_string(),
            exe,
            state_dir,
            helper_unit: PathBuf::from(install::HELPER_PATH_UNIT),
            target: Target::current(),
            verify_runs: true,
            manual_command: "sudo aegis-daemon self-update".to_string(),
            helper_timeout: Duration::from_secs(15 * 60),
            keyring: Keyring::release().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Downloading,
    BackingUp,
    Installing,
    WaitingForHelper,
    Restarting,
    Failed,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Downloading => "downloading",
            Self::BackingUp => "backing_up",
            Self::Installing => "installing",
            Self::WaitingForHelper => "waiting_for_helper",
            Self::Restarting => "restarting",
            Self::Failed => "failed",
        }
    }

    fn busy(&self) -> bool {
        !matches!(self, Self::Idle | Self::Failed)
    }
}

/// A snapshot for the API and dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub check_enabled: bool,
    pub checked_at: Option<String>,
    pub check_error: Option<String>,
    pub latest: Option<Release>,
    pub update_available: bool,
    pub mode: InstallMode,
    /// What applying means on this machine, in a sentence.
    pub mode_detail: String,
    /// For [`InstallMode::Manual`]: the command an administrator runs.
    pub manual_command: Option<String>,
    /// Why the available update can't be applied from the dashboard.
    pub blocked_reason: Option<String>,
    pub phase: Phase,
    pub phase_error: Option<String>,
    pub last_update: Option<AppliedUpdate>,
    /// IDs of the release keys this build accepts signatures from; empty
    /// when it has none (then nothing is installed from the dashboard).
    pub release_keys: Vec<String>,
}

#[derive(Debug)]
pub enum UpdateError {
    /// `[update] check = false`.
    Disabled,
    /// Nothing newer to install (or a different version than expected).
    NothingToApply(String),
    Busy,
    /// This machine can't install it from the daemon (with the reason).
    NotPossible(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(
                f,
                "Update checks are disabled ([update] check = false in the daemon configuration)"
            ),
            Self::NothingToApply(m) | Self::NotPossible(m) => write!(f, "{}", m),
            Self::Busy => write!(f, "An update is already in progress"),
        }
    }
}

impl std::error::Error for UpdateError {}

struct Inner {
    persisted: Persisted,
    check_error: Option<String>,
    phase: Phase,
    phase_error: Option<String>,
}

pub struct UpdateManager {
    settings: UpdateSettings,
    hooks: Arc<dyn UpdateHooks>,
    source: Source,
    inner: Mutex<Inner>,
    check_lock: tokio::sync::Mutex<()>,
    restart: watch::Sender<bool>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl UpdateManager {
    pub fn new(settings: UpdateSettings, hooks: Arc<dyn UpdateHooks>) -> Arc<Self> {
        let source = Source::new(
            settings.api_base.clone(),
            &settings.repo,
            &format!("aegis-daemon/{}", settings.current_version),
        )
        .with_keyring(settings.keyring.clone());
        let persisted = state::load(&settings.state_dir);
        Arc::new(Self {
            settings,
            hooks,
            source,
            inner: Mutex::new(Inner {
                persisted,
                check_error: None,
                phase: Phase::Idle,
                phase_error: None,
            }),
            check_lock: tokio::sync::Mutex::new(()),
            restart: watch::channel(false).0,
        })
    }

    pub fn settings(&self) -> &UpdateSettings {
        &self.settings
    }

    /// Flips to `true` when a new binary is in place and the daemon should
    /// shut down and start again.
    pub fn restart_requested(&self) -> watch::Receiver<bool> {
        self.restart.subscribe()
    }

    fn current(&self) -> Option<Version> {
        parse_version(&self.settings.current_version)
    }

    pub fn mode(&self) -> InstallMode {
        install::detect_mode(
            &self.settings.exe,
            &self.settings.helper_unit,
            self.settings
                .target
                .is_some_and(|t| t.self_update_supported()),
        )
    }

    fn mode_detail(&self, mode: InstallMode) -> String {
        let exe = self.settings.exe.display();
        match mode {
            InstallMode::InPlace => format!(
                "Aegis replaces {} itself (keeping {}.previous) and restarts.",
                exe, exe
            ),
            InstallMode::Helper => format!(
                "{} is owned by root; the aegis-update helper service (installed by install.sh) downloads, verifies and installs the release, then restarts Aegis.",
                exe
            ),
            InstallMode::Manual => format!(
                "Aegis can't write {} (it is owned by another user or on a read-only path), so an administrator runs the update on the server.",
                exe
            ),
            InstallMode::Unsupported => match self.settings.target {
                Some(t) if !t.self_update_supported() => format!(
                    "Self-update isn't supported on {}; download the release from GitHub.",
                    t
                ),
                _ => "No release is published for this machine; build Aegis from source."
                    .to_string(),
            },
        }
    }

    /// The cached release, when it is newer than this build.
    fn newer_release(&self, persisted: &Persisted) -> Option<Release> {
        let release = &persisted.last_check.as_ref()?.release;
        let latest = parse_version(&release.tag)?;
        (!release.draft
            && !release.prerelease
            && latest.is_stable()
            && self.current().is_some_and(|c| latest > c))
        .then(|| release.clone())
    }

    fn blocked_reason(&self, release: &Release) -> Option<String> {
        let (current, latest) = (self.current()?, parse_version(&release.tag)?);
        if is_major_jump(&current, &latest) {
            return Some(format!(
                "{} is a new major version. Read the release notes, then upgrade on the server with `aegis-daemon self-update --version {} --force`.",
                release.tag, release.tag
            ));
        }
        self.settings.keyring.is_empty().then(|| {
            format!(
                "This Aegis build has no release signing key, so it can't check that {} is genuine and won't install it from the dashboard. Install the release by hand, or run on the server (checks SHA-256 only, recorded in Events): {} --allow-unsigned",
                release.tag, self.settings.manual_command
            )
        })
    }

    pub fn status(&self) -> UpdateStatus {
        let mode = self.mode();
        let inner = self.inner.lock().unwrap();
        let newer = self.newer_release(&inner.persisted);
        UpdateStatus {
            current_version: self.settings.current_version.clone(),
            check_enabled: self.settings.check,
            checked_at: inner
                .persisted
                .last_check
                .as_ref()
                .map(|c| c.checked_at.clone()),
            check_error: inner.check_error.clone(),
            latest: inner
                .persisted
                .last_check
                .as_ref()
                .map(|c| c.release.clone()),
            update_available: newer.is_some(),
            mode,
            mode_detail: self.mode_detail(mode),
            manual_command: (mode == InstallMode::Manual)
                .then(|| self.settings.manual_command.clone()),
            blocked_reason: newer.as_ref().and_then(|r| self.blocked_reason(r)),
            phase: inner.phase,
            phase_error: inner.phase_error.clone(),
            last_update: inner.persisted.last_update.clone(),
            release_keys: self.settings.keyring.ids(),
        }
    }

    fn save(&self, persisted: &Persisted) {
        state::save(&self.settings.state_dir, persisted);
    }

    /// Asks GitHub for the latest release now. Failures are kept in the
    /// status (and logged), not returned. Errors only when checks are off.
    pub async fn check_now(&self) -> Result<UpdateStatus, UpdateError> {
        if !self.settings.check {
            return Err(UpdateError::Disabled);
        }
        let _one_at_a_time = self.check_lock.lock().await;
        match self.source.latest().await {
            Ok(release) => {
                let notify = {
                    let mut inner = self.inner.lock().unwrap();
                    inner.check_error = None;
                    inner.persisted.last_check = Some(LastCheck {
                        checked_at: now(),
                        release: release.clone(),
                    });
                    let notify = self.newer_release(&inner.persisted).is_some()
                        && inner.persisted.notified_tag.as_deref() != Some(release.tag.as_str());
                    if notify {
                        inner.persisted.notified_tag = Some(release.tag.clone());
                    }
                    self.save(&inner.persisted);
                    notify
                };
                if notify {
                    tracing::info!(
                        current = %self.settings.current_version,
                        latest = %release.tag,
                        "A new Aegis release is available"
                    );
                    self.hooks
                        .record(
                            "UpdateAvailable",
                            json!({
                                "current_version": self.settings.current_version,
                                "latest_version": release.tag,
                                "url": release.html_url,
                            }),
                        )
                        .await;
                }
            }
            Err(e) => {
                tracing::warn!(error = %format!("{:#}", e), "Update check failed");
                self.inner.lock().unwrap().check_error = Some(format!("{:#}", e));
            }
        }
        Ok(self.status())
    }

    /// Checks every `interval` (first after `initial_delay`). Does nothing
    /// when checks are disabled.
    pub fn spawn_periodic(self: &Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        if !self.settings.check {
            tracing::info!("Update checks are disabled ([update] check = false)");
            return None;
        }
        let me = self.clone();
        Some(tokio::spawn(async move {
            tokio::time::sleep(me.settings.initial_delay).await;
            loop {
                let _ = me.check_now().await;
                tokio::time::sleep(me.settings.interval).await;
            }
        }))
    }

    /// Records the outcome of an update after the daemon (re)started, and
    /// notices updates installed by other means.
    pub async fn on_boot(&self) {
        let current = self.settings.current_version.clone();
        let _ = helper::take_result(&self.settings.state_dir);
        // Written by `aegis-daemon self-update` (the helper, or an admin)
        // when it installed this version.
        let record = helper::take_installed(&self.settings.state_dir).filter(|r| {
            parse_version(&r.version).is_some() && parse_version(&r.version) == parse_version(&current)
        });
        let (events, persisted) = {
            let mut inner = self.inner.lock().unwrap();
            let p = &mut inner.persisted;
            let mut events = Vec::new();
            if let Some(pending) = p.pending.take() {
                let installed = parse_version(&pending.to_version);
                if installed.is_some() && installed == parse_version(&current) {
                    let signed_by = pending
                        .signed_by
                        .clone()
                        .or_else(|| record.as_ref().and_then(|r| r.signed_by.clone()));
                    p.last_update = Some(AppliedUpdate {
                        from_version: pending.from_version.clone(),
                        to_version: current.clone(),
                        at: now(),
                        method: pending.method.clone(),
                        signed_by: signed_by.clone(),
                        allow_unsigned: false,
                    });
                    events.push((
                        "UpdateApplied",
                        json!({
                            "from_version": pending.from_version,
                            "to_version": current,
                            "method": pending.method,
                            "backup": pending.backup,
                            "actor": pending.actor,
                            "signed_by": signed_by,
                        }),
                    ));
                } else {
                    events.push((
                        "UpdateFailed",
                        json!({
                            "from_version": pending.from_version,
                            "to_version": pending.to_version,
                            "stage": "restart",
                            "error": format!(
                                "Aegis {} started instead of {}: the new binary was rolled back or never started",
                                current, pending.to_version
                            ),
                            "actor": pending.actor,
                        }),
                    ));
                }
            } else if let Some(last) = p.last_version.clone() {
                let (before, now_running) = (parse_version(&last), parse_version(&current));
                if let (Some(before), Some(now_running)) = (before, now_running) {
                    if let Some(record) = record.filter(|_| now_running != before) {
                        // `aegis-daemon self-update` run by an admin (also
                        // reinstalls and downgrades with --force).
                        if record.allow_unsigned {
                            tracing::warn!(
                                version = %current,
                                "This version was installed with --allow-unsigned: its release signature was not checked"
                            );
                        }
                        p.last_update = Some(AppliedUpdate {
                            from_version: last.clone(),
                            to_version: current.clone(),
                            at: now(),
                            method: record.method.clone(),
                            signed_by: record.signed_by.clone(),
                            allow_unsigned: record.allow_unsigned,
                        });
                        events.push((
                            "UpdateApplied",
                            json!({"from_version": last, "to_version": current,
                                   "method": record.method,
                                   "signed_by": record.signed_by,
                                   "allow_unsigned": record.allow_unsigned}),
                        ));
                    } else if now_running > before {
                        p.last_update = Some(AppliedUpdate {
                            from_version: last.clone(),
                            to_version: current.clone(),
                            at: now(),
                            method: "external".into(),
                            signed_by: None,
                            allow_unsigned: false,
                        });
                        events.push((
                            "UpdateApplied",
                            json!({"from_version": last, "to_version": current,
                                   "method": "external"}),
                        ));
                    } else if now_running < before {
                        tracing::warn!(from = %last, to = %current, "Aegis was downgraded");
                    }
                }
            }
            p.last_version = Some(current.clone());
            (events, p.clone())
        };
        self.save(&persisted);
        for (event_type, payload) in events {
            self.hooks.record(event_type, payload).await;
        }
    }

    /// Checks that the cached update can be applied now; returns it.
    fn prepare(&self, expected: Option<&str>) -> Result<(Release, InstallMode), UpdateError> {
        if !self.settings.check {
            return Err(UpdateError::Disabled);
        }
        let mode = self.mode();
        let mut inner = self.inner.lock().unwrap();
        if inner.phase.busy() {
            return Err(UpdateError::Busy);
        }
        let release = self.newer_release(&inner.persisted).ok_or_else(|| {
            UpdateError::NothingToApply("No newer release is known; check for updates first".into())
        })?;
        if let Some(expected) = expected {
            if parse_version(expected) != parse_version(&release.tag) {
                return Err(UpdateError::NothingToApply(format!(
                    "The latest release is {}, not {}; reload and try again",
                    release.tag, expected
                )));
            }
        }
        if let Some(reason) = self.blocked_reason(&release) {
            return Err(UpdateError::NotPossible(reason));
        }
        match mode {
            InstallMode::InPlace | InstallMode::Helper => {}
            InstallMode::Manual => {
                return Err(UpdateError::NotPossible(format!(
                    "{} Run on the server: {}",
                    self.mode_detail(mode),
                    self.settings.manual_command
                )))
            }
            InstallMode::Unsupported => {
                return Err(UpdateError::NotPossible(self.mode_detail(mode)))
            }
        }
        inner.phase = Phase::Downloading;
        inner.phase_error = None;
        Ok((release, mode))
    }

    /// Validates, then applies in the background. The status shows progress;
    /// on success the daemon restarts.
    pub fn start_apply(
        self: &Arc<Self>,
        actor: String,
        expected: Option<&str>,
    ) -> Result<UpdateStatus, UpdateError> {
        let (release, mode) = self.prepare(expected)?;
        let me = self.clone();
        tokio::spawn(async move { me.run(release, mode, actor).await });
        Ok(self.status())
    }

    /// Validates and applies, returning when the new binary is installed
    /// (and a restart requested) or the update failed.
    pub async fn apply(
        self: &Arc<Self>,
        actor: String,
        expected: Option<&str>,
    ) -> Result<UpdateStatus, UpdateError> {
        let (release, mode) = self.prepare(expected)?;
        self.run(release, mode, actor).await;
        Ok(self.status())
    }

    fn set_phase(&self, phase: Phase) {
        self.inner.lock().unwrap().phase = phase;
    }

    async fn fail(&self, release: &Release, stage: &str, error: anyhow::Error, actor: &str) {
        let error = format!("{:#}", error);
        tracing::error!(stage, error = %error, to = %release.tag, "Update failed");
        let persisted = {
            let mut inner = self.inner.lock().unwrap();
            inner.phase = Phase::Failed;
            inner.phase_error = Some(error.clone());
            inner.persisted.pending = None;
            inner.persisted.clone()
        };
        self.save(&persisted);
        self.hooks
            .record(
                "UpdateFailed",
                json!({
                    "from_version": self.settings.current_version,
                    "to_version": release.tag,
                    "stage": stage,
                    "error": error,
                    "actor": actor,
                }),
            )
            .await;
    }

    fn set_pending(
        &self,
        release: &Release,
        method: &str,
        backup: &str,
        actor: &str,
        signed_by: Option<String>,
    ) {
        let persisted = {
            let mut inner = self.inner.lock().unwrap();
            inner.persisted.pending = Some(Pending {
                from_version: self.settings.current_version.clone(),
                to_version: release.tag.clone(),
                method: method.into(),
                backup: Some(backup.into()),
                actor: actor.into(),
                started_at: now(),
                signed_by,
            });
            inner.persisted.clone()
        };
        // Written before the swap: if the daemon dies right after, the next
        // start still reports how the update went.
        state::write_json_atomic(&self.settings.state_dir, "state.json", &persisted)
            .unwrap_or_else(|e| tracing::warn!(error = %e, "Cannot save update state"));
    }

    async fn run(self: &Arc<Self>, release: Release, mode: InstallMode, actor: String) {
        tracing::info!(to = %release.tag, mode = mode.as_str(), actor = %actor, "Starting update");
        self.hooks
            .record(
                "UpdateStarted",
                json!({
                    "from_version": self.settings.current_version,
                    "to_version": release.tag,
                    "mode": mode.as_str(),
                    "actor": actor,
                }),
            )
            .await;
        match mode {
            InstallMode::Helper => self.run_with_helper(release, actor).await,
            _ => self.run_in_place(release, actor).await,
        }
    }

    async fn run_in_place(&self, release: Release, actor: String) {
        // 1. Download, verify and stage before touching anything else.
        let Some(target) = self.settings.target else {
            return self
                .fail(
                    &release,
                    "download",
                    anyhow::anyhow!("No release for this machine"),
                    &actor,
                )
                .await;
        };
        // The dashboard never installs an unsigned release.
        let bundle = match self
            .source
            .fetch_bundle(&release, &target, SignaturePolicy::Required)
            .await
        {
            Ok(bundle) => bundle,
            Err(e) => {
                let stage = if signature::is_signature_error(&e) {
                    "signature"
                } else {
                    "download"
                };
                return self.fail(&release, stage, e, &actor).await;
            }
        };
        tracing::info!(
            to = %release.tag,
            key = bundle.signed_by.as_deref().unwrap_or("-"),
            "The release signature is valid"
        );
        let staged = match bundle.stage(&self.settings.exe) {
            Ok(staged) => staged,
            Err(e) => return self.fail(&release, "install", e, &actor).await,
        };
        if self.settings.verify_runs {
            if let Err(e) = install::check_runs(staged.daemon(), &bundle.version).await {
                return self.fail(&release, "verify", e, &actor).await;
            }
        }
        // 2. Backup.
        self.set_phase(Phase::BackingUp);
        let backup = match self.hooks.backup().await {
            Ok(name) => name,
            Err(e) => return self.fail(&release, "backup", e, &actor).await,
        };
        // 3. Swap.
        self.set_phase(Phase::Installing);
        self.set_pending(
            &release,
            "in_place",
            &backup,
            &actor,
            bundle.signed_by.clone(),
        );
        match staged.commit() {
            Ok(replaced) => tracing::info!(
                to = %release.tag,
                sha256 = %bundle.archive_sha256,
                replaced = ?replaced,
                "Installed the update; restarting"
            ),
            Err(e) => return self.fail(&release, "install", e, &actor).await,
        }
        // 4. Restart (the daemon shuts down gracefully and starts the new binary).
        self.set_phase(Phase::Restarting);
        let _ = self.restart.send(true);
    }

    async fn run_with_helper(&self, release: Release, actor: String) {
        self.set_phase(Phase::BackingUp);
        let backup = match self.hooks.backup().await {
            Ok(name) => name,
            Err(e) => return self.fail(&release, "backup", e, &actor).await,
        };
        self.set_pending(&release, "helper", &backup, &actor, None);
        if let Err(e) = helper::write_request(&self.settings.state_dir, &release.tag) {
            return self.fail(&release, "request", e, &actor).await;
        }
        self.set_phase(Phase::WaitingForHelper);
        // The helper restarts the daemon on success; we only hear back on failure.
        let deadline = tokio::time::Instant::now() + self.settings.helper_timeout;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Some(result) = helper::take_result(&self.settings.state_dir) {
                if result.ok {
                    // Installed, but we are still running: the restart failed.
                    return self
                        .fail(
                            &release,
                            "restart",
                            anyhow::anyhow!(
                                "The helper installed {} but could not restart Aegis: {}. Restart it (sudo systemctl restart aegis).",
                                release.tag,
                                result.message
                            ),
                            &actor,
                        )
                        .await;
                }
                // Only stages the helper can report; anything else is "helper".
                let stage = match result.stage.as_deref() {
                    Some("signature") => "signature",
                    _ => "helper",
                };
                return self
                    .fail(&release, stage, anyhow::anyhow!(result.message), &actor)
                    .await;
            }
        }
        let _ = std::fs::remove_file(self.settings.state_dir.join(helper::REQUEST_FILE));
        self.fail(
            &release,
            "helper",
            anyhow::anyhow!(
                "The aegis-update helper did not respond within {} minutes. Is it enabled? (sudo systemctl enable --now aegis-update.path; see journalctl -u aegis-update)",
                self.settings.helper_timeout.as_secs() / 60
            ),
            &actor,
        )
        .await
    }
}
