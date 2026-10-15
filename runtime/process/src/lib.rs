//! Process supervisor: runs app processes, keeps them alive according to a
//! restart policy, stops them gracefully, and captures their output.
//!
//! Every app runs as `sh -c <command>` in its own process group, so stopping
//! an app also stops whatever it spawned (`npm start` -> `node`, etc.).

use aegis_types::{ProcessId, ProjectId};
use chrono::{DateTime, SecondsFormat, Utc};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, oneshot, watch};
use tokio::task::JoinHandle;

const MAX_LINE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Always,
    OnFailure,
    Never,
}

impl RestartPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().replace('_', "-").as_str() {
            "always" => Some(Self::Always),
            "on-failure" | "onfailure" => Some(Self::OnFailure),
            "never" | "no" => Some(Self::Never),
            _ => None,
        }
    }
}

/// Everything needed to (re)start a process.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub project_id: ProjectId,
    /// Shell command line, run via `sh -c` (`cmd /C` on Windows).
    pub command: String,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub restart_policy: RestartPolicy,
    /// Where stdout/stderr are appended. `None` keeps logs in memory only.
    pub log_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    /// Exited and waiting to be restarted.
    Backoff,
    Stopping,
    /// Stopped on request.
    Stopped,
    /// Exited on its own and the restart policy says not to restart.
    Exited,
    /// Could not be started, or crashed too often.
    Failed,
}

impl ProcessStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Backoff => "Backoff",
            Self::Stopping => "Stopping",
            Self::Stopped => "Stopped",
            Self::Exited => "Exited",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// An operator asked for it.
    Requested,
    /// Part of a restart; the process comes straight back.
    Restart,
    /// A new release took over.
    Replaced,
    /// The daemon is shutting down; the process should come back on boot.
    Shutdown,
}

impl StopReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Restart => "restart",
            Self::Replaced => "replaced",
            Self::Shutdown => "shutdown",
        }
    }
}

/// Lets another component (resource limits) prepare each process before it
/// starts, e.g. by creating a cgroup for it.
pub trait SpawnHook: Send + Sync {
    /// Called before every spawn (including automatic restarts). Returns
    /// files (`cgroup.procs`) the child joins by writing its own pid before
    /// running the command, so everything it forks is covered too. Problems
    /// are reported as messages for the process's log; the process starts
    /// anyway.
    fn prepare(&self, process_id: &ProcessId, project_id: &ProjectId) -> Placement;
    /// The process is gone for good (stopped, failed, or exited without a
    /// restart).
    fn finished(&self, process_id: &ProcessId);
}

#[derive(Debug, Default, Clone)]
pub struct Placement {
    pub join_files: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub id: ProcessId,
    pub project_id: ProjectId,
    pub command: String,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    pub status: ProcessStatus,
    pub restart_count: u32,
    pub last_exit_code: Option<i32>,
    pub started_at: Option<DateTime<Utc>>,
}

/// Lifecycle notifications, in the order they happen for a given process.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessEvent {
    Started {
        id: ProcessId,
        project_id: ProjectId,
        pid: Option<u32>,
        restart_count: u32,
    },
    /// The process exited without being asked to.
    Exited {
        id: ProcessId,
        project_id: ProjectId,
        exit_code: Option<i32>,
        will_restart: bool,
        restart_delay: Duration,
    },
    /// Crash loop or spawn failure; the supervisor has given up.
    Failed {
        id: ProcessId,
        project_id: ProjectId,
        reason: String,
    },
    Stopped {
        id: ProcessId,
        project_id: ProjectId,
        reason: StopReason,
        exit_code: Option<i32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
    /// Lines written by the supervisor itself (exits, restarts, stops).
    System,
}

impl LogStream {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stdout => "out",
            Self::Stderr => "err",
            Self::System => "sys",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "out" => Some(Self::Stdout),
            "err" => Some(Self::Stderr),
            "sys" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub timestamp: DateTime<Utc>,
    pub stream: LogStream,
    pub line: String,
}

impl LogLine {
    fn now(stream: LogStream, line: String) -> Self {
        Self {
            timestamp: Utc::now(),
            stream,
            line,
        }
    }

    fn to_file_line(&self) -> String {
        format!(
            "{} {} {}\n",
            self.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
            self.stream.as_str(),
            self.line
        )
    }

