use aegis_types::{ProcessId, ProjectId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartPolicy {
    Always,
    OnFailure,
    Never,
}

#[derive(Debug, Clone)]
pub struct ManagedProcessInfo {
    pub id: ProcessId,
    pub project_id: ProjectId,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub is_running: bool,
}

pub struct ProcessSupervisor {
    processes: Arc<RwLock<HashMap<ProcessId, ManagedProcessInfo>>>,
}

impl ProcessSupervisor {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Spawns a child OS process and begins background log streaming & lifecycle monitoring.
    pub async fn spawn_process(
        &self,
        project_id: ProjectId,
        command: &str,
        args: &[&str],
        cwd: PathBuf,
        env: HashMap<String, String>,
        _policy: RestartPolicy,
    ) -> Result<ProcessId, anyhow::Error> {
        let process_id = ProcessId::new();
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.current_dir(&cwd);
        cmd.envs(env);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        tracing::info!(process_id = %process_id, command = %command, "Spawning process");

        let mut child = cmd.spawn()?;
        let pid = child.id();

        let info = ManagedProcessInfo {
            id: process_id,
            project_id,
            command: command.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd,
            pid,
            restart_count: 0,
            is_running: true,
        };

        self.processes.write().unwrap().insert(process_id, info);

        // Pipe stdout asynchronously
        if let Some(stdout) = child.stdout.take() {
            let proc_id_str = process_id.to_string();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    tracing::info!(target: "process_stdout", process_id = %proc_id_str, "{}", line);
                }
            });
        }

        // Pipe stderr asynchronously
        if let Some(stderr) = child.stderr.take() {
            let proc_id_str = process_id.to_string();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    tracing::warn!(target: "process_stderr", process_id = %proc_id_str, "{}", line);
                }
            });
        }

        let procs = self.processes.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            if let Ok(st) = status {
                tracing::info!(process_id = %process_id, status = %st, "Process exited");
            }
            if let Some(proc) = procs.write().unwrap().get_mut(&process_id) {
                proc.is_running = false;
                proc.pid = None;
            }
        });

        Ok(process_id)
    }

    /// Stops a running process.
    pub async fn stop_process(&self, process_id: &ProcessId) -> Result<(), anyhow::Error> {
        if let Some(proc) = self.processes.write().unwrap().get_mut(process_id) {
            if let Some(pid) = proc.pid {
                tracing::info!(process_id = %process_id, pid = pid, "Killing process");
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/F", "/PID", &pid.to_string()])
                        .output();
                }
                #[cfg(not(windows))]
                {
                    let _ = std::process::Command::new("kill")
                        .arg("-9")
                        .arg(pid.to_string())
                        .output();
                }
            }
            proc.is_running = false;
            proc.pid = None;
        }
        Ok(())
    }

    pub fn list_processes(&self) -> Vec<ManagedProcessInfo> {
        self.processes.read().unwrap().values().cloned().collect()
    }
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_and_list_process() {
        let supervisor = ProcessSupervisor::new();
        let proj_id = ProjectId::new();

        #[cfg(windows)]
        let (cmd, args) = ("cmd", vec!["/C", "echo hello"]);
        #[cfg(not(windows))]
        let (cmd, args) = ("echo", vec!["hello"]);

        let proc_id = supervisor
            .spawn_process(
                proj_id,
                cmd,
                &args,
                std::env::current_dir().unwrap(),
                HashMap::new(),
                RestartPolicy::Never,
            )
            .await
            .unwrap();

        let procs = supervisor.list_processes();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].id, proc_id);
    }
}

