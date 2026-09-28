//! Builds a release in an isolated directory. Nothing here touches the live
//! release: if any stage fails, the new directory is simply left behind (or
//! removed by the caller) and the running app is unaffected.
//!
//! Stages: Clone (fetch source) -> Install -> Build -> Test -> Package
//! (content checksum) -> Verify. Activation ("Promote") is the caller's job.

use aegis_artifact_store::ArtifactStore;
use aegis_engine::ResolvedProjectConfig;
use aegis_release::Release;
use aegis_types::{DeploymentId, ProjectId};
use async_trait::async_trait;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

const LOG_TAIL_LINES: usize = 20;

/// Directories never copied from a local (non-git) source.
const LOCAL_EXCLUDES: &[&str] = &[
    ".git",
    ".aegis",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineStage {
    Clone,
    Install,
    Build,
    Test,
    Package,
    Verify,
    Promote,
}

impl PipelineStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Clone => "Clone",
            Self::Install => "Install",
            Self::Build => "Build",
            Self::Test => "Test",
            Self::Package => "Package",
            Self::Verify => "Verify",
            Self::Promote => "Promote",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageStatus {
    Started,
    Success,
    Skipped,
    Failed,
}

impl StageStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Started => "Started",
            Self::Success => "Success",
            Self::Skipped => "Skipped",
            Self::Failed => "Failed",
        }
    }
}

/// Receives stage progress. Reports are awaited, so they are delivered in order.
#[async_trait]
pub trait StageReporter: Send + Sync {
    async fn report(&self, stage: PipelineStage, status: StageStatus, detail: Option<String>);
}

/// Where the code for a release comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    /// Copy a working tree. Inside a git work tree this copies tracked and
    /// untracked-but-not-ignored files, plus `aegis.toml`.
    LocalDir(PathBuf),
    /// Clone a repository at a branch, tag or commit.
    Git { url: String, reference: String },
}

#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub project_id: ProjectId,
    pub deployment_id: DeploymentId,
    pub version: String,
    pub branch: String,
    pub source: SourceSpec,
    /// Must not exist yet; the release is assembled here.
    pub release_dir: PathBuf,
    /// Output of every command is appended here.
    pub log_path: PathBuf,
}

#[derive(Debug)]
pub struct BuildOutput {
    pub release: Release,
    pub config: ResolvedProjectConfig,
    pub duration: Duration,
}

/// A failed stage, with the last lines of output for diagnosis.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildFailure {
    pub stage: PipelineStage,
    pub message: String,
    pub log_tail: Vec<String>,
}

impl std::fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} stage failed: {}", self.stage.as_str(), self.message)
    }
}

impl std::error::Error for BuildFailure {}

pub struct BuildPipeline<'a> {
    artifact_store: &'a ArtifactStore,
    reporter: &'a dyn StageReporter,
}

struct BuildLog {
    path: PathBuf,
    tail: std::collections::VecDeque<String>,
}

impl BuildLog {
    fn new(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path,
            tail: Default::default(),
        })
    }

    fn line(&mut self, line: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{}", line);
        }
        if self.tail.len() >= LOG_TAIL_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(line.to_string());
    }

    fn tail(&self) -> Vec<String> {
        self.tail.iter().cloned().collect()
    }
}

impl<'a> BuildPipeline<'a> {
    pub fn new(artifact_store: &'a ArtifactStore, reporter: &'a dyn StageReporter) -> Self {
        Self {
            artifact_store,
            reporter,
        }
    }