    fn from_file_line(raw: &str) -> Option<Self> {
        let mut parts = raw.splitn(3, ' ');
        let timestamp = DateTime::parse_from_rfc3339(parts.next()?)
            .ok()?
            .with_timezone(&Utc);
        let stream = LogStream::parse(parts.next()?)?;
        Some(Self {
            timestamp,
            stream,
            line: parts.next().unwrap_or("").to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    /// A run at least this long resets the backoff and crash counter.
    pub stable_after: Duration,
    /// More than this many restarts within `crash_loop_window` marks the process Failed.
    pub crash_loop_max_restarts: usize,
    pub crash_loop_window: Duration,
    pub log_buffer_lines: usize,
    pub max_log_file_bytes: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            stable_after: Duration::from_secs(30),
            crash_loop_max_restarts: 5,
            crash_loop_window: Duration::from_secs(120),
            log_buffer_lines: 1000,
            max_log_file_bytes: 10 * 1024 * 1024,
        }
    }
}

struct ProcState {
    pid: Option<u32>,
    status: ProcessStatus,
    restart_count: u32,
    last_exit_code: Option<i32>,
    started_at: Option<DateTime<Utc>>,
}

type StopSignal = Option<(StopReason, Duration)>;

struct Control {
    stop_tx: watch::Sender<StopSignal>,
    task: JoinHandle<()>,
}

struct LogFile {
    path: PathBuf,
    file: Option<std::fs::File>,
    size: u64,
}

struct Managed {
    id: ProcessId,
    spec: Mutex<ProcessSpec>,
    state: Mutex<ProcState>,
    control: Mutex<Option<Control>>,
    ring: Mutex<VecDeque<LogLine>>,
    log_file: Mutex<Option<LogFile>>,
    log_tx: broadcast::Sender<LogLine>,
}

impl Managed {
    fn info(&self) -> ProcessInfo {
        let spec = self.spec.lock().unwrap();
        let state = self.state.lock().unwrap();
        ProcessInfo {
            id: self.id,
            project_id: spec.project_id,
            command: spec.command.clone(),
            cwd: spec.cwd.clone(),
            pid: state.pid,
            status: state.status,
            restart_count: state.restart_count,
            last_exit_code: state.last_exit_code,
            started_at: state.started_at,
        }
    }

    fn set_status(&self, status: ProcessStatus) {
        let mut state = self.state.lock().unwrap();
        state.status = status;
        if !matches!(status, ProcessStatus::Running | ProcessStatus::Stopping) {
            state.pid = None;
        }
    }

    fn push_log(&self, entry: LogLine, max_ring: usize, max_file_bytes: u64) {
        {
            let mut ring = self.ring.lock().unwrap();
            if ring.len() >= max_ring {
                ring.pop_front();
            }
            ring.push_back(entry.clone());
        }
        if let Some(log) = self.log_file.lock().unwrap().as_mut() {
            write_log_line(log, &entry, max_file_bytes);
        }
        let _ = self.log_tx.send(entry);
    }
}

fn write_log_line(log: &mut LogFile, entry: &LogLine, max_bytes: u64) {
    if log.size >= max_bytes {
        log.file = None;
        let rotated = log.path.with_extension("log.1");
        let _ = std::fs::rename(&log.path, rotated);
        log.size = 0;
    }
    if log.file.is_none() {
        if let Some(parent) = log.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        log.file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log.path)
            .ok();
        log.size = std::fs::metadata(&log.path).map(|m| m.len()).unwrap_or(0);
    }
    if let Some(file) = log.file.as_mut() {
        let line = entry.to_file_line();
        if file.write_all(line.as_bytes()).is_ok() {
            log.size += line.len() as u64;
        }
    }
}

/// Reads the last `n` lines from a log file written by the supervisor.
pub fn read_log_tail(path: &Path, n: usize) -> Vec<LogLine> {
    const WINDOW: u64 = 1024 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(WINDOW);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // The first line is probably cut in half.
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(n);
    lines
        .into_iter()
        .skip(skip)
        .filter_map(LogLine::from_file_line)
        .collect()
}

struct Inner {
    processes: Mutex<HashMap<ProcessId, Arc<Managed>>>,
    events: broadcast::Sender<ProcessEvent>,
    config: SupervisorConfig,
    hook: OnceLock<Arc<dyn SpawnHook>>,
}

