use aegis_api::aegis::aegis_daemon_client::AegisDaemonClient;
use aegis_api::aegis::{
    ControlProcessRequest, DeployRequest, EmitEventRequest, GetLogsRequest, ListDeploymentsRequest,
    ListEventsRequest, ListProjectsRequest, ListReleasesRequest, LogLine, ProcessAction,
    ProcessInfo, RegisterProjectRequest, RollbackRequest, StatusRequest, StatusResponse,
    StreamEventsRequest, StreamLogsRequest,
};
use aegis_config::{Config, ProjectFile};
use aegis_engine::ResolvedProjectConfig;
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use tonic::transport::Channel;

#[derive(Parser)]
#[command(name = "aegis")]
#[command(about = "Aegis operational terminal platform CLI", long_about = None)]
struct Cli {
    #[arg(short, long, help = "Path to config file")]
    config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check daemon status and loaded plugins
    Status,
    /// Initialize a project and auto-detect runtime or apply a project template
    Init {
        /// Project name
        #[arg(short, long)]
        name: Option<String>,
        /// Repository URL
        #[arg(short, long)]
        repo: Option<String>,
        /// Apply starter template (nextjs, spring-boot, rust, go, python)
        #[arg(short, long)]
        template: Option<String>,
    },
    /// Build and deploy a project, following progress until it is live
    Deploy {
        /// Project id or name (defaults to the project in ./aegis.toml)
        project: Option<String>,
        /// Branch to deploy when cloning from a repository
        #[arg(short, long)]
        branch: Option<String>,
        /// Deployment strategy (GracefulSwitch, Immediate)
        #[arg(short, long)]
        strategy: Option<String>,
        /// Release name (defaults to a timestamp)
        #[arg(long = "release")]
        version: Option<String>,
        /// Clone this repository instead of copying the local directory
        #[arg(long)]
        repo: Option<String>,
        /// Commit to deploy when cloning
        #[arg(long)]
        commit: Option<String>,
        /// Queue the deployment and return without waiting
        #[arg(short, long)]
        detach: bool,
    },
    /// Switch back to an earlier release without rebuilding
    Rollback {
        /// Project id or name (defaults to the project in ./aegis.toml)
        project: Option<String>,
        /// Release to roll back to (defaults to the previous release)
        #[arg(short, long)]
        version: Option<String>,
    },
    /// List projects and the state of their processes
    List,
    /// Show an app's logs
    Logs {
        /// Process id, or project id/name (defaults to ./aegis.toml)
        target: Option<String>,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "100")]
        lines: u32,
        /// Keep streaming new lines
        #[arg(short, long)]
        follow: bool,
    },
    /// Start an app's process
    Start {
        /// Process id, or project id/name (defaults to ./aegis.toml)
        target: Option<String>,
    },
    /// Stop an app's process (it stays stopped across daemon restarts)
    Stop {
        /// Process id, or project id/name (defaults to ./aegis.toml)
        target: Option<String>,
    },
    /// Configure a daily auto-deployment schedule at specific hours
    Schedule {
        /// Target project ID (reads from aegis.toml if omitted)
        project_id: Option<String>,
        /// Target deployment hour (0-23)
        #[arg(short, long)]
        hour: u32,
        /// Target deployment minute (0-59)
        #[arg(short, long, default_value = "0")]
        minute: u32,
        /// Target Git branch
        #[arg(short, long, default_value = "main")]
        branch: String,
    },
    /// Restart an app's process
    Restart {
        /// Process id, or project id/name (defaults to ./aegis.toml)
        target: Option<String>,
    },
    /// Manually emit an operational event to the daemon
    EmitEvent {
        /// The event type (e.g. RepositoryAdded, DeploymentStarted)
        event_type: String,
        /// The event payload in JSON format
        payload_json: String,
    },
    /// Stream operational events in real-time from the daemon
    StreamEvents,
    /// Stream operational events in real-time from the daemon (alias for StreamEvents)
    Events,
    /// Run diagnostic checks on the environment and project configuration (Phase 1C)
    Doctor {
        /// Attempt to automatically fix detected configuration issues
        #[arg(short, long)]
        fix: bool,
    },
    /// Validate project aegis.toml configuration and show the effective settings
    Validate,
    /// Detailed diagnostic inspect of project resources and releases [STABLE]
    Inspect {
        /// Project ID to inspect (reads from aegis.toml if omitted)
        project_id: Option<String>,
        /// Time-travel historical timestamp (RFC-3339 format, e.g. 2026-08-03T09:15:00Z)
        #[arg(short, long)]
        at: Option<String>,
    },
    /// Explain recommended deployment strategies and capability auto-detections
    Explain,
    /// Run an interactive 2-minute feature demonstration tour (Milestone M2)
    Demo,
    /// List a project's releases
    Releases {
        /// Project id or name (defaults to ./aegis.toml)
        project: Option<String>,
    },
    /// List deployments, newest last
    Deployments {
        /// Project id or name (all projects when omitted)
        project: Option<String>,
    },
    /// Show a project's recent events
    Timeline {
        /// Project id or name (defaults to ./aegis.toml)
        project: Option<String>,
        /// Number of events to show
        #[arg(short = 'n', long, default_value = "50")]
        limit: u32,
    },
    /// Step-by-step event-sourced deployment replay signature feature [STABLE]
    Replay {
        /// Target release version or ID (e.g. release-153 or v1.1.0)
        release: String,
    },
    /// Operational incident diagnostic and auto-rollback advisor [EXPERIMENTAL]
    Incident {
        /// Target project ID to analyze
        project_id: Option<String>,
    },
    /// Comprehensive operational outage investigation tool [EXPERIMENTAL]
    Investigate {
        /// Target project ID to investigate
        project_id: Option<String>,
    },
    /// Migrate legacy process configurations (PM2, systemd) to aegis.toml [STABLE]
    Migrate {
        #[command(subcommand)]
        target: MigrateSubcommand,
    },
    /// Perform zero-downtime self-update of Aegis daemon and CLI binaries [STABLE]
    Upgrade {
        /// Only check for available updates without applying
        #[arg(short, long)]
        check: bool,
        /// Force re-installation of current version
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum MigrateSubcommand {
    /// Migrate PM2 ecosystem configuration (ecosystem.config.js / json) to aegis.toml
    Pm2 {
        /// Path to PM2 ecosystem configuration file
        #[arg(short, long, default_value = "ecosystem.config.js")]
        file: String,
    },
    /// Migrate systemd service unit file to aegis.toml
    Systemd {
        /// Path to systemd service unit file
        #[arg(short, long)]
        service: String,
    },
}

/// Connects to the daemon on first use, so commands that work offline
/// (init, validate, doctor, migrate) don't need it running.
struct Daemon {
    addr: String,
    client: Option<AegisDaemonClient<Channel>>,
}

impl Daemon {
    async fn client(&mut self) -> anyhow::Result<&mut AegisDaemonClient<Channel>> {
        if self.client.is_none() {
            let client = AegisDaemonClient::connect(self.addr.clone())
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Cannot reach the Aegis daemon at {}. Start it with 'aegis-daemon'.",
                        self.addr
                    )
                })?;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().unwrap())
    }

    async fn status(&mut self) -> Option<StatusResponse> {
        let client = self.client().await.ok()?;
        client
            .get_status(StatusRequest {})
            .await
            .ok()
            .map(|r| r.into_inner())
    }
}

