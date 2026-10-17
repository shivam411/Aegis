//! Runs a release as a container under the process supervisor.
//!
//! Each start (including automatic restarts) creates a fresh container
//! `aegis-<process id>` from the release's image, and the supervisor runs
//! `docker start --attach` for it: that command streams the container's
//! output into the process's log file and exits with the container's exit
//! code, so crash detection, restart backoff and crash-loop handling are
//! the supervisor's own. Stopping uses `docker stop`, and the container is
//! removed once the process is gone for good.

use crate::args::{self, ContainerPlan, Mount};
use crate::cli::Docker;
use aegis_config::{ResourceLimits, RunSection};
use aegis_process::{Launch, Launcher, ProcessSpec, RuntimeInfo};
use aegis_types::{ProcessId, ProjectId};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// The limits to start a container with (read at every start).
pub type LimitsSource = Arc<dyn Fn() -> ResourceLimits + Send + Sync>;

/// One release's container settings.
#[derive(Debug, Clone)]
pub struct ContainerSettings {
    pub project_id: ProjectId,
    pub release: String,
    /// The release's image (`aegis/<project>:<release>`).
    pub image: String,
    pub run: RunSection,
}

pub struct DockerLauncher {
    docker: Arc<Docker>,
    settings: ContainerSettings,
    limits: LimitsSource,
}

impl std::fmt::Debug for DockerLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerLauncher")
            .field("image", &self.settings.image)
            .finish()
    }
}

impl DockerLauncher {
    pub fn new(docker: Arc<Docker>, settings: ContainerSettings, limits: LimitsSource) -> Self {
        Self {
            docker,
            settings,
            limits,
        }
    }

    /// What `docker create` gets for a start, and the container's
    /// environment. `PORT` in `env` is the host port (the configured port,
    /// or the one allocated for the proxy); the app inside listens on the
    /// container port, and `PORT` is set to that.
    pub fn plan(
        &self,
        id: &ProcessId,
        env: &std::collections::HashMap<String, String>,
    ) -> (ContainerPlan, BTreeMap<String, String>) {
        let s = &self.settings;
        let host_port = env.get("PORT").and_then(|p| p.trim().parse::<u16>().ok());
        let container_port = s.run.container_port.or(host_port);
        let mut env: BTreeMap<String, String> =
            env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Some(port) = container_port {
            env.insert("PORT".to_string(), port.to_string());
        }
        let mut labels = self.docker.base_labels();
        labels.extend([
            (args::LABEL_PROJECT.to_string(), s.project_id.to_string()),
            (args::LABEL_PROCESS.to_string(), id.to_string()),
            (args::LABEL_RELEASE.to_string(), s.release.clone()),
        ]);
        let plan = ContainerPlan {
            name: args::container_name(id),
            image: s.image.clone(),
            labels,
            publish: host_port.zip(container_port),
            limits: (self.limits)(),
            init: s.run.init(),
            user: s.run.user.clone(),
            read_only: s.run.read_only.unwrap_or(false),
            tmpfs: s.run.tmpfs.clone(),
            mounts: s
                .run
                .volume_mounts()
                .into_iter()
                .map(|m| Mount {
                    volume: args::volume_name(&s.project_id, &m.name),
                    target: m.target,
                    read_only: m.read_only,
                })
                .collect(),
            command: s.run.command.clone().unwrap_or_default(),
        };
        (plan, env)
    }
}

#[async_trait::async_trait]
impl Launcher for DockerLauncher {
    fn info(&self, id: &ProcessId) -> RuntimeInfo {
        RuntimeInfo {
            kind: "docker",
            container: Some(args::container_name(id)),
            image: Some(self.settings.image.clone()),
        }
    }

    async fn prepare(&self, id: &ProcessId, spec: &ProcessSpec) -> Result<Launch, String> {
        let (plan, env) = self.plan(id, &spec.env);
        // A container left by a crash or an unclean daemon exit.
        self.docker.remove_container(&plan.name).await;
        self.docker
            .create(&plan, &env)
            .await
            .map_err(|e| format!("Could not create container {}: {}", plan.name, e))?;
        Ok(Launch {
            program: self.docker.bin().to_string(),
            args: vec!["start".into(), "--attach".into(), plan.name],
        })
    }

    async fn started(&self, id: &ProcessId) -> Option<u32> {
        let name = args::container_name(id);
        for _ in 0..100 {
            match self.docker.state(&name).await {
                Some(s) if s.status == "running" && s.pid > 0 => return Some(s.pid),
                // Docker could not start it (e.g. the port is taken); the
                // attached `docker start` reports why and exits.
                Some(s) if s.status == "created" && s.exit_code != 0 => return None,
                Some(s) if s.status == "created" || s.status == "running" => {}
                // Exited already, or gone.
                _ => return None,
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    async fn stop(&self, id: &ProcessId, drain: Duration) {
        self.docker
            .stop_container(&args::container_name(id), drain)
            .await;
    }

    async fn cleanup(&self, id: &ProcessId) {
        self.docker
            .remove_container(&args::container_name(id))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn launcher(run: RunSection) -> DockerLauncher {
        let docker = Arc::new(Docker::new(
            std::env::temp_dir().join("aegis-docker-test"),
            "inst".into(),
        ));
        DockerLauncher::new(
            docker,
            ContainerSettings {
                project_id: ProjectId::new(),
                release: "r7".into(),
                image: "aegis/p:r7".into(),
                run,
            },
            Arc::new(|| ResourceLimits {
                cpu_cores: Some(1.5),
                ..Default::default()
            }),
        )
    }

    #[test]
    fn plan_maps_host_port_to_container_port() {
        let l = launcher(RunSection {
            runtime: Some("docker".into()),
            container_port: Some(80),
            volumes: vec!["data:/data".into()],
            ..Default::default()
        });
        let id = ProcessId::new();
        let env = HashMap::from([
            ("PORT".to_string(), "41000".to_string()),
            ("TOKEN".to_string(), "secret".to_string()),
        ]);
        let (plan, container_env) = l.plan(&id, &env);
        assert_eq!(plan.publish, Some((41000, 80)));
        assert_eq!(container_env["PORT"], "80");
        assert_eq!(container_env["TOKEN"], "secret");
        assert_eq!(plan.name, format!("aegis-{}", id));
        assert_eq!(plan.limits.cpu_cores, Some(1.5));
        assert!(plan.init);
        assert_eq!(
            plan.mounts[0].volume,
            format!("aegis-{}-data", l.settings.project_id)
        );
        assert!(plan
            .labels
            .contains(&(args::LABEL_RELEASE.to_string(), "r7".to_string())));
        assert!(plan
            .labels
            .contains(&(args::LABEL_INSTANCE.to_string(), "inst".to_string())));
        let info = l.info(&id);
        assert_eq!(info.kind, "docker");
        assert_eq!(info.image.as_deref(), Some("aegis/p:r7"));

        // Without container_port the app listens on the same port inside.
        let l = launcher(RunSection {
            runtime: Some("docker".into()),
            ..Default::default()
        });
        let (plan, container_env) = l.plan(&id, &env);
        assert_eq!(plan.publish, Some((41000, 41000)));
        assert_eq!(container_env["PORT"], "41000");
        // No port at all: nothing published.
        let (plan, container_env) = l.plan(&id, &HashMap::new());
        assert_eq!(plan.publish, None);
        assert!(!container_env.contains_key("PORT"));
    }
}