impl Inner {
    fn emit(&self, event: ProcessEvent) {
        let _ = self.events.send(event);
    }

    fn system_log(&self, managed: &Managed, message: String) {
        managed.push_log(
            LogLine::now(LogStream::System, message),
            self.config.log_buffer_lines,
            self.config.max_log_file_bytes,
        );
    }
}

#[derive(Clone)]
pub struct ProcessSupervisor {
    inner: Arc<Inner>,
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSupervisor {
    pub fn new() -> Self {
        Self::with_config(SupervisorConfig::default())
    }

    pub fn with_config(config: SupervisorConfig) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(Inner {
                processes: Mutex::new(HashMap::new()),
                events,
                config,
                hook: OnceLock::new(),
            }),
        }
    }

    /// Installs the hook that prepares each process before it starts. Only
    /// the first call has an effect.
    pub fn set_spawn_hook(&self, hook: Arc<dyn SpawnHook>) {
        let _ = self.inner.hook.set(hook);
    }

    /// Lifecycle events for every supervised process.
    pub fn subscribe(&self) -> broadcast::Receiver<ProcessEvent> {
        self.inner.events.subscribe()
    }

    /// Starts a new process and returns its id once it has been spawned.
    pub async fn spawn(&self, spec: ProcessSpec) -> Result<ProcessId, anyhow::Error> {
        let id = ProcessId::new();
        self.start_with_id(id, spec).await?;
        Ok(id)
    }

    /// Starts (or starts again) the process with a known id, e.g. after a
    /// daemon restart or a manual stop. Fails if it is already running.
    pub async fn start_with_id(
        &self,
        id: ProcessId,
        spec: ProcessSpec,
    ) -> Result<Option<u32>, anyhow::Error> {
        let managed = {
            let mut processes = self.inner.processes.lock().unwrap();
            match processes.get(&id) {
                Some(existing) => {
                    if existing.control.lock().unwrap().is_some() {
                        anyhow::bail!("Process {} is already running", id);
                    }
                    *existing.spec.lock().unwrap() = spec.clone();
                    *existing.log_file.lock().unwrap() =
                        spec.log_file.clone().map(|path| LogFile {
                            path,
                            file: None,
                            size: 0,
                        });
                    existing.clone()
                }
                None => {
                    let (log_tx, _) = broadcast::channel(1024);
                    let managed = Arc::new(Managed {
                        id,
                        log_file: Mutex::new(spec.log_file.clone().map(|path| LogFile {
                            path,
                            file: None,
                            size: 0,
                        })),
                        spec: Mutex::new(spec.clone()),
                        state: Mutex::new(ProcState {
                            pid: None,
                            status: ProcessStatus::Stopped,
                            restart_count: 0,
                            last_exit_code: None,
                            started_at: None,
                        }),
                        control: Mutex::new(None),
                        ring: Mutex::new(VecDeque::new()),
                        log_tx,
                    });
                    processes.insert(id, managed.clone());
                    managed
                }
            }
        };

        let (stop_tx, stop_rx) = watch::channel(None);
        let (first_tx, first_rx) = oneshot::channel();
        let inner = self.inner.clone();
        let for_monitor = managed.clone();
        let task = tokio::spawn(async move {
            monitor(inner.clone(), for_monitor, stop_rx, first_tx).await;
            if let Some(hook) = inner.hook.get() {
                hook.finished(&id);
            }
        });
        *managed.control.lock().unwrap() = Some(Control { stop_tx, task });

        match first_rx.await {
            Ok(Ok(pid)) => Ok(pid),
            Ok(Err(e)) => {
                managed.control.lock().unwrap().take();
                Err(anyhow::anyhow!(e))
            }
            Err(_) => {
                managed.control.lock().unwrap().take();
                anyhow::bail!("Process monitor for {} ended unexpectedly", id)
            }
        }
    }

    /// Stops a process: SIGTERM to its process group, then SIGKILL once the
    /// drain timeout has passed. Returns once the process is gone.
    pub async fn stop(
        &self,
        id: &ProcessId,
        drain: Duration,
        reason: StopReason,
    ) -> Result<(), anyhow::Error> {
        let managed = self
            .get_managed(id)
            .ok_or_else(|| anyhow::anyhow!("Unknown process {}", id))?;
        let control = managed.control.lock().unwrap().take();
        let Some(control) = control else {
            return Ok(());
        };
        let _ = control.stop_tx.send(Some((reason, drain)));
        let wait = drain + Duration::from_secs(10);
        if tokio::time::timeout(wait, control.task).await.is_err() {
            anyhow::bail!("Timed out waiting for process {} to stop", id);
        }
        Ok(())
    }

    /// Stops the process and starts it again with the same spec and id.
    pub async fn restart(&self, id: &ProcessId, drain: Duration) -> Result<(), anyhow::Error> {
        let managed = self
            .get_managed(id)
            .ok_or_else(|| anyhow::anyhow!("Unknown process {}", id))?;
        self.stop(id, drain, StopReason::Restart).await?;
        managed.state.lock().unwrap().restart_count += 1;
        let spec = managed.spec.lock().unwrap().clone();
        self.start_with_id(*id, spec).await?;
        Ok(())
    }

    /// Stops every running process concurrently.
    pub async fn stop_all(&self, drain: Duration, reason: StopReason) {
        let ids: Vec<ProcessId> = self
            .inner
            .processes
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect();
        let mut set = tokio::task::JoinSet::new();
        for id in ids {
            let this = self.clone();
            set.spawn(async move {
                if let Err(e) = this.stop(&id, drain, reason).await {
                    tracing::warn!(process_id = %id, error = %e, "Failed to stop process");
                }
            });
        }
        while set.join_next().await.is_some() {}
    }

    pub fn get(&self, id: &ProcessId) -> Option<ProcessInfo> {
        self.get_managed(id).map(|m| m.info())
    }

    pub fn list_processes(&self) -> Vec<ProcessInfo> {
        self.inner
            .processes
            .lock()
            .unwrap()
            .values()
            .map(|m| m.info())
            .collect()
    }

    /// The last `n` log lines, from the log file when there is one.
    pub fn logs(&self, id: &ProcessId, n: usize) -> Vec<LogLine> {
        let Some(managed) = self.get_managed(id) else {
            return Vec::new();
        };
        let path = managed.spec.lock().unwrap().log_file.clone();
        match path {
            Some(path) => read_log_tail(&path, n),
            None => {
                let ring = managed.ring.lock().unwrap();
                let skip = ring.len().saturating_sub(n);
                ring.iter().skip(skip).cloned().collect()
            }
        }
    }

    /// Live log lines for a process, or `None` if the supervisor doesn't know it.
    pub fn subscribe_logs(&self, id: &ProcessId) -> Option<broadcast::Receiver<LogLine>> {
        self.get_managed(id).map(|m| m.log_tx.subscribe())
    }

    fn get_managed(&self, id: &ProcessId) -> Option<Arc<Managed>> {
        self.inner.processes.lock().unwrap().get(id).cloned()
    }
}