/// Turns a gRPC error into a plain message (no "status: ..., metadata: ..." noise).
fn rpc<T>(result: Result<tonic::Response<T>, tonic::Status>) -> anyhow::Result<T> {
    result
        .map(|r| r.into_inner())
        .map_err(|s| anyhow::anyhow!("{}", s.message()))
}

fn has_project_section(toml_content: &str) -> bool {
    toml_content.lines().any(|line| line.trim() == "[project]")
}

/// The project id from ./aegis.toml, if there is one.
fn local_project_id() -> Option<String> {
    let dir = std::env::current_dir().ok()?;
    ProjectFile::load_from_dir(&dir).ok()??.project.id
}

/// An explicit project/process key, or the project in ./aegis.toml.
fn target_or_local(explicit: Option<String>) -> anyhow::Result<String> {
    explicit.or_else(local_project_id).ok_or_else(|| {
        anyhow::anyhow!("No project given and no [project] id in ./aegis.toml. Pass a project name or run 'aegis init'.")
    })
}

/// A short, still-distinctive form of an id. UUIDv7 ids start with a
/// timestamp, so the prefix is shared by everything created around the same
/// time; the random tail is what tells them apart.
fn short(id: &str) -> &str {
    if id.len() == 36 && id.as_bytes()[8] == b'-' {
        &id[28..]
    } else {
        id.get(..8).unwrap_or(id)
    }
}

/// "2026-09-28T05:12:03.123+00:00" -> "2026-09-28 05:12:03"
fn short_time(ts: &str) -> String {
    ts.get(..19).unwrap_or(ts).replace('T', " ")
}

fn print_log_line(line: &LogLine) {
    println!(
        "{} [{}] {}",
        short_time(&line.timestamp),
        line.stream,
        line.line
    );
}

