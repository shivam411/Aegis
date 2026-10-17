//! Talks to Docker through the `docker` CLI, which honours the operator's
//! `DOCKER_HOST`, contexts and rootless setups without extra configuration.

use crate::args::{self, ContainerPlan};
use aegis_config::ResourceLimits;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::Command;

/// The `docker` executable: `AEGIS_DOCKER_BIN` if set, else `docker` on PATH.
pub fn docker_bin() -> String {
    std::env::var("AEGIS_DOCKER_BIN")
        .ok()
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| "docker".to_string())
}

/// Whether Docker can be used, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DockerStatus {
    pub available: bool,
    /// The Docker Engine version, when reachable.
    pub version: Option<String>,
    /// Why Docker can't be used.
    pub reason: Option<String>,
}

impl DockerStatus {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            version: None,
            reason: Some(reason.into()),
        }
    }
}

/// Runs `docker` with `args`; stdout on success, a readable error otherwise.
pub async fn run_docker(bin: &str, args: &[String], timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => {
            return Err(format!(
                "docker {} timed out after {}s",
                args.first().map(String::as_str).unwrap_or(""),
                timeout.as_secs()
            ))
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(not_installed(bin));
        }
        Ok(Err(e)) => return Err(format!("Could not run {}: {}", bin, e)),
        Ok(Ok(output)) => output,
    };
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Err(if message.is_empty() {
        format!("docker exited with {}", output.status)
    } else {
        message
    })
}

fn not_installed(bin: &str) -> String {
    format!(
        "Docker is not installed: the '{}' command was not found (install Docker, or set AEGIS_DOCKER_BIN to its path)",
        bin
    )
}

/// Asks the Docker daemon for its version: tells "not installed", "daemon
/// not running" and "no permission" apart.
pub async fn probe(bin: &str) -> DockerStatus {
    let args = [
        "version".to_string(),
        "--format".to_string(),
        "{{.Server.Version}}".to_string(),
    ];
    match run_docker(bin, &args, Duration::from_secs(15)).await {
        Ok(version) if !version.is_empty() => DockerStatus {
            available: true,
            version: Some(version),
            reason: None,
        },
        Ok(_) => DockerStatus::unavailable("The Docker daemon did not report a version"),
        Err(e) if e.starts_with("Docker is not installed") => DockerStatus::unavailable(e),
        Err(e) if e.to_ascii_lowercase().contains("permission denied") => {
            DockerStatus::unavailable(format!(
                "No permission to use the Docker daemon; add the user running Aegis to the 'docker' group ({})",
                e
            ))
        }
        Err(e) => DockerStatus::unavailable(format!(
            "The Docker daemon is not running or not reachable ({})",
            e
        )),
    }
}

/// A container's state, from `docker inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerState {
    /// created, running, exited, ...
    pub status: String,
    /// The main process's pid on the host (0 when not running).
    pub pid: u32,
    pub exit_code: i32,
}

/// One Aegis installation's view of Docker.
pub struct Docker {
    bin: String,
    /// Private directory for env files (0700).
    run_dir: PathBuf,
    /// Value of the `aegis.instance` label on what this installation creates.
    instance: String,
    status: Mutex<Option<DockerStatus>>,
}

impl std::fmt::Debug for Docker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Docker")
            .field("bin", &self.bin)
            .field("instance", &self.instance)
            .finish()
    }
}

impl Docker {
    /// `run_dir` holds short-lived env files; `instance` tells containers of
    /// different Aegis installations (data directories) apart.
    pub fn new(run_dir: PathBuf, instance: String) -> Self {
        Self {
            bin: docker_bin(),
            run_dir,
            instance,
            status: Mutex::new(None),
        }
    }