fn spawn_child(spec: &ProcessSpec, join_files: &[PathBuf]) -> std::io::Result<Child> {
    #[cfg(unix)]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&spec.command);
        cmd.process_group(0);
        if !join_files.is_empty() {
            use std::os::unix::ffi::OsStrExt;
            let paths: Vec<std::ffi::CString> = join_files
                .iter()
                .filter_map(|p| std::ffi::CString::new(p.as_os_str().as_bytes()).ok())
                .collect();
            // Runs in the child between fork and exec, so it may only make
            // async-signal-safe calls: open/write/close on prepared paths.
            // Failures are ignored here; the parent checks placement after.
            unsafe {
                cmd.pre_exec(move || {
                    for path in &paths {
                        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                        if fd >= 0 {
                            libc::write(fd, b"0".as_ptr().cast(), 1);
                            libc::close(fd);
                        }
                    }
                    Ok(())
                });
            }
        }
        cmd
    };
    #[cfg(not(unix))]
    let _ = join_files;
    #[cfg(not(unix))]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(&spec.command);
        cmd
    };
    cmd.current_dir(&spec.cwd)
        .envs(&spec.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd.spawn()
}

#[cfg(unix)]
fn signal_group(pid: Option<u32>, signal: i32) {
    if let Some(pid) = pid {
        // The child is its own process group leader (process_group(0)), so
        // -pid addresses it and everything it spawned.
        unsafe {
            libc::kill(-(pid as i32), signal);
        }
    }
}