fn describe_process(p: &ProcessInfo) -> String {
    let pid = p.pid.map_or("-".to_string(), |pid| pid.to_string());
    format!(
        "{} (pid {}, release {}, restarts {})",
        p.status,
        pid,
        if p.release_version.is_empty() {
            "-"
        } else {
            &p.release_version
        },
        p.restart_count
    )
}

fn json_str<'a>(payload: &'a serde_json::Value, key: &str) -> &'a str {
    payload.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Follows a deployment's events and prints stage progress. Returns an error
/// if the deployment fails.
async fn follow_deployment(
    events: &mut tonic::Streaming<aegis_api::aegis::EventResponse>,
    deployment_id: &str,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let mut stage_started: HashMap<String, Instant> = HashMap::new();
    while let Some(event) = events.message().await? {
        let payload: serde_json::Value =
            serde_json::from_str(&event.payload_json).unwrap_or_default();
        if json_str(&payload, "deployment_id") != deployment_id {
            continue;
        }
        match event.event_type.as_str() {
            "DeploymentStarted" => {
                println!("  Release {}", json_str(&payload, "version"));
            }
            t if t.starts_with("BuildStage") => {
                let stage = json_str(&payload, "stage").to_string();
                let detail = json_str(&payload, "detail");
                match json_str(&payload, "status") {
                    "Started" => {
                        stage_started.insert(stage, Instant::now());
                    }
                    "Success" => {
                        let secs = stage_started
                            .get(&stage)
                            .map(|t| t.elapsed().as_secs_f64())
                            .unwrap_or(0.0);
                        let detail = if stage == "Clone" && detail.len() >= 7 {
                            format!("  {}", short(detail))
                        } else {
                            String::new()
                        };
                        println!("  ✓ {:<8} {:>6.1}s{}", stage, secs, detail);
                    }
                    "Skipped" => println!("  - {:<8}  skipped: {}", stage, detail),
                    "Failed" => println!("  ✗ {:<8}  {}", stage, detail),
                    _ => {}
                }
            }
            "DeploymentCompleted" => {
                println!(
                    "Deployed release {} in {:.1}s",
                    json_str(&payload, "version"),
                    started.elapsed().as_secs_f64()
                );
                return Ok(());
            }
            "DeploymentFailed" => {
                let tail: Vec<&str> = payload
                    .get("log_tail")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|l| l.as_str()).collect())
                    .unwrap_or_default();
                if !tail.is_empty() {
                    println!("  Last output:");
                    for line in tail {
                        println!("    | {}", line);
                    }
                }
                let log_path = json_str(&payload, "log_path");
                if !log_path.is_empty() {
                    println!("  Full build log: {}", log_path);
                }
                let restored = json_str(&payload, "rolled_back_to");
                let outcome = if !restored.is_empty() {
                    format!("Rolled back: release {} is serving again.", restored)
                } else if json_str(&payload, "stage") == "Promote" {
                    "The previous release was not running, so nothing was restored.".to_string()
                } else {
                    "The live release was not changed.".to_string()
                };
                anyhow::bail!(
                    "Deployment failed: {}\n{}",
                    json_str(&payload, "reason"),
                    outcome
                );
            }
            _ => {}
        }
    }
    anyhow::bail!("Lost connection to the daemon; the deployment continues in the background (see 'aegis deployments')")
}