    pub async fn run(&self, req: &BuildRequest) -> Result<BuildOutput, BuildFailure> {
        let started = Instant::now();
        let mut log = BuildLog::new(req.log_path.clone()).map_err(|e| BuildFailure {
            stage: PipelineStage::Clone,
            message: format!("Cannot create build log {}: {}", req.log_path.display(), e),
            log_tail: Vec::new(),
        })?;
        log.line(&format!(
            "==> Building {} (deployment {})",
            req.version, req.deployment_id
        ));

        // 1. Clone
        self.begin(PipelineStage::Clone).await;
        let fetched = fetch_source(&req.source, &req.release_dir, &mut log).await;
        let commit = match fetched {
            Ok(c) => c,
            Err(e) => return Err(self.fail(PipelineStage::Clone, e, &log).await),
        };
        let config = match ResolvedProjectConfig::load(&req.release_dir) {
            Ok(c) => c,
            Err(e) => {
                return Err(self
                    .fail(
                        PipelineStage::Clone,
                        anyhow::anyhow!("Invalid aegis.toml: {}", e),
                        &log,
                    )
                    .await)
            }
        };
        self.succeed(PipelineStage::Clone, Some(commit.sha.clone()))
            .await;

        let env: HashMap<String, String> = config
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .chain([("AEGIS_RELEASE_VERSION".to_string(), req.version.clone())])
            .collect();

        // 2-4. Install, Build, Test
        for (stage, command) in [
            (PipelineStage::Install, &config.install_command),
            (PipelineStage::Build, &config.build_command),
            (PipelineStage::Test, &config.test_command),
        ] {
            let Some(command) = command else {
                self.reporter
                    .report(
                        stage,
                        StageStatus::Skipped,
                        Some("No command configured".into()),
                    )
                    .await;
                continue;
            };
            self.begin(stage).await;
            log.line(&format!("==> {}: {}", stage.as_str(), command));
            if let Err(e) = run_command(
                command,
                &req.release_dir,
                &env,
                config.build_timeout,
                &mut log,
            )
            .await
            {
                return Err(self.fail(stage, e, &log).await);
            }
            self.succeed(stage, None).await;
        }

        // 5. Package
        self.begin(PipelineStage::Package).await;
        let mut release = Release::new(
            req.project_id,
            req.version.clone(),
            commit.sha,
            commit.message,
            commit.author,
            req.branch.clone(),
            config.runtime.clone(),
        );
        release.build_metadata.build_command = config.build_command.clone();
        release.build_metadata.tests_passed = config.test_command.as_ref().map(|_| true);
        let artifact = match self
            .artifact_store
            .register(&release.id, &req.release_dir)
            .await
        {
            Ok(a) => a,
            Err(e) => return Err(self.fail(PipelineStage::Package, e, &log).await),
        };
        release.artifacts.push(artifact.id);
        release
            .checksums
            .insert(artifact.name.clone(), artifact.sha256_checksum.clone());
        self.succeed(
            PipelineStage::Package,
            Some(format!("sha256:{}", artifact.sha256_checksum)),
        )
        .await;

        // 6. Verify
        self.begin(PipelineStage::Verify).await;
        match self.artifact_store.verify(&release.id).await {
            Ok(true) => {}
            Ok(false) => {
                return Err(self
                    .fail(
                        PipelineStage::Verify,
                        anyhow::anyhow!("Artifact for release {} is missing", release.id),
                        &log,
                    )
                    .await)
            }
            Err(e) => return Err(self.fail(PipelineStage::Verify, e, &log).await),
        }
        self.succeed(PipelineStage::Verify, None).await;

        let duration = started.elapsed();
        release.build_metadata.build_duration_ms = duration.as_millis() as u64;
        log.line(&format!(
            "==> Build finished in {:.1}s",
            duration.as_secs_f64()
        ));
        Ok(BuildOutput {
            release,
            config,
            duration,
        })
    }

    async fn begin(&self, stage: PipelineStage) {
        tracing::info!(stage = stage.as_str(), "Build stage started");
        self.reporter
            .report(stage, StageStatus::Started, None)
            .await;
    }

    async fn succeed(&self, stage: PipelineStage, detail: Option<String>) {
        self.reporter
            .report(stage, StageStatus::Success, detail)
            .await;
    }

    async fn fail(
        &self,
        stage: PipelineStage,
        error: anyhow::Error,
        log: &BuildLog,
    ) -> BuildFailure {
        let message = error.to_string();
        tracing::warn!(stage = stage.as_str(), error = %message, "Build stage failed");
        self.reporter
            .report(stage, StageStatus::Failed, Some(message.clone()))
            .await;
        BuildFailure {
            stage,
            message,
            log_tail: log.tail(),
        }
    }
}

struct CommitInfo {
    sha: String,
    message: String,
    author: String,
}

async fn git_output(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

async fn commit_info(dir: &Path, fallback_sha: &str) -> CommitInfo {
    let sha = git_output(dir, &["rev-parse", "HEAD"])
        .await
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_sha.to_string());
    let (message, author) = match git_output(dir, &["log", "-1", "--format=%s%n%an"]).await {
        Some(out) => {
            let mut lines = out.lines();
            (
                lines.next().unwrap_or("").to_string(),
                lines.next().unwrap_or("").to_string(),
            )
        }
        None => (String::new(), String::new()),
    };
    CommitInfo {
        sha,
        message,
        author,
    }
}

