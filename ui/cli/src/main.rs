use aegis_api::aegis::aegis_daemon_client::AegisDaemonClient;
use aegis_api::aegis::{
    ConfigureScheduleRequest, ControlProcessRequest, DeployRequest, DeploymentInfo,
    EmitEventRequest, GetLogsRequest, ListDeploymentsRequest, ListEventsRequest,
    ListProjectsRequest, ListReleasesRequest, LogLine, ProcessAction, ProcessInfo,
    RegisterProjectRequest, RollbackRequest, StatusRequest, StatusResponse, StreamEventsRequest,
    StreamLogsRequest,
};
use aegis_config::{Config, ProjectFile};
use aegis_engine::ResolvedProjectConfig;
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use tonic::transport::Channel;

mod diagnose;
mod history;
mod migrate;
mod upgrade;

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
    /// Configure a daily auto-deployment (UTC time)
    Schedule {
        /// Target project ID (reads from aegis.toml if omitted)
        project_id: Option<String>,
        /// Target deployment hour (0-23)
        #[arg(short, long)]
        hour: u32,
        /// Target deployment minute (0-59)
        #[arg(short, long, default_value = "0")]
        minute: u32,
        /// Branch to deploy (defaults to the project's branch)
        #[arg(short, long)]
        branch: Option<String>,
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
    /// Show a project's state: releases, process, schedule, settings, limits
    /// and recent deployments, now or as it was at a past time [EXPERIMENTAL]
    Inspect {
        /// Project id or name (defaults to ./aegis.toml)
        project_id: Option<String>,
        /// Replay history up to this time (RFC 3339, e.g. 2026-08-03T09:15:00Z; UTC if no offset)
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
    /// Show what happened during a deployment, step by step [EXPERIMENTAL]
    Replay {
        /// Deployment id (or its short form), release version or release id
        target: String,
        /// Only look in this project (id or name)
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Diagnose a project's process, crashes and failed deployments, and
    /// suggest what to do [EXPERIMENTAL]
    Incident {
        /// Project id or name (defaults to ./aegis.toml)
        project_id: Option<String>,
        /// How far back to look for crashes and alerts (e.g. 30m, 2h, 1d)
        #[arg(short, long, default_value = "24h")]
        since: String,
    },
    /// Timeline of a project's recent events with the likely trigger of
    /// any problems [EXPERIMENTAL]
    Investigate {
        /// Project id or name (defaults to ./aegis.toml)
        project_id: Option<String>,
        /// How far back to look (e.g. 30m, 2h, 1d)
        #[arg(short, long, default_value = "24h")]
        since: String,
    },
    /// Generate aegis.toml from a PM2 ecosystem file or a systemd unit [EXPERIMENTAL]
    Migrate {
        #[command(subcommand)]
        target: MigrateSubcommand,
    },
    /// Update the Aegis binaries to the latest GitHub release with install.sh
    /// (Linux and macOS) [EXPERIMENTAL]
    Upgrade {
        /// Only report whether a newer release exists
        #[arg(short, long)]
        check: bool,
        /// Reinstall even if this version is current
        #[arg(short, long)]
        force: bool,
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum MigrateSubcommand {
    /// Read a PM2 ecosystem file (.json, or .js on a best-effort basis)
    Pm2 {
        /// Ecosystem file (defaults to ./ecosystem.config.{js,cjs,json})
        #[arg(short, long)]
        file: Option<String>,
        /// App to migrate when the file defines several
        #[arg(short, long)]
        app: Option<String>,
        #[command(flatten)]
        output: MigrateOutput,
    },
    /// Read a systemd service unit
    Systemd {
        /// Path to the .service file
        #[arg(short, long)]
        service: String,
        #[command(flatten)]
        output: MigrateOutput,
    },
}

#[derive(clap::Args)]
struct MigrateOutput {
    /// Print the generated aegis.toml instead of writing it
    #[arg(long)]
    dry_run: bool,
    /// Overwrite an existing aegis.toml (its project id is kept)
    #[arg(long)]
    force: bool,
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

/// 536870912 -> "512 MiB"
fn bytes(n: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let mib = n as f64 / MIB;
    if mib >= 1024.0 {
        format!("{:.1} GiB", mib / 1024.0)
    } else {
        format!("{:.0} MiB", mib)
    }
}

/// Every recorded event of a project, oldest first.
async fn project_history(daemon: &mut Daemon, project: &str) -> anyhow::Result<Vec<history::Ev>> {
    let events = rpc(daemon
        .client()
        .await?
        .list_events(ListEventsRequest {
            project: project.to_string(),
            // Daemons that predate `all` treat this as "everything".
            limit: u32::MAX,
            all: true,
        })
        .await)?
    .events;
    Ok(history::from_responses(events))
}

/// The one project in a replayed single-project history.
fn only_project(
    engine: &aegis_projection::ProjectionEngine,
    key: &str,
) -> anyhow::Result<aegis_projection::ProjectState> {
    engine
        .get_projects()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No history recorded for project '{}'", key))
}

/// Deployments matching a deployment id (full, or as shown by `short`) or a
/// release version. Id matches win over version matches.
fn match_deployments<'a>(
    deployments: &'a [DeploymentInfo],
    target: &str,
) -> Vec<&'a DeploymentInfo> {
    let by_id: Vec<_> = deployments
        .iter()
        .filter(|d| {
            d.id == target
                || (target.len() >= 6 && d.id.ends_with(target))
                || (target.len() >= 8 && d.id.starts_with(target))
        })
        .collect();
    if !by_id.is_empty() {
        return by_id;
    }
    let by_version: Vec<_> = deployments.iter().filter(|d| d.version == target).collect();
    // A version is only built once; prefer the deployment that succeeded.
    match by_version.iter().rev().find(|d| d.status == "Success") {
        Some(d) => vec![*d],
        None => by_version.last().map(|d| vec![*d]).unwrap_or_default(),
    }
}

/// Asks a yes/no question on the terminal; anything but "y"/"yes" is no.
fn confirm(question: &str) -> bool {
    use std::io::Write;
    print!("{} [y/N] ", question);
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Prints or writes a migrated aegis.toml.
fn write_migration(mut m: migrate::Migration, output: &MigrateOutput) -> anyhow::Result<()> {
    let path = std::env::current_dir()?.join("aegis.toml");
    let exists = path.exists();
    if exists && !output.force && !output.dry_run {
        anyhow::bail!(
            "aegis.toml already exists in this directory. Use --dry-run to see the result or --force to overwrite it."
        );
    }
    // Keep the project's identity when replacing its aegis.toml.
    if exists {
        if let Some(id) = ProjectFile::load(&path).ok().and_then(|f| f.project.id) {
            m.file.project.id = Some(id);
        }
    }
    for warning in &m.warnings {
        eprintln!("  ! {}", warning);
    }
    let text = m.render()?;
    if output.dry_run {
        print!("{}", text);
        return Ok(());
    }
    std::fs::write(&path, text)?;
    println!(
        "Wrote aegis.toml from {} (start: {}).",
        m.source,
        m.file.build.start_command.as_deref().unwrap_or("-")
    );
    println!("  Next: 'aegis validate', then 'aegis deploy'.");
    Ok(())
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
            if response.web_url.is_empty() {
                println!("  HTTP API:    disabled (set [web] enabled = true)");
            } else {
                println!("  HTTP API:    {}/api/v1", response.web_url);
            }
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
            let project = target_or_local(project_id)?;
            let updated = rpc(daemon
                .client()
                .await?
                .configure_schedule(ConfigureScheduleRequest {
                    project,
                    hour,
                    minute,
                    branch: branch.unwrap_or_default(),
                })
                .await)?;
            println!(
                "{} will deploy daily at {:02}:{:02} UTC (branch {}).",
                updated.name, hour, minute, updated.branch
            );
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
                    all: false,
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
            inspect(&mut daemon, target_or_local(project_id)?, at).await?;
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
        Commands::Replay { target, project } => {
            replay(&mut daemon, &target, project).await?;
        }
        Commands::Incident { project_id, since } => {
            incident(&mut daemon, target_or_local(project_id)?, &since).await?;
        }
        Commands::Investigate { project_id, since } => {
            investigate(&mut daemon, target_or_local(project_id)?, &since).await?;
        }
        Commands::Migrate { target } => match target {
            MigrateSubcommand::Pm2 { file, app, output } => {
                let file = match file {
                    Some(f) => f,
                    None => [
                        "ecosystem.config.js",
                        "ecosystem.config.cjs",
                        "ecosystem.config.json",
                        "ecosystem.json",
                    ]
                    .into_iter()
                    .find(|f| Path::new(f).exists())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "No ecosystem.config.{{js,cjs,json}} here. Pass the file with --file."
                        )
                    })?
                    .to_string(),
                };
                let text = std::fs::read_to_string(&file)
                    .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", file, e))?;
                write_migration(migrate::from_pm2(&file, &text, app.as_deref())?, &output)?;
            }
            MigrateSubcommand::Systemd { service, output } => {
                let path = Path::new(&service);
                let text = std::fs::read_to_string(path)
                    .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", service, e))?;
                let read = |p: &Path| std::fs::read_to_string(p).ok();
                write_migration(migrate::from_systemd(path, &text, &read)?, &output)?;
            }
        },
        Commands::Upgrade { check, force, yes } => {
            let current = env!("CARGO_PKG_VERSION");
            println!("This CLI:       v{}", current);
            if let Some(status) = daemon.status().await {
                println!("Daemon:         v{}", status.version);
            }
            let latest = upgrade::fetch_latest_tag()?;
            println!("Latest release: {}", latest);
            let plan = upgrade::plan(&latest, current)?;
            match plan {
                upgrade::Plan::UpToDate => println!("Aegis is up to date."),
                upgrade::Plan::Upgrade => println!("A newer release is available."),
                upgrade::Plan::Ahead => {
                    println!("This build is newer than the latest release (development build).")
                }
            }
            if check {
                if plan == upgrade::Plan::Upgrade {
                    println!("Install it with 'aegis upgrade'.");
                }
                return Ok(());
            }
            if plan != upgrade::Plan::Upgrade && !force {
                return Ok(());
            }
            if cfg!(windows) {
                anyhow::bail!(
                    "Self-update is not supported on Windows. Download {} from https://github.com/{}/releases",
                    latest,
                    upgrade::REPO
                );
            }
            let exe = std::env::current_exe()?;
            if upgrade::is_server_install(&exe) {
                println!(
                    "Aegis is installed as a system service ({}). Upgrade it with:",
                    exe.display()
                );
                println!(
                    "  curl -fsSL {} | sudo bash -s -- --server   (add --domain NAME if you used it)",
                    upgrade::INSTALL_SCRIPT_URL
                );
                println!("  sudo systemctl restart aegis");
                return Ok(());
            }
            println!(
                "This downloads {} and runs it with bash; it installs {} into ~/.local/bin.",
                upgrade::INSTALL_SCRIPT_URL,
                latest
            );
            println!("  (this binary: {})", exe.display());
            if !yes && !confirm("Continue?") {
                println!("Cancelled. Pass --yes to skip this question.");
                return Ok(());
            }
            upgrade::run_installer()?;
            println!(
                "Installed {}. Restart the daemon (stop it, then run 'aegis-daemon') to use the new version.",
                latest
            );
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

/// `aegis inspect`: the project's state folded from its event history,
/// either all of it or only what was recorded up to `at`.
async fn inspect(daemon: &mut Daemon, key: String, at: Option<String>) -> anyhow::Result<()> {
    let at = at.as_deref().map(history::parse_at).transpose()?;
    let events = project_history(daemon, &key).await?;
    let (engine, used) = history::replay_until(&events, at);
    let Some(project) = engine.get_projects().into_iter().next() else {
        let created = events
            .iter()
            .find(|e| e.event_type == "ProjectCreated")
            .or(events.first());
        match (at, created) {
            (Some(t), Some(c)) => anyhow::bail!(
                "Project '{}' did not exist yet at {} (it was registered at {}).",
                key,
                t.format("%Y-%m-%d %H:%M:%S UTC"),
                c.at.format("%Y-%m-%d %H:%M:%S UTC")
            ),
            _ => anyhow::bail!("No history recorded for project '{}'", key),
        }
    };

    match at {
        Some(t) => {
            println!(
                "Project {} as of {} (replayed {} of {} events)",
                project.name,
                t.format("%Y-%m-%d %H:%M:%S UTC"),
                used,
                events.len()
            );
            if t > chrono::Utc::now() {
                println!("  (that time is in the future: this is the current state)");
            }
        }
        None => println!("Project {} (replayed {} events)", project.name, used),
    }
    println!("  Id:           {}", project.id);
    println!("  Registered:   {}", short_time(&project.created_at));
    if !project.repository_url.is_empty() {
        println!(
            "  Repository:   {} (branch {})",
            project.repository_url, project.branch
        );
    }
    if let Some(dir) = &project.source_dir {
        println!("  Source dir:   {}", dir);
    }
    println!(
        "  Runtime:      {}",
        project.runtime.as_deref().unwrap_or("(detected at build)")
    );
    println!(
        "  Live release: {}",
        project.current_release.as_deref().unwrap_or("none")
    );
    let process = project
        .current_process
        .and_then(|id| engine.get_process(&id));
    match &process {
        Some(p) => {
            println!(
                "  Process:      {} (pid {}, wanted {}, restarts {}, last exit code {})",
                p.status,
                p.pid.map_or("-".to_string(), |pid| pid.to_string()),
                p.desired,
                p.restart_count,
                p.last_exit_code.map_or("-".to_string(), |c| c.to_string())
            );
            println!(
                "                {} since {}",
                p.command.as_deref().unwrap_or("-"),
                p.last_start.as_deref().map(short_time).unwrap_or_default()
            );
        }
        None => println!("  Process:      none"),
    }
    match &project.schedule {
        Some(s) => println!(
            "  Schedule:     daily at {:02}:{:02} UTC (branch {})",
            s.hour, s.minute, s.branch
        ),
        None => println!("  Schedule:     none"),
    }
    let settings = serde_json::to_value(&project.settings).unwrap_or_default();
    let overrides: Vec<String> = settings
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| match v {
                    serde_json::Value::String(s) => format!("{}={}", k, s),
                    other => format!("{}={}", k, other),
                })
                .collect()
        })
        .unwrap_or_default();
    if overrides.is_empty() {
        println!("  Settings:     from aegis.toml (no overrides)");
    } else {
        println!("  Settings:     {}", overrides.join(", "));
    }
    let r = &project.resources;
    if r.is_unlimited() {
        println!("  Limits:       unlimited");
    } else {
        println!(
            "  Limits:       cpu {}, memory {}, processes {}",
            r.cpu_cores
                .map_or("unlimited".to_string(), |c| format!("{} cores", c)),
            r.memory_bytes.map_or("unlimited".to_string(), bytes),
            r.pids_max
                .map_or("unlimited".to_string(), |p| p.to_string())
        );
    }

    let releases = engine.get_project_releases(&project.id);
    println!("Releases ({}):", releases.len());
    for rel in releases.iter().rev() {
        println!(
            "  {:<22} {:<9} {:<9} {}",
            rel.version,
            rel.status,
            short(&rel.commit_sha),
            short_time(&rel.created_at)
        );
    }
    let deployments: Vec<_> = engine
        .get_deployments()
        .into_iter()
        .filter(|d| d.project_id == project.id)
        .collect();
    let skip = deployments.len().saturating_sub(5);
    println!("Recent deployments ({} total):", deployments.len());
    for d in &deployments[skip..] {
        println!(
            "  {}  {}  {:<22} {:<10} {:<8} {}",
            short_time(&d.created_at),
            short(&d.id.to_string()),
            d.version.as_deref().unwrap_or("-"),
            d.status,
            d.stage.as_deref().unwrap_or(""),
            d.error.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

/// `aegis replay`: one deployment's events with time offsets.
async fn replay(daemon: &mut Daemon, target: &str, project: Option<String>) -> anyhow::Result<()> {
    let client = daemon.client().await?;
    let deployments = rpc(client
        .list_deployments(ListDeploymentsRequest {
            project: project.clone().unwrap_or_default(),
        })
        .await)?
    .deployments;
    let projects = rpc(client.list_projects(ListProjectsRequest {}).await)?.projects;
    let mut found = match_deployments(&deployments, target);

    // Maybe a release id: find its version, then the deployment that built it.
    if found.is_empty() {
        for p in &projects {
            if project.as_ref().is_some_and(|k| *k != p.id && *k != p.name) {
                continue;
            }
            let releases = rpc(client
                .list_releases(ListReleasesRequest {
                    project: p.id.clone(),
                })
                .await)?
            .releases;
            if let Some(r) = releases
                .iter()
                .find(|r| r.id == target || (target.len() >= 6 && r.id.ends_with(target)))
            {
                let own: Vec<DeploymentInfo> = deployments
                    .iter()
                    .filter(|d| d.project_id == p.id)
                    .cloned()
                    .collect();
                let matched: Vec<DeploymentInfo> = match_deployments(&own, &r.version)
                    .into_iter()
                    .cloned()
                    .collect();
                if let Some(d) = matched.into_iter().next() {
                    found = deployments.iter().filter(|x| x.id == d.id).collect();
                }
                break;
            }
        }
    }
    let deployment = match found.as_slice() {
        [] => anyhow::bail!(
            "No deployment, release version or release id matches '{}'. See 'aegis deployments'.",
            target
        ),
        [one] => (*one).clone(),
        many => anyhow::bail!(
            "'{}' matches several deployments: {}. Use more of the id.",
            target,
            many.iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let name = projects
        .iter()
        .find(|p| p.id == deployment.project_id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| deployment.project_id.clone());
    let events = project_history(daemon, &deployment.project_id).await?;
    println!(
        "Deployment {} of {}: release {}, strategy {}, trigger {}, queued {}",
        short(&deployment.id),
        name,
        if deployment.version.is_empty() {
            "-"
        } else {
            &deployment.version
        },
        deployment.strategy,
        if deployment.trigger.is_empty() {
            "-"
        } else {
            &deployment.trigger
        },
        short_time(&deployment.created_at)
    );
    match diagnose::replay_lines(&events, &deployment.id) {
        Some(lines) => {
            for line in lines {
                println!("{}", line);
            }
        }
        None => println!("No events were recorded for this deployment."),
    }
    Ok(())
}

/// `aegis incident`: what is wrong with a project right now, and what to do.
async fn incident(daemon: &mut Daemon, key: String, since: &str) -> anyhow::Result<()> {
    let span = history::parse_duration(since)?;
    let from = chrono::Utc::now() - span;
    let events = project_history(daemon, &key).await?;
    let (engine, _) = history::replay_until(&events, None);
    let project = only_project(&engine, &key)?;
    let report = diagnose::analyze_incident(&engine, &project, &events, from);
    let p = &project.name;

    println!("Incident report for {} (last {})", p, since);
    match &report.process {
        Some(proc) => {
            println!(
                "  Process:      {} (pid {}, release {}, restarts {}, last exit code {})",
                proc.status,
                proc.pid.map_or("-".to_string(), |pid| pid.to_string()),
                proc.release_version.as_deref().unwrap_or("-"),
                proc.restart_count,
                proc.last_exit_code
                    .map_or("-".to_string(), |c| c.to_string())
            );
            if proc.desired == "stopped" {
                println!("                stopped on purpose by an operator");
            }
        }
        None => println!("  Process:      none (never deployed)"),
    }
    println!(
        "  Live release: {}{}",
        project.current_release.as_deref().unwrap_or("none"),
        report
            .rollback_target
            .as_ref()
            .map_or(String::new(), |r| format!(" (previous: {})", r))
    );

    let alerts = report
        .problems
        .iter()
        .filter(|e| e.event_type == "ResourcePressureDetected")
        .count();
    println!(
        "  In window:    {} crash(es), {} crash-loop failure(s), {} resource alert(s), {} failed deployment(s)",
        report.crashes,
        report
            .problems
            .iter()
            .filter(|e| e.event_type == "ProcessFailed")
            .count(),
        alerts,
        report
            .problems
            .iter()
            .filter(|e| e.event_type == "DeploymentFailed")
            .count()
    );
    if !report.problems.is_empty() {
        let skip = report.problems.len().saturating_sub(10);
        println!("Recent problems:");
        for e in &report.problems[skip..] {
            println!("  {}", diagnose::timeline_line(e));
        }
    }

    if let Some((d, event)) = &report.failed_deployment {
        println!(
            "Last failed deployment{}: {} ({}) at {}, stage {}: {}",
            if report.latest_deployment_failed {
                " (the latest one)"
            } else {
                ""
            },
            short(&d.id.to_string()),
            d.version.as_deref().unwrap_or("-"),
            short_time(d.completed_at.as_deref().unwrap_or(&d.created_at)),
            d.stage.as_deref().unwrap_or("-"),
            d.error.as_deref().unwrap_or("-")
        );
        let tail = event.as_ref().map(|e| e.log_tail()).unwrap_or_default();
        for line in tail.iter().skip(tail.len().saturating_sub(10)) {
            println!("    | {}", line);
        }
    }

    if report.process.is_some() {
        let logs = daemon
            .client()
            .await?
            .get_logs(GetLogsRequest {
                target: project.id.to_string(),
                lines: 15,
            })
            .await;
        if let Ok(logs) = logs {
            let lines = logs.into_inner().lines;
            if !lines.is_empty() {
                println!("Last process output:");
                for line in &lines {
                    print!("  ");
                    print_log_line(line);
                }
            }
        }
    }

    println!("What to do:");
    for (i, advice) in report.advice.iter().enumerate() {
        println!("  {}. {}", i + 1, advice);
    }
    Ok(())
}

/// `aegis investigate`: a project's events in a window, with the change that
/// most likely set off any problems.
async fn investigate(daemon: &mut Daemon, key: String, since: &str) -> anyhow::Result<()> {
    let span = history::parse_duration(since)?;
    let from = chrono::Utc::now() - span;
    let events = project_history(daemon, &key).await?;
    let (engine, _) = history::replay_until(&events, None);
    let project = only_project(&engine, &key)?;
    // Successful build steps are noise here; `aegis replay` shows them.
    let shown: Vec<&history::Ev> = history::window(&events, from)
        .iter()
        .filter(|e| !e.event_type.starts_with("BuildStage") || e.str("status") == "Failed")
        .collect();

    println!(
        "Timeline for {} since {} ({}); * marks changes, ! marks problems:",
        project.name,
        from.format("%Y-%m-%d %H:%M:%S UTC"),
        since
    );
    if shown.is_empty() {
        println!("  (no events)");
    }
    const MAX_ROWS: usize = 200;
    let skip = shown.len().saturating_sub(MAX_ROWS);
    if skip > 0 {
        println!("  ... {} earlier events not shown", skip);
    }
    for e in &shown[skip..] {
        println!("{}", diagnose::timeline_line(e));
    }
    println!("Summary:");
    for line in diagnose::investigate_summary(&events, from) {
        println!("  {}", line);
    }
    Ok(())
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
    #[test]
    fn test_match_deployments() {
        let dep = |id: &str, version: &str, status: &str| DeploymentInfo {
            id: id.to_string(),
            version: version.to_string(),
            status: status.to_string(),
            ..Default::default()
        };
        let deps = vec![
            dep("01a0e66a-2697-73c0-bff5-3346fccc6ad3", "r1", "Success"),
            dep("01a0e66a-2697-73c0-bff5-3346fccc7777", "r2", "Failed"),
            dep("01a0e66a-2697-73c0-bff5-3346fccc8888", "r2", "Success"),
            dep("01a0e66a-2697-73c0-bff5-3346fccc9999", "r3", "Failed"),
        ];
        let ids = |m: Vec<&DeploymentInfo>| m.iter().map(|d| d.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(match_deployments(&deps, "fccc6ad3")),
            vec![deps[0].id.clone()]
        );
        assert_eq!(
            ids(match_deployments(&deps, &deps[1].id)),
            vec![deps[1].id.clone()]
        );
        assert_eq!(
            ids(match_deployments(&deps, "r2")),
            vec![deps[2].id.clone()]
        );
        assert_eq!(
            ids(match_deployments(&deps, "r3")),
            vec![deps[3].id.clone()]
        );
        // A shared prefix is ambiguous.
        assert_eq!(match_deployments(&deps, "01a0e66a-2697").len(), 4);
        assert!(match_deployments(&deps, "nope").is_empty());
        // Too short to be an id.
        assert!(match_deployments(&deps, "6ad3").is_empty());
    }

    #[test]
    fn test_bytes() {
        assert_eq!(bytes(512 * 1024 * 1024), "512 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GiB");
    }
}