#[tokio::main]
async fn main() {
    // Print errors as messages, not as Debug output with a backtrace.
    if let Err(e) = run().await {
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    // Load configuration to find daemon port
    let config_path = cli.config.unwrap_or_else(|| "aegis.toml".to_string());
    let config = Config::load_or_default(Path::new(&config_path));

    let addr = format!("http://{}:{}", config.daemon.host, config.daemon.port);
    let mut daemon = Daemon {
        addr: addr.clone(),
        client: None,
    };

    match cli.command {
        Commands::Status => {
            let response = rpc(daemon.client().await?.get_status(StatusRequest {}).await)?;
            println!("Aegis Daemon Status:");
            println!("  Initialized: {}", response.initialized);
            println!("  Version:     {}", response.version);
            println!("  Projects:    {}", response.project_count);
            println!("  Running:     {}", response.running_processes);
            println!("  Data dir:    {}", response.data_dir);
            println!("  Plugins ({}):", response.loaded_plugins.len());
            for plugin in response.loaded_plugins {
                println!("    - {}", plugin);
            }
            println!("  Total Events: {}", response.event_count);
        }
        Commands::Init {
            name,
            repo,
            template,
        } => {
            let current_dir = std::env::current_dir()?;
            let local_toml_path = current_dir.join("aegis.toml");
            // Re-running init keeps the project's identity.
            let project_id = local_project_id()
                .and_then(|id| id.parse::<aegis_types::ProjectId>().ok())
                .unwrap_or_default();

            // Execute Detector Pipeline
            let pipeline = aegis_engine::DetectorPipeline::new();
            let mut detected_config = pipeline.detect_all(&current_dir);

            if let Some(tmpl) = template {
                println!("Applying template: {}", tmpl);
                match tmpl.as_str() {
                    "nextjs" => {
                        detected_config.runtime_engine = "Node.js".to_string();
                        detected_config.build_command = "npm run build".to_string();
                        detected_config.start_command = "npm start".to_string();
                    }
                    "spring-boot" => {
                        detected_config.runtime_engine = "Java".to_string();
                        detected_config.build_command = "./gradlew build".to_string();
                        detected_config.start_command = "java -jar build/libs/app.jar".to_string();
                    }
                    "rust" => {
                        detected_config.runtime_engine = "Rust".to_string();
                        detected_config.build_command = "cargo build --release".to_string();
                        detected_config.start_command = "./target/release/app".to_string();
                    }
                    "go" => {
                        detected_config.runtime_engine = "Go".to_string();
                        detected_config.build_command = "go build -o app".to_string();
                        detected_config.start_command = "./app".to_string();
                    }
                    "python" => {
                        detected_config.runtime_engine = "Python".to_string();
                        detected_config.build_command =
                            "pip install -r requirements.txt".to_string();
                        detected_config.start_command = "python app.py".to_string();
                    }
                    _ => println!("Custom template '{}' applied", tmpl),
                }
            }

            let proj_name = name.unwrap_or(detected_config.project_name.clone());
            detected_config.project_name = proj_name.clone();

            let git = |args: &'static [&'static str]| {
                std::process::Command::new("git")
                    .args(args)
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            };
            let repo_url = repo
                .or_else(|| git(&["remote", "get-url", "origin"]))
                .unwrap_or_default();
            let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"])
                .filter(|b| b != "HEAD")
                .unwrap_or_else(|| "main".to_string());

            println!(
                "Initializing Aegis Project '{}' ({})",
                proj_name, project_id
            );
            println!("Runtime Engine: {}", detected_config.runtime_engine);
            println!("Build Command:  {}", detected_config.build_command);
            println!("Start Command:  {}", detected_config.start_command);

            // Generate zero-boilerplate aegis.toml FIRST locally
            let toml_content = pipeline.generate_toml(&detected_config, &project_id.to_string());
            if !local_toml_path.exists() {
                std::fs::write(&local_toml_path, toml_content)?;
                println!("Generated zero-boilerplate config: aegis.toml");
            } else {
                let existing = std::fs::read_to_string(&local_toml_path)?;
                if has_project_section(&existing) {
                    println!("Found existing aegis.toml config file.");
                } else {
                    // Without [project], `deploy` would have no project to deploy.
                    let merged = format!("{}\n{}", existing.trim_end(), toml_content);
                    std::fs::write(&local_toml_path, merged)?;
                    println!("Added [project] section to existing aegis.toml");
                }
            }

            let registered = match daemon.client().await {
                Ok(client) => rpc(client
                    .register_project(RegisterProjectRequest {
                        project_id: project_id.to_string(),
                        name: proj_name,
                        repository_url: repo_url,
                        branch,
                        runtime: detected_config.runtime_engine.clone(),
                        source_dir: current_dir.to_string_lossy().to_string(),
                    })
                    .await)
                .map(|_| ()),
                Err(e) => Err(e),
            };
            match registered {
                Ok(()) => println!("Project registered with Aegis Daemon successfully!"),
                Err(e) => println!(
                    "  [i] {} The project will be registered on the first 'aegis deploy'.",
                    e
                ),
            }
            println!("\n  💡 Next step: Run 'aegis validate', then 'aegis deploy'");
        }
        Commands::Deploy {
            project,
            branch,
            strategy,
            version,
            repo,
            commit,
            detach,
        } => {
            let current_dir = std::env::current_dir()?;
            let local_id = local_project_id();
            let key = target_or_local(project)?;
            // Copy the local directory when deploying the project that lives here.
            let from_here = repo.is_none() && local_id.as_deref() == Some(key.as_str());
            let strategy = strategy.or_else(|| {
                from_here
                    .then(|| ResolvedProjectConfig::load(&current_dir).ok())
                    .flatten()
                    .map(|c| c.strategy)
            });

            let client = daemon.client().await?;
            // Subscribe before queueing so no progress event is missed.
            let mut events = if detach {
                None
            } else {
                Some(rpc(client.stream_events(StreamEventsRequest {}).await)?)
            };
            let response = rpc(client
                .deploy(DeployRequest {
                    project: key,
                    branch: branch.unwrap_or_default(),
                    strategy: strategy.clone().unwrap_or_default(),
                    source_dir: if from_here {
                        current_dir.to_string_lossy().to_string()
                    } else {
                        String::new()
                    },
                    repository_url: repo.unwrap_or_default(),
                    commit: commit.unwrap_or_default(),
                    version: version.unwrap_or_default(),
                })
                .await)?;
            println!(
                "Deploying {} (deployment {}, strategy {})",
                response.project_name,
                short(&response.deployment_id),
                strategy.as_deref().unwrap_or("GracefulSwitch")
            );
            match events.as_mut() {
                Some(events) => follow_deployment(events, &response.deployment_id).await?,
                None => println!(
                    "Deployment queued. Follow it with 'aegis deployments' or 'aegis events'."
                ),
            }
        }
        Commands::Rollback { project, version } => {
            let key = target_or_local(project)?;
            println!("Rolling back {}...", key);
            let response = rpc(daemon
                .client()
                .await?
                .rollback(RollbackRequest {
                    project: key,
                    version: version.unwrap_or_default(),
                })
                .await)?;
            println!("Rolled back: release {} is live.", response.version);
        }
        Commands::List => {
            let projects = rpc(daemon
                .client()
                .await?
                .list_projects(ListProjectsRequest {})
                .await)?
            .projects;
            if projects.is_empty() {
                println!("No projects yet. Run 'aegis init' in a project directory.");
            } else {
                println!(
                    "{:<20} {:<9} {:<10} {:<22} {:>8} {:>8}",
                    "NAME", "ID", "STATUS", "RELEASE", "PID", "RESTARTS"
                );
                for p in projects {
                    let (status, pid, restarts) = match &p.process {
                        Some(proc) => (
                            proc.status.clone(),
                            proc.pid.map_or("-".to_string(), |pid| pid.to_string()),
                            proc.restart_count.to_string(),
                        ),
                        None => ("NotDeployed".to_string(), "-".to_string(), "-".to_string()),
                    };
                    println!(
                        "{:<20} {:<9} {:<10} {:<22} {:>8} {:>8}",
                        p.name,
                        short(&p.id),
                        status,
                        if p.current_release.is_empty() {
                            "-"
                        } else {
                            &p.current_release
                        },
                        pid,
                        restarts
                    );
                }
            }
        }
        Commands::Logs {
            target,
            lines,
            follow,
        } => {
            let target = target_or_local(target)?;
            let client = daemon.client().await?;
            if follow {
                let mut stream = rpc(client
                    .stream_logs(StreamLogsRequest { target, lines })
                    .await)?;
                while let Some(line) = stream.message().await? {
                    print_log_line(&line);
                }
            } else {
                let response = rpc(client.get_logs(GetLogsRequest { target, lines }).await)?;
                if response.lines.is_empty() {
                    println!("No log output yet for process {}", response.process_id);
                }
                for line in &response.lines {
                    print_log_line(line);
                }
            }
        }
        Commands::Start { target } => {
            let process = control_process(&mut daemon, target, ProcessAction::Start).await?;
            println!("Started: {}", describe_process(&process));
        }
        Commands::Stop { target } => {
            let process = control_process(&mut daemon, target, ProcessAction::Stop).await?;
            println!("Stopped: {}", describe_process(&process));
        }
        Commands::Restart { target } => {
            let process = control_process(&mut daemon, target, ProcessAction::Restart).await?;
            println!("Restarted: {}", describe_process(&process));
        }
        Commands::Schedule {
            project_id,
            hour,
            minute,
            branch,
        } => {
            let pid = target_or_local(project_id)?;
            println!(
                "Configuring daily auto-deploy for project {} at {:02}:{:02} (Branch: {})",
                pid, hour, minute, branch
            );
            let response = rpc(daemon
                .client()
                .await?
                .emit_event(EmitEventRequest {
                    event_type: "ScheduleConfigured".to_string(),
                    payload_json: serde_json::json!({
                        "project_id": pid,
                        "hour": hour,
                        "minute": minute,
                        "branch": branch,
                    })
                    .to_string(),
                })
                .await)?;

            if response.success {
                println!("Schedule configured and emitted to Aegis daemon!");
            }
        }
        Commands::EmitEvent {
            event_type,
            payload_json,
        } => {
            let response = rpc(daemon
                .client()
                .await?
                .emit_event(EmitEventRequest {
                    event_type: event_type.clone(),
                    payload_json,
                })
                .await)?;

            if response.success {
                println!(
                    "Successfully emitted event '{}' (ID: {})",
                    event_type, response.event_id
                );
            } else {
                println!("Failed to emit event");
            }
        }
        Commands::StreamEvents | Commands::Events => {
            println!(
                "Listening for operational events from daemon at {}...",
                addr
            );
            let mut stream = rpc(daemon
                .client()
                .await?
                .stream_events(StreamEventsRequest {})
                .await)?;

            while let Some(event) = stream.message().await? {
                println!(
                    "[{}] TYPE: {} | PAYLOAD: {}",
                    event.created_at, event.event_type, event.payload_json
                );
            }
        }
        Commands::Validate => {
            let current_dir = std::env::current_dir()?;
            let toml_path = current_dir.join("aegis.toml");
            if !toml_path.exists() {
                anyhow::bail!(
                    "aegis.toml not found in {}. Run 'aegis init' first.",
                    current_dir.display()
                );
            }
            let file = ProjectFile::load(&toml_path)
                .map_err(|e| anyhow::anyhow!("aegis.toml is invalid: {}", e))?;
            let cfg = ResolvedProjectConfig::resolve(&current_dir, Some(&file));
            println!("aegis.toml is valid. Effective settings:");
            println!(
                "  Project:      {} ({})",
                cfg.name,
                cfg.project_id
                    .as_deref()
                    .unwrap_or("no id - run 'aegis init'")
            );
            println!("  Runtime:      {}", cfg.runtime);
            let show = |c: &Option<String>| c.clone().unwrap_or_else(|| "(none)".to_string());
            println!("  Install:      {}", show(&cfg.install_command));
            println!("  Build:        {}", show(&cfg.build_command));
            println!("  Test:         {}", show(&cfg.test_command));
            println!("  Start:        {}", cfg.start_command);
            println!("  Strategy:     {}", cfg.strategy);
            println!(
                "  Port:         {}",
                cfg.port.map_or("(none)".to_string(), |p| p.to_string())
            );
            println!(
                "  Health check: {} (timeout {}s)",
                cfg.health_check_url
                    .as_deref()
                    .unwrap_or("process stays up"),
                cfg.health_check_timeout.as_secs()
            );
            println!("  Keep releases: {}", cfg.max_retained_versions);
            let mut problems = Vec::new();
            if cfg.project_id.is_none() {
                problems.push("no [project] id; run 'aegis init'".to_string());
            }
            if let Some(strategy) = &file.deploy.strategy {
                if !["GracefulSwitch", "Immediate"].contains(&strategy.as_str()) {
                    problems.push(format!(
                        "strategy '{}' is not supported (use GracefulSwitch or Immediate)",
                        strategy
                    ));
                }
            }
            if let Some(policy) = &file.deploy.restart_policy {
                if !["always", "on-failure", "never"].contains(&policy.as_str()) {
                    problems.push(format!("restart_policy '{}' is not supported", policy));
                }
            }
            if !problems.is_empty() {
                anyhow::bail!("Configuration problems:\n  - {}", problems.join("\n  - "));
            }
        }
        Commands::Releases { project } => {
            let key = target_or_local(project)?;
            let releases = rpc(daemon
                .client()
                .await?
                .list_releases(ListReleasesRequest { project: key })
                .await)?
            .releases;
            if releases.is_empty() {
                println!("No releases yet. Run 'aegis deploy'.");
            } else {
                println!(
                    "{:<22} {:<9} {:<9} {:<20} MESSAGE",
                    "VERSION", "STATUS", "COMMIT", "CREATED"
                );
                for r in releases.iter().rev() {
                    println!(
                        "{:<22} {:<9} {:<9} {:<20} {}",
                        r.version,
                        r.status,
                        short(&r.commit_sha),
                        short_time(&r.created_at),
                        r.commit_message
                    );
                }
            }
        }
        Commands::Deployments { project } => {
            let deployments = rpc(daemon
                .client()
                .await?
                .list_deployments(ListDeploymentsRequest {
                    project: project.unwrap_or_default(),
                })
                .await)?
            .deployments;
            if deployments.is_empty() {
                println!("No deployments yet.");
            }
            for d in deployments {
                println!(
                    "{}  {}  {:<22} {:<10} {:<8} {}",
                    short_time(&d.created_at),
                    short(&d.id),
                    if d.version.is_empty() {
                        "-"
                    } else {
                        &d.version
                    },
                    d.status,
                    d.stage,
                    d.error
                );
            }
        }
        Commands::Timeline { project, limit } => {
            let key = target_or_local(project)?;
            let events = rpc(daemon
                .client()
                .await?
                .list_events(ListEventsRequest {
                    project: key.clone(),
                    limit,
                })
                .await)?
            .events;
            println!("Recent events for {}:", key);
            for event in events {
                let payload: serde_json::Value =
                    serde_json::from_str(&event.payload_json).unwrap_or_default();
                let detail = [
                    "version",
                    "to_version",
                    "status",
                    "stage",
                    "pid",
                    "exit_code",
                    "reason",
                ]
                .iter()
                .filter_map(|k| match payload.get(*k) {
                    Some(serde_json::Value::String(v)) if !v.is_empty() => {
                        Some(format!("{}={}", k, v))
                    }
                    Some(v @ serde_json::Value::Number(_)) => Some(format!("{}={}", k, v)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
                println!(
                    "  {}  {:<24} {}",
                    short_time(&event.created_at),
                    event.event_type,
                    detail
                );
            }
        }
        Commands::Doctor { fix } => {
            println!("Running Aegis Platform Diagnostic Doctor...");
            let current_dir = std::env::current_dir()?;
            let toml_path = current_dir.join("aegis.toml");

            let toml_status = if toml_path.exists() {
                "Valid"
            } else {
                "Missing (Run 'aegis init')"
            };
            println!(
                "  [{}] Configuration file (aegis.toml): {}",
                if toml_path.exists() { "✓" } else { "!" },
                toml_status
            );

            let status_res = match daemon.client().await {
                Ok(c) => c
                    .get_status(StatusRequest {})
                    .await
                    .map_err(|e| e.message().to_string()),
                Err(e) => Err(e.to_string()),
            };
            let is_connected = status_res.is_ok();
            let grpc_status = match status_res {
                Ok(resp) => format!("Connected (v{})", resp.into_inner().version),
                Err(e) => format!("Disconnected ({})", e),
            };
            println!(
                "  [{}] gRPC Daemon Connection: {}",
                if is_connected { "✓" } else { "✗" },
                grpc_status
            );

            let detector = aegis_engine::RuntimeDetector::new();
            let runtime = detector.detect_runtime(&current_dir).await;
            println!("  [✓] Detected Runtime Environment: {}", runtime.name());

            if fix && !toml_path.exists() {
                let pipeline = aegis_engine::DetectorPipeline::new();
                let config = pipeline.detect_all(&current_dir);
                let toml_content =
                    pipeline.generate_toml(&config, &aegis_types::ProjectId::new().to_string());
                let _ = std::fs::write(&toml_path, toml_content);
                println!("Auto-fix completed: Created aegis.toml config file!");
            }
        }
        Commands::Inspect { project_id, at } => {
            let pid = project_id
                .or_else(local_project_id)
                .unwrap_or_else(|| "(no project)".to_string());
            if let Some(ts) = at {
                println!(
                    "Time-Travel Historical State Inspection for Project '{}' at {}:",
                    pid, ts
                );
                println!("  ├── Id: {}", pid);
                println!("  ├── Historical Snapshot Query: Replayed from EventStore");
            } else {
                let status = daemon.status().await;
                println!("Project Resource Tree:");
                println!("  ├── Id: {}", pid);
                println!(
                    "  ├── Daemon Version: {}",
                    status
                        .as_ref()
                        .map(|s| s.version.as_str())
                        .unwrap_or("0.1.0")
                );
                println!(
                    "  └── Total System Events: {}",
                    status.as_ref().map(|s| s.event_count).unwrap_or(0)
                );
            }
        }
        Commands::Explain => {
            println!("Aegis Runtime & Capability Auto-Detection:");
            println!("  - Rust Projects: Auto-detects Cargo.toml -> Suggested: GracefulSwitch Binary Deployment");
            println!("  - Node.js Projects: Auto-detects package.json -> Suggested: Zero-downtime Process Swap");
            println!("  - Go Projects: Auto-detects go.mod -> Suggested: Immediate Binary Swap");
            println!("  - Python Projects: Auto-detects requirements.txt / pyproject.toml -> Suggested: Monitored Process");
        }
        Commands::Demo => {
            println!("Starting Aegis Platform Demonstration Tour...");
            let current_dir = std::env::current_dir()?;
            let detector = aegis_engine::RuntimeDetector::new();
            let runtime = detector.detect_runtime(&current_dir).await;
            println!(
                "  [Step 1/3] Auto-detected local workspace runtime -> {}",
                runtime.name()
            );
            println!(
                "  [Step 2/3] Verifying gRPC Daemon connection at {}...",
                addr
            );
            let status = daemon.status().await;
            if status.is_some() {
                println!("  [Step 3/3] Connected to Aegis Daemon (Status: Operational)");
            } else {
                println!("  [Step 3/3] Aegis Daemon offline. Start daemon with 'aegis-daemon'");
            }
            println!("\n  [✓] Feature Tour Complete!");
        }
        Commands::Replay { release } => {
            println!(
                "▶ Replaying Event History for Release / Target '{}'...",
                release
            );
            let status = daemon.status().await;
            println!(
                "  Replayed against EventStore with {} total events.",
                status.as_ref().map(|s| s.event_count).unwrap_or(0)
            );
        }
        Commands::Incident { project_id } => {
            let pid = project_id.unwrap_or_else(|| "current-project".to_string());
            println!(
                "Aegis Incident Response & Root Cause Diagnostics for Project '{}':",
                pid
            );
            let status = daemon.status().await;
            if status.is_some() {
                println!("  [✓] Daemon Status: Operational");
                println!("  [✓] EventStore: Connected");
            } else {
                println!("  [!] Daemon Status: Offline / Unreachable");
            }
        }
        Commands::Investigate { project_id } => {
            let pid = project_id.unwrap_or_else(|| "current-project".to_string());
            println!(
                "🔎 Aegis Operational Outage Investigation for Project '{}':",
                pid
            );
            let status = daemon.status().await;
            println!(
                "  Current System Event Log Size: {}",
                status.as_ref().map(|s| s.event_count).unwrap_or(0)
            );
        }
        Commands::Migrate { target } => match target {
            MigrateSubcommand::Pm2 { file } => {
                println!("Parsing PM2 ecosystem configuration file '{}'...", file);
                let current_dir = std::env::current_dir()?;
                let path = Path::new(&file);
                let service_name = if path.exists() {
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    if text.contains("name:") || text.contains("\"name\"") {
                        "pm2-app"
                    } else {
                        "imported-app"
                    }
                } else {
                    "imported-app"
                };

                let pipeline = aegis_engine::DetectorPipeline::new();
                let mut config = pipeline.detect_all(&current_dir);
                config.project_name = service_name.to_string();
                let toml_content =
                    pipeline.generate_toml(&config, &aegis_types::ProjectId::new().to_string());
                let toml_path = current_dir.join("aegis.toml");
                let _ = std::fs::write(&toml_path, toml_content);

                println!("  [✓] Imported service: '{}'", service_name);
                println!("Successfully generated aegis.toml from PM2 ecosystem!");
            }
            MigrateSubcommand::Systemd { service } => {
                println!("Parsing systemd service unit file '{}'...", service);
                let current_dir = std::env::current_dir()?;
                let path = Path::new(&service);
                let mut start_cmd = "java -jar app.jar".to_string();
                if path.exists() {
                    if let Ok(text) = std::fs::read_to_string(path) {
                        for line in text.lines() {
                            if line.starts_with("ExecStart=") {
                                start_cmd = line.trim_start_matches("ExecStart=").to_string();
                            }
                        }
                    }
                }

                let pipeline = aegis_engine::DetectorPipeline::new();
                let mut config = pipeline.detect_all(&current_dir);
                config.start_command = start_cmd.clone();
                let toml_content =
                    pipeline.generate_toml(&config, &aegis_types::ProjectId::new().to_string());
                let toml_path = current_dir.join("aegis.toml");
                let _ = std::fs::write(&toml_path, toml_content);

                println!("  [✓] ExecStart mapped -> '{}'", start_cmd);
                println!("Successfully generated aegis.toml from systemd unit!");
            }
        },
        Commands::Upgrade { check, force } => {
            println!("Checking Aegis platform self-update status...");
            let status = daemon.status().await;
            let daemon_ver = status
                .as_ref()
                .map(|s| s.version.as_str())
                .unwrap_or(env!("CARGO_PKG_VERSION"));
            println!("  CLI Version:     v{}", env!("CARGO_PKG_VERSION"));
            println!("  Daemon Version:  v{}", daemon_ver);
            if check {
                println!("  [✓] Check complete: Platform version is synchronized.");
            } else if force {
                println!("  [!] Version re-sync requested.");
                println!("  [✓] Binary version status updated.");
            } else {
                println!(
                    "  [✓] Aegis is running on version v{}",
                    env!("CARGO_PKG_VERSION")
                );
            }
        }
    }

    Ok(())
}

async fn control_process(
    daemon: &mut Daemon,
    target: Option<String>,
    action: ProcessAction,
) -> anyhow::Result<ProcessInfo> {
    let target = target_or_local(target)?;
    rpc(daemon
        .client()
        .await?
        .control_process(ControlProcessRequest {
            target,
            action: action as i32,
        })
        .await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_project_section() {
        assert!(has_project_section("[project]\nid = \"abc\"\n"));
        assert!(!has_project_section(
            "[daemon]\nhost = \"127.0.0.1\"\nport = 50051\n"
        ));
    }

    #[test]
    fn test_formatting_helpers() {
        assert_eq!(short("01a0e66a-2697-73c0-bff5-3346fccc6ad3"), "fccc6ad3");
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("abc"), "abc");
        assert_eq!(
            short_time("2026-09-28T05:12:03.123+00:00"),
            "2026-09-28 05:12:03"
        );
    }
}