async fn fetch_source(
    source: &SourceSpec,
    dest: &Path,
    log: &mut BuildLog,
) -> Result<CommitInfo, anyhow::Error> {
    if dest.exists() {
        anyhow::bail!(
            "Release directory {} already exists; choose a different version",
            dest.display()
        );
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    match source {
        SourceSpec::Git { url, reference } => {
            log.line(&format!("==> Clone: {} @ {}", url, reference));
            let is_commit = reference.len() >= 7
                && reference.len() <= 40
                && reference.chars().all(|c| c.is_ascii_hexdigit());
            let dest_str = dest.to_string_lossy().to_string();
            let env = HashMap::from([("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())]);
            let timeout = Duration::from_secs(600);
            let cwd = dest.parent().unwrap_or(Path::new("."));
            if is_commit {
                run_argv("git", &["clone", url, &dest_str], cwd, &env, timeout, log).await?;
                run_argv(
                    "git",
                    &["checkout", "--detach", reference],
                    dest,
                    &env,
                    timeout,
                    log,
                )
                .await?;
            } else {
                run_argv(
                    "git",
                    &[
                        "clone", "--depth", "1", "--branch", reference, url, &dest_str,
                    ],
                    cwd,
                    &env,
                    timeout,
                    log,
                )
                .await?;
            }
            Ok(commit_info(dest, reference).await)
        }
        SourceSpec::LocalDir(src) => {
            if !src.is_dir() {
                anyhow::bail!("Source directory {} does not exist", src.display());
            }
            log.line(&format!("==> Copy: {}", src.display()));
            let info = commit_info(src, "local").await;
            let files = git_file_list(src).await;
            let (src, dest_owned) = (src.clone(), dest.to_path_buf());
            let copied = tokio::task::spawn_blocking(move || match files {
                Some(files) => copy_listed(&src, &dest_owned, &files),
                None => copy_tree_excluding(&src, &dest_owned, &src),
            })
            .await??;
            log.line(&format!("Copied {} files", copied));
            Ok(info)
        }
    }
}

/// Files git considers part of the working tree (tracked plus untracked,
/// minus ignored), relative to `dir`. `None` when `dir` isn't in a git repo.
async fn git_file_list(dir: &Path) -> Option<Vec<PathBuf>> {
    let inside = git_output(dir, &["rev-parse", "--is-inside-work-tree"]).await?;
    if inside.trim() != "true" {
        return None;
    }
    let output = git_output(
        dir,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )
    .await?;
    let mut files: Vec<PathBuf> = output
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    // aegis.toml is usually gitignored but always belongs to the release.
    files.push(PathBuf::from("aegis.toml"));
    files.sort();
    files.dedup();
    Some(files)
}

fn copy_entry(src: &Path, dest: &Path) -> std::io::Result<bool> {
    let meta = match std::fs::symlink_metadata(src) {
        Ok(m) => m,
        // Deleted in the working tree but still tracked.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::fs::read_link(src)?, dest)?;
            return Ok(true);
        }
        #[cfg(not(unix))]
        return Ok(false);
    }
    if meta.is_file() {
        std::fs::copy(src, dest)?;
        return Ok(true);
    }
    Ok(false)
}

fn copy_listed(src: &Path, dest: &Path, files: &[PathBuf]) -> Result<usize, anyhow::Error> {
    std::fs::create_dir_all(dest)?;
    let mut copied = 0;
    for rel in files {
        if copy_entry(&src.join(rel), &dest.join(rel))? {
            copied += 1;
        }
    }
    Ok(copied)
}

fn copy_tree_excluding(src: &Path, dest: &Path, root: &Path) -> Result<usize, anyhow::Error> {
    std::fs::create_dir_all(dest)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if LOCAL_EXCLUDES.contains(&name.as_str())
            || (src == root && (name.ends_with(".db") || name.contains(".db-")))
        {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copied += copy_tree_excluding(&entry.path(), &dest.join(&name), root)?;
        } else if copy_entry(&entry.path(), &dest.join(&name))? {
            copied += 1;
        }
    }
    Ok(copied)
}

async fn run_command(
    command: &str,
    cwd: &Path,
    env: &HashMap<String, String>,
    timeout: Duration,
    log: &mut BuildLog,
) -> Result<(), anyhow::Error> {
    #[cfg(unix)]
    let (program, args) = ("sh", vec!["-c", command]);
    #[cfg(not(unix))]
    let (program, args) = ("cmd", vec!["/C", command]);
    run_argv(program, &args, cwd, env, timeout, log).await
}

async fn run_argv(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &HashMap<String, String>,
    timeout: Duration,
    log: &mut BuildLog,
) -> Result<(), anyhow::Error> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run {}: {}", program, e))?;
    let pid = child.id();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    for reader in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut buf = Vec::new();
            while let Ok(n) = reader.read_until(b'\n', &mut buf).await {
                if n == 0 {
                    break;
                }
                let line = String::from_utf8_lossy(&buf)
                    .trim_end_matches(['\n', '\r'])
                    .to_string();
                let _ = tx.send(line);
                buf.clear();
            }
        });
    }
    drop(tx);

    let deadline = tokio::time::Instant::now() + timeout;
    let mut output_open = true;
    let status = loop {
        tokio::select! {
            line = rx.recv(), if output_open => match line {
                Some(line) => log.line(&line),
                None => output_open = false,
            },
            status = child.wait() => break status?,
            _ = tokio::time::sleep_until(deadline) => {
                #[cfg(unix)]
                if let Some(pid) = pid {
                    unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
                }
                #[cfg(not(unix))]
                if let Some(pid) = pid {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/T", "/F", "/PID", &pid.to_string()])
                        .output();
                }
                let _ = child.kill().await;
                anyhow::bail!("Timed out after {}s", timeout.as_secs());
            }
        }
    };
    // Drain whatever output is still buffered.
    while let Ok(Some(line)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        log.line(&line);
    }
    #[cfg(unix)]
    if let Some(pid) = pid {
        // Don't leave background processes from the build running.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    if !status.success() {
        anyhow::bail!(
            "Command exited with {}",
            status
                .code()
                .map_or("a signal".to_string(), |c| format!("code {}", c))
        );
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<(PipelineStage, StageStatus)>>);

    #[async_trait]
    impl StageReporter for Recorder {
        async fn report(&self, stage: PipelineStage, status: StageStatus, _d: Option<String>) {
            self.0.lock().unwrap().push((stage, status));
        }
    }

    fn request(source: SourceSpec, root: &Path) -> BuildRequest {
        BuildRequest {
            project_id: ProjectId::new(),
            deployment_id: DeploymentId::new(),
            version: "r1".to_string(),
            branch: "main".to_string(),
            source,
            release_dir: root.join("releases/r1"),
            log_path: root.join("logs/build.log"),
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(status.status.success(), "git {:?}: {:?}", args, status);
    }

    #[tokio::test]
    async fn test_local_build_runs_configured_commands() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(
            src.path().join("aegis.toml"),
            r#"
[build]
install_command = "echo installed > installed.txt"
build_command = "echo hello-from-build; echo $GREETING > built.txt"
test_command = "test -f built.txt"
start_command = "sleep 1"
[env]
GREETING = "hi"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(src.path().join("node_modules/pkg")).unwrap();
        std::fs::write(src.path().join("node_modules/pkg/x.js"), "x").unwrap();
        std::fs::write(src.path().join("app.db"), "db").unwrap();

        let store = ArtifactStore::new(out.path().join("artifacts"));
        let recorder = Recorder::default();
        let req = request(SourceSpec::LocalDir(src.path().to_path_buf()), out.path());
        let output = BuildPipeline::new(&store, &recorder)
            .run(&req)
            .await
            .unwrap();

        let dir = &req.release_dir;
        assert!(dir.join("aegis.toml").exists());
        assert!(dir.join("installed.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("built.txt")).unwrap(),
            "hi\n"
        );
        assert!(!dir.join("node_modules").exists());
        assert!(!dir.join("app.db").exists());
        assert_eq!(output.release.commit_sha, "local");
        assert_eq!(output.config.start_command, "sleep 1");
        assert!(store.verify_checksum(&output.release.id).await.unwrap());
        let log = std::fs::read_to_string(&req.log_path).unwrap();
        assert!(log.contains("hello-from-build"));

        let reports = recorder.0.lock().unwrap().clone();
        let succeeded: Vec<_> = reports
            .iter()
            .filter(|(_, s)| *s == StageStatus::Success)
            .map(|(st, _)| *st)
            .collect();
        assert_eq!(
            succeeded,
            vec![
                PipelineStage::Clone,
                PipelineStage::Install,
                PipelineStage::Build,
                PipelineStage::Test,
                PipelineStage::Package,
                PipelineStage::Verify
            ]
        );
    }

    #[tokio::test]
    async fn test_failed_build_reports_stage_and_output() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(
            src.path().join("aegis.toml"),
            "[build]\nbuild_command = \"echo boom; exit 2\"\nstart_command = \"true\"\n",
        )
        .unwrap();
        let store = ArtifactStore::new(out.path().join("artifacts"));
        let recorder = Recorder::default();
        let req = request(SourceSpec::LocalDir(src.path().to_path_buf()), out.path());
        let err = BuildPipeline::new(&store, &recorder)
            .run(&req)
            .await
            .unwrap_err();
        assert_eq!(err.stage, PipelineStage::Build);
        assert!(err.message.contains("code 2"), "{}", err.message);
        assert!(err.log_tail.iter().any(|l| l == "boom"));
        assert!(recorder
            .0
            .lock()
            .unwrap()
            .contains(&(PipelineStage::Build, StageStatus::Failed)));

        // Re-using a version that already exists is refused.
        let err = BuildPipeline::new(&store, &recorder)
            .run(&req)
            .await
            .unwrap_err();
        assert_eq!(err.stage, PipelineStage::Clone);
        assert!(err.message.contains("already exists"));
    }

    #[tokio::test]
    async fn test_build_timeout() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        std::fs::write(
            src.path().join("aegis.toml"),
            "[build]\nbuild_command = \"sleep 30\"\nstart_command = \"true\"\ntimeout_secs = 1\n",
        )
        .unwrap();
        let store = ArtifactStore::new(out.path().join("artifacts"));
        let recorder = Recorder::default();
        let req = request(SourceSpec::LocalDir(src.path().to_path_buf()), out.path());
        let started = Instant::now();
        let err = BuildPipeline::new(&store, &recorder)
            .run(&req)
            .await
            .unwrap_err();
        assert!(err.message.contains("Timed out"));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn test_git_aware_local_copy_and_git_clone() {
        let repo = TempDir::new().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join(".gitignore"), "aegis.toml\nsecret.txt\n").unwrap();
        std::fs::write(repo.path().join("index.js"), "tracked").unwrap();
        std::fs::write(
            repo.path().join("aegis.toml"),
            "[build]\nbuild_command = \"\"\nstart_command = \"true\"\n",
        )
        .unwrap();
        git(repo.path(), &["add", ".gitignore", "index.js"]);
        git(repo.path(), &["commit", "-q", "-m", "first commit"]);
        std::fs::write(repo.path().join("secret.txt"), "ignored").unwrap();
        std::fs::write(repo.path().join("new.js"), "untracked").unwrap();

        let out = TempDir::new().unwrap();
        let store = ArtifactStore::new(out.path().join("artifacts"));
        let recorder = Recorder::default();
        let req = request(SourceSpec::LocalDir(repo.path().to_path_buf()), out.path());
        let output = BuildPipeline::new(&store, &recorder)
            .run(&req)
            .await
            .unwrap();
        let dir = &req.release_dir;
        assert!(dir.join("index.js").exists());
        assert!(dir.join("new.js").exists());
        assert!(dir.join("aegis.toml").exists());
        assert!(!dir.join("secret.txt").exists());
        assert!(!dir.join(".git").exists());
        assert_eq!(output.release.commit_sha.len(), 40);
        assert_eq!(output.release.commit_message, "first commit");
        assert_eq!(output.release.author, "Test");

        // Clone from the repository by branch.
        let mut req2 = request(
            SourceSpec::Git {
                url: repo.path().to_string_lossy().to_string(),
                reference: "main".to_string(),
            },
            out.path(),
        );
        req2.release_dir = out.path().join("releases/r2");
        let output2 = BuildPipeline::new(&store, &recorder)
            .run(&req2)
            .await
            .unwrap();
        assert!(req2.release_dir.join("index.js").exists());
        assert!(!req2.release_dir.join("new.js").exists());
        assert_eq!(output2.release.commit_sha, output.release.commit_sha);

        // A missing branch fails in the Clone stage.
        let mut req3 = req2.clone();
        req3.release_dir = out.path().join("releases/r3");
        req3.source = SourceSpec::Git {
            url: repo.path().to_string_lossy().to_string(),
            reference: "does-not-exist".to_string(),
        };
        let err = BuildPipeline::new(&store, &recorder)
            .run(&req3)
            .await
            .unwrap_err();
        assert_eq!(err.stage, PipelineStage::Clone);
    }
}