async fn terminate(child: &mut Child, pid: Option<u32>, drain: Duration) -> Option<i32> {
    #[cfg(unix)]
    {
        signal_group(pid, libc::SIGTERM);
        let status = match tokio::time::timeout(drain, child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => {
                signal_group(pid, libc::SIGKILL);
                child.wait().await.ok()
            }
        };
        // Clean up anything left in the group after the leader exited.
        signal_group(pid, libc::SIGKILL);
        status.and_then(|s| s.code())
    }
    #[cfg(not(unix))]
    {
        if let Some(pid) = pid {
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/PID", &pid.to_string()])
                .output();
        }
        let status = match tokio::time::timeout(drain, child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => {
                if let Some(pid) = pid {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/T", "/F", "/PID", &pid.to_string()])
                        .output();
                }
                let _ = child.kill().await;
                child.wait().await.ok()
            }
        };
        status.and_then(|s| s.code())
    }
}

fn pipe_output<R>(inner: Arc<Inner>, managed: Arc<Managed>, reader: R, stream: LogStream)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    buf.truncate(MAX_LINE_BYTES);
                    let line = String::from_utf8_lossy(&buf)
                        .trim_end_matches(['\n', '\r'])
                        .to_string();
                    managed.push_log(
                        LogLine::now(stream, line),
                        inner.config.log_buffer_lines,
                        inner.config.max_log_file_bytes,
                    );
                }
            }
        }
    });
}

enum Wake {
    Exited(Option<std::process::ExitStatus>),
    Stop(StopReason, Duration),
}

async fn wait_for_stop(stop_rx: &mut watch::Receiver<StopSignal>) -> (StopReason, Duration) {
    loop {
        if stop_rx.changed().await.is_err() {
            // The supervisor handle was dropped; treat it as shutdown.
            return (StopReason::Shutdown, Duration::from_secs(0));
        }
        if let Some(signal) = *stop_rx.borrow() {
            return signal;
        }
    }
}