    /// An instance label value derived from the data directory.
    pub fn instance_for(data_dir: &Path) -> String {
        let text = data_dir.to_string_lossy();
        let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
        });
        format!("{:016x}", hash)
    }

    pub fn bin(&self) -> &str {
        &self.bin
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    async fn run(&self, args: &[String], timeout: Duration) -> Result<String, String> {
        run_docker(&self.bin, args, timeout).await
    }

    /// Checks Docker now and remembers the answer for [`Docker::status`].
    pub async fn check(&self) -> DockerStatus {
        let status = probe(&self.bin).await;
        *self.status.lock().unwrap() = Some(status.clone());
        status
    }

    /// The last checked status (`None` before the first check).
    pub fn status(&self) -> Option<DockerStatus> {
        self.status.lock().unwrap().clone()
    }

    /// The last known status, checking first if there is none.
    pub async fn current_status(&self) -> DockerStatus {
        match self.status() {
            Some(s) => s,
            None => self.check().await,
        }
    }

    /// The common labels for containers and images this installation makes.
    pub fn base_labels(&self) -> Vec<(String, String)> {
        vec![
            (args::LABEL_MANAGED.to_string(), "true".to_string()),
            (args::LABEL_INSTANCE.to_string(), self.instance.clone()),
        ]
    }

    pub async fn image_exists(&self, image: &str) -> bool {
        let args = [
            "image".to_string(),
            "inspect".to_string(),
            "--format".to_string(),
            "{{.Id}}".to_string(),
            image.to_string(),
        ];
        self.run(&args, Duration::from_secs(30)).await.is_ok()
    }

    /// Creates a container. The environment goes through an env file that
    /// only the daemon's user can read, removed as soon as Docker has read
    /// it, so values never show up in a process list.
    pub async fn create(
        &self,
        plan: &ContainerPlan,
        env: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let contents = args::env_file_contents(env)?;
        let env_file = EnvFile::write(&self.run_dir, &plan.name, &contents)
            .map_err(|e| format!("Could not write the container's env file: {}", e))?;
        let args = args::create_args(plan, &env_file.path);
        let result = self.run(&args, Duration::from_secs(120)).await;
        drop(env_file);
        result.map(|_| ())
    }

    /// Force-removes a container (and its anonymous volumes); a missing one is fine.
    pub async fn remove_container(&self, name: &str) {
        let args = [
            "rm".to_string(),
            "--force".to_string(),
            "--volumes".to_string(),
            name.to_string(),
        ];
        if let Err(e) = self.run(&args, Duration::from_secs(60)).await {
            if !e.contains("No such container") {
                tracing::warn!(container = name, error = %e, "Could not remove container");
            }
        }
    }

    /// `docker stop`: SIGTERM, then SIGKILL after `drain`.
    pub async fn stop_container(&self, name: &str, drain: Duration) {
        let secs = drain.as_secs() + u64::from(drain.subsec_nanos() > 0);
        let args = [
            "stop".to_string(),
            "--time".to_string(),
            secs.to_string(),
            name.to_string(),
        ];
        if let Err(e) = self.run(&args, drain + Duration::from_secs(30)).await {
            if !e.contains("No such container") {
                tracing::warn!(container = name, error = %e, "Could not stop container");
            }
        }
    }

    pub async fn kill_container(&self, name: &str) -> Result<(), String> {
        let args = ["kill".to_string(), name.to_string()];
        self.run(&args, Duration::from_secs(30)).await.map(|_| ())
    }

    pub async fn state(&self, name: &str) -> Option<ContainerState> {
        let args = [
            "inspect".to_string(),
            "--type".to_string(),
            "container".to_string(),
            "--format".to_string(),
            "{{.State.Status}} {{.State.Pid}} {{.State.ExitCode}}".to_string(),
            name.to_string(),
        ];
        let out = self.run(&args, Duration::from_secs(30)).await.ok()?;
        let mut parts = out.split_whitespace();
        Some(ContainerState {
            status: parts.next()?.to_string(),
            pid: parts.next()?.parse().ok()?,
            exit_code: parts.next()?.parse().ok()?,
        })
    }

    /// Applies limits to a running container at once (`docker update`).
    pub async fn update_limits(
        &self,
        name: &str,
        limits: &ResourceLimits,
        host_cpus: usize,
        host_memory_bytes: u64,
    ) -> Result<(), String> {
        let mut args = args::limit_update_args(limits, host_cpus, host_memory_bytes);
        args.push(name.to_string());
        self.run(&args, Duration::from_secs(60)).await.map(|_| ())
    }

    /// Names of containers this installation created, with their process id label.
    pub async fn list_containers(&self) -> Result<Vec<(String, String)>, String> {
        let args = [
            "ps".to_string(),
            "--all".to_string(),
            "--no-trunc".to_string(),
            "--filter".to_string(),
            format!("label={}={}", args::LABEL_INSTANCE, self.instance),
            "--format".to_string(),
            format!("{{{{.Names}}}} {{{{.Label \"{}\"}}}}", args::LABEL_PROCESS),
        ];
        let out = self.run(&args, Duration::from_secs(60)).await?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let (name, process) = l.trim().split_once(' ').unwrap_or((l.trim(), ""));
                (!name.is_empty()).then(|| (name.to_string(), process.to_string()))
            })
            .collect())
    }

    /// Removes an image tag. Images still used by a container stay.
    pub async fn remove_image(&self, image: &str) -> Result<(), String> {
        let args = ["image".to_string(), "rm".to_string(), image.to_string()];
        self.run(&args, Duration::from_secs(120)).await.map(|_| ())
    }
}

/// A 0600 file in a 0700 directory, deleted when dropped.
struct EnvFile {
    path: PathBuf,
}

impl EnvFile {
    fn write(dir: &Path, name: &str, contents: &str) -> std::io::Result<Self> {
        use std::io::Write;
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!("{}-{}.env", name, nanos));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let guard = Self { path };
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        Ok(guard)
    }
}

impl Drop for EnvFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_is_private_and_removed() {
        let dir = tempfile::TempDir::new().unwrap();
        let run_dir = dir.path().join("run");
        let file = EnvFile::write(&run_dir, "aegis-x", "A=1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file.path).unwrap(), "A=1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&file.path), 0o600);
            assert_eq!(mode(&run_dir), 0o700);
        }
        let path = file.path.clone();
        drop(file);
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(&run_dir).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn missing_docker_is_reported_as_not_installed() {
        let status = probe("/definitely/not/docker-aegis-test").await;
        assert!(!status.available);
        assert!(
            status.reason.as_deref().unwrap().contains("not installed"),
            "{status:?}"
        );
    }
}