async fn monitor(
    inner: Arc<Inner>,
    managed: Arc<Managed>,
    mut stop_rx: watch::Receiver<StopSignal>,
    first_tx: oneshot::Sender<Result<Option<u32>, String>>,
) {
    let config = inner.config.clone();
    let id = managed.id;
    let mut first_tx = Some(first_tx);
    let mut backoff = config.backoff_initial;
    let mut recent_restarts: VecDeque<Instant> = VecDeque::new();

    loop {
        let spec = managed.spec.lock().unwrap().clone();
        let project_id = spec.project_id;
        let placement = inner
            .hook
            .get()
            .map(|hook| hook.prepare(&id, &project_id))
            .unwrap_or_default();
        for warning in &placement.warnings {
            inner.system_log(&managed, warning.clone());
        }
        let mut child = match spawn_child(&spec, &placement.join_files) {
            Ok(child) => child,
            Err(e) => {
                let reason = format!("Failed to spawn '{}': {}", spec.command, e);
                managed.set_status(ProcessStatus::Failed);
                inner.system_log(&managed, reason.clone());
                match first_tx.take() {
                    Some(tx) => {
                        let _ = tx.send(Err(reason));
                    }
                    None => {
                        managed.control.lock().unwrap().take();
                        inner.emit(ProcessEvent::Failed {
                            id,
                            project_id,
                            reason,
                        });
                    }
                }
                return;
            }
        };

        let pid = child.id();
        // The child joined its cgroups itself; make sure it worked (writing
        // the pid again is harmless) so a missing limit never goes unnoticed.
        if let Some(pid) = pid {
            for file in &placement.join_files {
                if let Err(e) = std::fs::write(file, pid.to_string()) {
                    inner.system_log(
                        &managed,
                        format!(
                            "Could not place the process in {}: {} (resource limits do not apply)",
                            file.display(),
                            e
                        ),
                    );
                }
            }
        }
        let restart_count = {
            let mut state = managed.state.lock().unwrap();
            state.pid = pid;
            state.status = ProcessStatus::Running;
            state.started_at = Some(Utc::now());
            state.restart_count
        };
        if let Some(stdout) = child.stdout.take() {
            pipe_output(inner.clone(), managed.clone(), stdout, LogStream::Stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            pipe_output(inner.clone(), managed.clone(), stderr, LogStream::Stderr);
        }
        tracing::info!(process_id = %id, pid = ?pid, command = %spec.command, "Process started");
        inner.emit(ProcessEvent::Started {
            id,
            project_id,
            pid,
            restart_count,
        });
        if let Some(tx) = first_tx.take() {
            let _ = tx.send(Ok(pid));
        }

        let started = Instant::now();
        let wake = tokio::select! {
            status = child.wait() => Wake::Exited(status.ok()),
            (reason, drain) = wait_for_stop(&mut stop_rx) => Wake::Stop(reason, drain),
        };

        match wake {
            Wake::Stop(reason, drain) => {
                managed.set_status(ProcessStatus::Stopping);
                let exit_code = terminate(&mut child, pid, drain).await;
                managed.state.lock().unwrap().last_exit_code = exit_code;
                managed.set_status(ProcessStatus::Stopped);
                inner.system_log(&managed, format!("Stopped ({})", reason.as_str()));
                tracing::info!(process_id = %id, reason = reason.as_str(), "Process stopped");
                inner.emit(ProcessEvent::Stopped {
                    id,
                    project_id,
                    reason,
                    exit_code,
                });
                return;
            }
            Wake::Exited(status) => {
                #[cfg(unix)]
                signal_group(pid, libc::SIGKILL);
                let exit_code = status.and_then(|s| s.code());
                let success = status.map(|s| s.success()).unwrap_or(false);
                managed.state.lock().unwrap().last_exit_code = exit_code;

                if started.elapsed() >= config.stable_after {
                    backoff = config.backoff_initial;
                    recent_restarts.clear();
                }
                let should_restart = match spec.restart_policy {
                    RestartPolicy::Always => true,
                    RestartPolicy::OnFailure => !success,
                    RestartPolicy::Never => false,
                };
                let now = Instant::now();
                while recent_restarts
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > config.crash_loop_window)
                {
                    recent_restarts.pop_front();
                }
                let crash_loop =
                    should_restart && recent_restarts.len() >= config.crash_loop_max_restarts;
                let will_restart = should_restart && !crash_loop;

                let code_str = exit_code.map_or("signal".to_string(), |c| c.to_string());
                inner.system_log(
                    &managed,
                    if will_restart {
                        format!(
                            "Exited with code {}; restarting in {}ms",
                            code_str,
                            backoff.as_millis()
                        )
                    } else {
                        format!("Exited with code {}", code_str)
                    },
                );
                tracing::warn!(process_id = %id, exit_code = ?exit_code, will_restart, "Process exited");
                inner.emit(ProcessEvent::Exited {
                    id,
                    project_id,
                    exit_code,
                    will_restart,
                    restart_delay: if will_restart {
                        backoff
                    } else {
                        Duration::ZERO
                    },
                });

                if crash_loop {
                    managed.set_status(ProcessStatus::Failed);
                    managed.control.lock().unwrap().take();
                    let reason = format!(
                        "Crash loop: restarted {} times within {}s",
                        recent_restarts.len(),
                        config.crash_loop_window.as_secs()
                    );
                    inner.system_log(&managed, reason.clone());
                    inner.emit(ProcessEvent::Failed {
                        id,
                        project_id,
                        reason,
                    });
                    return;
                }
                if !will_restart {
                    managed.set_status(ProcessStatus::Exited);
                    managed.control.lock().unwrap().take();
                    return;
                }

                managed.set_status(ProcessStatus::Backoff);
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    (reason, _) = wait_for_stop(&mut stop_rx) => {
                        managed.set_status(ProcessStatus::Stopped);
                        inner.system_log(&managed, format!("Stopped ({})", reason.as_str()));
                        inner.emit(ProcessEvent::Stopped { id, project_id, reason, exit_code });
                        return;
                    }
                }
                recent_restarts.push_back(Instant::now());
                backoff = (backoff * 2).min(config.backoff_max);
                managed.state.lock().unwrap().restart_count += 1;
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spec(command: &str, policy: RestartPolicy) -> ProcessSpec {
        ProcessSpec {
            project_id: ProjectId::new(),
            command: command.to_string(),
            cwd: std::env::temp_dir(),
            env: HashMap::new(),
            restart_policy: policy,
            log_file: None,
        }
    }

    fn fast_config() -> SupervisorConfig {
        SupervisorConfig {
            backoff_initial: Duration::from_millis(20),
            backoff_max: Duration::from_millis(50),
            crash_loop_max_restarts: 3,
            ..SupervisorConfig::default()
        }
    }

    fn is_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    async fn wait_until(mut cond: impl FnMut() -> bool) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("condition not met in time");
    }

    #[tokio::test]
    async fn test_spawn_stop_and_events() {
        let supervisor = ProcessSupervisor::new();
        let mut events = supervisor.subscribe();
        let id = supervisor
            .spawn(spec("sleep 30", RestartPolicy::Always))
            .await
            .unwrap();
        let info = supervisor.get(&id).unwrap();
        assert_eq!(info.status, ProcessStatus::Running);
        let pid = info.pid.unwrap();
        assert!(is_alive(pid));

        supervisor
            .stop(&id, Duration::from_secs(2), StopReason::Requested)
            .await
            .unwrap();
        assert_eq!(supervisor.get(&id).unwrap().status, ProcessStatus::Stopped);
        assert!(!is_alive(pid));

        assert!(matches!(
            events.recv().await.unwrap(),
            ProcessEvent::Started { .. }
        ));
        assert!(matches!(
            events.recv().await.unwrap(),
            ProcessEvent::Stopped {
                reason: StopReason::Requested,
                ..
            }
        ));
        // Stopping again is a no-op.
        supervisor
            .stop(&id, Duration::from_secs(1), StopReason::Requested)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_stop_kills_whole_process_group() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut s = spec("sleep 30 & echo $! > child.pid; wait", RestartPolicy::Never);
        s.cwd = dir.path().to_path_buf();
        let supervisor = ProcessSupervisor::new();
        let id = supervisor.spawn(s).await.unwrap();
        let pid_file = dir.path().join("child.pid");
        wait_until(|| {
            std::fs::read_to_string(&pid_file)
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        })
        .await;
        let grandchild: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(is_alive(grandchild));

        supervisor
            .stop(&id, Duration::from_secs(2), StopReason::Requested)
            .await
            .unwrap();
        wait_until(|| !is_alive(grandchild)).await;
    }

    #[tokio::test]
    async fn test_sigterm_ignoring_process_is_killed_after_drain() {
        let supervisor = ProcessSupervisor::new();
        let id = supervisor
            .spawn(spec("trap '' TERM; sleep 30", RestartPolicy::Always))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        supervisor
            .stop(&id, Duration::from_millis(300), StopReason::Requested)
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(supervisor.get(&id).unwrap().status, ProcessStatus::Stopped);
    }

    #[tokio::test]
    async fn test_restart_policy_with_backoff_and_crash_loop() {
        let supervisor = ProcessSupervisor::with_config(fast_config());
        let mut events = supervisor.subscribe();
        let id = supervisor
            .spawn(spec("exit 3", RestartPolicy::Always))
            .await
            .unwrap();

        let mut starts = 0;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .unwrap()
                .unwrap()
            {
                ProcessEvent::Started { .. } => starts += 1,
                ProcessEvent::Exited { exit_code, .. } => assert_eq!(exit_code, Some(3)),
                ProcessEvent::Failed { reason, .. } => {
                    assert!(reason.contains("Crash loop"));
                    break;
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
        // Initial start plus crash_loop_max_restarts restarts.
        assert_eq!(starts, 4);
        let info = supervisor.get(&id).unwrap();
        assert_eq!(info.status, ProcessStatus::Failed);
        assert_eq!(info.restart_count, 3);

        // A failed process can be started again.
        supervisor
            .start_with_id(id, spec("sleep 30", RestartPolicy::Always))
            .await
            .unwrap();
        assert_eq!(supervisor.get(&id).unwrap().status, ProcessStatus::Running);
        supervisor
            .stop(&id, Duration::from_secs(1), StopReason::Requested)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_never_and_on_failure_policies() {
        let supervisor = ProcessSupervisor::with_config(fast_config());
        let id = supervisor
            .spawn(spec("exit 0", RestartPolicy::OnFailure))
            .await
            .unwrap();
        wait_until(|| supervisor.get(&id).unwrap().status == ProcessStatus::Exited).await;
        assert_eq!(supervisor.get(&id).unwrap().restart_count, 0);

        let id = supervisor
            .spawn(spec("exit 1", RestartPolicy::Never))
            .await
            .unwrap();
        wait_until(|| supervisor.get(&id).unwrap().status == ProcessStatus::Exited).await;
        assert_eq!(supervisor.get(&id).unwrap().last_exit_code, Some(1));
    }

    #[tokio::test]
    async fn test_restart_changes_pid_and_keeps_id() {
        let supervisor = ProcessSupervisor::new();
        let id = supervisor
            .spawn(spec("sleep 30", RestartPolicy::Always))
            .await
            .unwrap();
        let first_pid = supervisor.get(&id).unwrap().pid.unwrap();
        supervisor
            .restart(&id, Duration::from_secs(1))
            .await
            .unwrap();
        let info = supervisor.get(&id).unwrap();
        assert_eq!(info.status, ProcessStatus::Running);
        assert_ne!(info.pid.unwrap(), first_pid);
        assert_eq!(info.restart_count, 1);
        assert!(!is_alive(first_pid));
        supervisor
            .stop_all(Duration::from_secs(1), StopReason::Shutdown)
            .await;
        assert_eq!(supervisor.get(&id).unwrap().status, ProcessStatus::Stopped);
    }

    #[tokio::test]
    async fn test_logs_file_tail_and_live_stream() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("logs/app.log");
        let mut s = spec(
            "echo hello; echo oops 1>&2; printf 'no newline'; sleep 30",
            RestartPolicy::Never,
        );
        s.env.insert("GREETING".to_string(), "hi".to_string());
        s.log_file = Some(log_path.clone());

        let supervisor = ProcessSupervisor::new();
        let id = supervisor.spawn(s).await.unwrap();
        let mut live = supervisor.subscribe_logs(&id).unwrap();
        wait_until(|| supervisor.logs(&id, 10).len() >= 2).await;

        let lines = supervisor.logs(&id, 10);
        assert!(lines
            .iter()
            .any(|l| l.line == "hello" && l.stream == LogStream::Stdout));
        assert!(lines
            .iter()
            .any(|l| l.line == "oops" && l.stream == LogStream::Stderr));
        assert_eq!(read_log_tail(&log_path, 1).len(), 1);

        supervisor
            .stop(&id, Duration::from_secs(1), StopReason::Requested)
            .await
            .unwrap();
        // A trailing partial line is flushed once the pipe closes.
        wait_until(|| {
            supervisor
                .logs(&id, 10)
                .iter()
                .any(|l| l.line == "no newline")
        })
        .await;
        let mut saw_stop = false;
        while let Ok(Ok(line)) = tokio::time::timeout(Duration::from_millis(500), live.recv()).await
        {
            saw_stop |= line.stream == LogStream::System && line.line.contains("Stopped");
        }
        assert!(saw_stop);
    }

    #[tokio::test]
    async fn test_spawn_failure_is_reported() {
        let supervisor = ProcessSupervisor::new();
        let mut s = spec("true", RestartPolicy::Never);
        s.cwd = PathBuf::from("/definitely/not/a/dir");
        assert!(supervisor.spawn(s).await.is_err());
    }

    #[test]
    fn test_log_rotation() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("app.log");
        let mut log = LogFile {
            path: path.clone(),
            file: None,
            size: 0,
        };
        for i in 0..50 {
            write_log_line(
                &mut log,
                &LogLine::now(LogStream::Stdout, format!("line {i}")),
                200,
            );
        }
        assert!(dir.path().join("app.log.1").exists());
        assert!(std::fs::metadata(&path).unwrap().len() <= 300);
        assert_eq!(read_log_tail(&path, 1)[0].line, "line 49");
    }

    #[test]
    fn test_restart_policy_parse() {
        assert_eq!(RestartPolicy::parse("always"), Some(RestartPolicy::Always));
        assert_eq!(
            RestartPolicy::parse("on_failure"),
            Some(RestartPolicy::OnFailure)
        );
        assert_eq!(RestartPolicy::parse("never"), Some(RestartPolicy::Never));
        assert_eq!(RestartPolicy::parse("sometimes"), None);
    }
}
