mod service;

use aegis_api::aegis::aegis_daemon_server::AegisDaemonServer;
use aegis_auth::{AuthConfig, AuthStore, TokenScope, ADMIN_USER};
use aegis_config::Config;
use aegis_control::{ControlPlane, ControlSettings};
use aegis_event_store::EventStore;
use aegis_plugins::PluginManager;
use aegis_process::ProcessSupervisor;
use async_trait::async_trait;
use service::{DaemonService, DAEMON_VERSION};
use sqlx::sqlite::SqlitePoolOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tonic::transport::Server;

// A Demo System Plugin that implements the dynamic Plugin trait
struct DemoPlugin;

#[async_trait]
impl aegis_plugins::Plugin for DemoPlugin {
    fn name(&self) -> &str {
        "demo-system-plugin"
    }

    async fn on_init(&self) -> Result<(), anyhow::Error> {
        tracing::info!("DemoPlugin: System plugin initialized!");
        Ok(())
    }

    async fn on_event(&self, event: &aegis_types::Event) -> Result<(), anyhow::Error> {
        tracing::debug!(
            event_id = %event.id,
            event_type = %event.event_type,
            "DemoPlugin: Received operational event"
        );
        Ok(())
    }
}

/// Creates the admin account on first start and tells the operator where
/// its password is. Once the password has been changed, the file is removed.
async fn prepare_admin_account(config: &Config, auth: &AuthStore) -> Result<(), anyhow::Error> {
    let path = initial_password_path(config);
    if let Some(password) = auth.bootstrap_admin().await? {
        write_secret_file(&path, &password)?;
        tracing::warn!(
            path = %path.display(),
            "Created the '{}' account. Its password is in this file (readable only by the daemon's user). \
             Sign in and change it, or run `aegis-daemon admin reset-password`.",
            ADMIN_USER
        );
    } else if let Ok(content) = std::fs::read_to_string(&path) {
        if !auth.verify_login(ADMIN_USER, content.trim()).await? {
            let _ = std::fs::remove_file(&path);
            tracing::info!(path = %path.display(), "Initial admin password was changed; removed the file");
        }
    }
    Ok(())
}

/// Registers a `ScheduleConfigured` event with the scheduler.
fn apply_schedule(scheduler: &aegis_scheduler::SchedulerEngine, event: &aegis_types::Event) {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.payload_json) else {
        return;
    };
    let Some(project_id) = payload["project_id"]
        .as_str()
        .and_then(|s| s.parse::<aegis_types::ProjectId>().ok())
    else {
        return;
    };
    let hour = payload["hour"].as_u64().unwrap_or(0) as u32;
    let minute = payload["minute"].as_u64().unwrap_or(0) as u32;
    let branch = payload["branch"].as_str().unwrap_or("main").to_string();
    scheduler.schedule_daily_deploy(project_id, hour, minute, branch);
}

/// Resolves on Ctrl-C or SIGTERM (what systemd and `kill` send).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

const USAGE: &str = "Usage:
  aegis-daemon [run]                     Start the daemon
  aegis-daemon check-config              Validate the configuration and exit
  aegis-daemon admin reset-password [--stdin]
                                         Set a new admin password (random, or read from stdin)
                                         and sign out all sessions
  aegis-daemon admin create-token --name NAME [--scope read|deploy]
                                         Create an API token for automation

The configuration is read from $AEGIS_CONFIG (default: ./aegis.toml).";

/// Where the first-boot admin password is written (mode 0600).
fn initial_password_path(config: &Config) -> PathBuf {
    config
        .daemon
        .resolved_data_dir()
        .join("initial-admin-password")
}

fn write_secret_file(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    // `mode` only applies when creating; fix up a pre-existing file too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(content.as_bytes())?;
    file.write_all(b"\n")
}

#[tokio::main]
async fn main() {
    // Report startup errors as a message, not as Debug output with a backtrace.
    if let Err(e) = run().await {
        eprintln!("aegis-daemon: {:#}", e);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), anyhow::Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // 1. Load config. The daemon must not create aegis.toml itself: `aegis init`
    // owns that file, and a pre-existing daemon-only file made init skip writing
    // the [project] section, so every deploy got a fresh random project ID.
    let config_path = std::env::var("AEGIS_CONFIG").unwrap_or_else(|_| "aegis.toml".to_string());
    let config = Config::load(&config_path)?;
    validate_config(&config)?;

    match args.as_slice() {
        [] | ["run"] => run_daemon(config).await,
        ["check-config"] => {
            println!(
                "Configuration OK ({}). HTTP API: {}",
                config_path,
                describe_web(&config)
            );
            Ok(())
        }
        ["admin", rest @ ..] => admin(&config, rest).await,
        ["-h" | "--help" | "help"] => {
            println!("{}", USAGE);
            Ok(())
        }
        ["-V" | "--version"] => {
            println!("aegis-daemon {}", DAEMON_VERSION);
            Ok(())
        }
        _ => anyhow::bail!("Unknown arguments {:?}\n\n{}", args, USAGE),
    }
}

fn resource_mode(config: &aegis_config::ResourcesConfig) -> aegis_resources::Mode {
    if config.cgroups == "off" {
        return aegis_resources::Mode::Off;
    }
    match &config.cgroup_root {
        Some(root) => aegis_resources::Mode::V2At(root.clone()),
        None => aegis_resources::Mode::Auto,
    }
}

/// Refuses configurations that would expose the daemon unsafely.
fn validate_config(config: &Config) -> Result<(), anyhow::Error> {
    config.daemon.validate()?;
    config.resources.validate()?;
    config.notifications.validate()?;
    if config.web.enabled {
        config.web.validate()?;
    }
    Ok(())
}

fn describe_web(config: &Config) -> String {
    if !config.web.enabled {
        return "disabled".into();
    }
    let scheme = if config.web.tls.enabled() {
        "https"
    } else {
        "http"
    };
    let mut text = format!("{}://{}:{}", scheme, config.web.host, config.web.port);
    if config.web.behind_proxy {
        text.push_str(" (behind a reverse proxy)");
    }
    if !config.web.allowed_hosts.is_empty() {
        text.push_str(&format!(", public hosts {:?}", config.web.allowed_hosts));
    }
    text
}

async fn open_database(config: &Config) -> Result<sqlx::SqlitePool, anyhow::Error> {
    let db_path = config.daemon.database_path.clone();
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    // The database holds password hashes and webhook secrets: create it
    // private before SQLite opens it (SQLite gives journals the same mode).
    #[cfg(unix)]
    if !db_path.exists() {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&db_path)?;
    }
    let conn_str = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
    tracing::info!(database = %conn_str, "Connecting to SQLite database");
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&conn_str)
        .await?;
    EventStore::initialize_db(&pool).await?;
    // Also tighten databases created by older versions.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(pool)
}

fn auth_store(config: &Config, pool: sqlx::SqlitePool) -> Result<AuthStore, anyhow::Error> {
    AuthStore::new(
        pool,
        AuthConfig {
            session_idle: Duration::from_secs(config.web.session_idle_minutes * 60),
            session_absolute: Duration::from_secs(config.web.session_max_hours * 3600),
        },
    )
}

/// Local administration, authorized by access to the database file.
async fn admin(config: &Config, args: &[&str]) -> Result<(), anyhow::Error> {
    let pool = open_database(config).await?;
    let auth = auth_store(config, pool.clone())?;
    let store = EventStore::new(pool);
    match args {
        ["reset-password", flags @ ..] => {
            let password = if flags.contains(&"--stdin") {
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line.trim_end_matches(['\r', '\n']).to_string()
            } else {
                let mut bytes = [0u8; 18];
                use rand_core::RngCore;
                rand_core::OsRng.fill_bytes(&mut bytes);
                bytes.iter().map(|b| format!("{:02x}", b)).collect()
            };
            auth.set_password(ADMIN_USER, &password).await?;
            store
                .append_event(
                    "PasswordReset",
                    serde_json::json!({"username": ADMIN_USER, "actor": "local-admin"}),
                )
                .await?;
            let _ = std::fs::remove_file(initial_password_path(config));
            if flags.contains(&"--stdin") {
                println!(
                    "Password for '{}' updated. All sessions were signed out.",
                    ADMIN_USER
                );
            } else {
                println!("New password for '{}': {}", ADMIN_USER, password);
                println!("All sessions were signed out.");
            }
            Ok(())
        }
        ["create-token", flags @ ..] => {
            let mut name = None;
            let mut scope = TokenScope::Deploy;
            let mut it = flags.iter();
            while let Some(flag) = it.next() {
                match *flag {
                    "--name" => name = it.next().map(|s| s.to_string()),
                    "--scope" => {
                        scope = it
                            .next()
                            .and_then(|s| TokenScope::parse(s))
                            .ok_or_else(|| anyhow::anyhow!("--scope must be read or deploy"))?
                    }
                    other => anyhow::bail!("Unknown flag {}\n\n{}", other, USAGE),
                }
            }
            let name = name.ok_or_else(|| anyhow::anyhow!("--name is required"))?;
            let created = auth.create_token(&name, scope).await?;
            store
                .append_event(
                    "ApiTokenCreated",
                    serde_json::json!({"token_id": created.info.id, "name": name,
                                       "scope": scope.as_str(), "actor": "local-admin"}),
                )
                .await?;
            // Only the token on stdout, so scripts can capture it.
            println!("{}", created.token);
            Ok(())
        }
        _ => anyhow::bail!("Unknown admin command\n\n{}", USAGE),
    }
}

async fn run_daemon(config: Config) -> Result<(), anyhow::Error> {
    // 2. Initialize logs
    aegis_logs::init_logging(&config.daemon.log_level);
    tracing::info!("Aegis Daemon v{} starting up...", DAEMON_VERSION);

    // 3. Connect to Database
    let pool = open_database(&config).await?;
    let auth = auth_store(&config, pool.clone())?;
    let event_store = EventStore::new(pool);

    // 4. Control plane: rebuilds state from history and runs deployments.
    let data_dir = config.daemon.resolved_data_dir();
    let mut settings = ControlSettings::new(data_dir.clone());
    settings.max_retained_versions = config.daemon.max_retained_versions;
    settings.resources = resource_mode(&config.resources);
    settings.sample_interval = Duration::from_secs(config.resources.sample_interval_secs);
    settings.metrics_retention = Duration::from_secs(config.resources.retention_days * 86400);
    let control = ControlPlane::new(event_store.clone(), ProcessSupervisor::new(), settings)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // 5. Plugins, including notifications.
    let mut plugin_manager = PluginManager::new();
    plugin_manager.register(Arc::new(DemoPlugin));
    if let Some(url) = &config.notifications.slack_webhook_url {
        let names = control.clone();
        plugin_manager.register(Arc::new(aegis_notifications::SlackPlugin::with_names(
            url.clone(),
            Arc::new(move |id: &str| {
                id.parse()
                    .ok()
                    .and_then(|id| names.projection().get_project(&id))
                    .map(|p| p.name)
            }),
        )));
    }
    plugin_manager.initialize_all().await?;
    plugin_manager.start_event_loop(event_store.subscribe());
    let plugin_manager = Arc::new(plugin_manager);
    control.start();
    let caps = control.resource_capabilities();
    tracing::info!(
        backend = caps.backend.as_str(),
        cpu = caps.cpu,
        memory = caps.memory,
        reason = caps.reason.as_deref().unwrap_or(""),
        "Resource limits"
    );
    tracing::info!(data_dir = %data_dir.display(), "Control plane ready");

    // 6. Scheduler: restore persisted schedules, then follow new ones.
    let scheduler = Arc::new(aegis_scheduler::SchedulerEngine::new());
    for event in event_store.get_events().await? {
        if event.event_type == "ScheduleConfigured" {
            apply_schedule(&scheduler, &event);
        }
    }
    let mut schedule_rx = event_store.subscribe();
    let scheduler_for_events = scheduler.clone();
    tokio::spawn(async move {
        loop {
            match schedule_rx.recv().await {
                Ok(event) if event.event_type == "ScheduleConfigured" => {
                    apply_schedule(&scheduler_for_events, &event)
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    // Scheduled deployments are published on their own bus; persist them so
    // the control plane picks them up like any other queued deployment.
    let sched_bus = aegis_event_bus::EventBus::new();
    let mut sched_rx = sched_bus.subscribe();
    let control_for_sched = control.clone();
    tokio::spawn(async move {
        while let Ok(event) = sched_rx.recv().await {
            let payload = serde_json::from_str(&event.payload_json).unwrap_or_default();
            if let Err(e) = control_for_sched.record(&event.event_type, payload).await {
                tracing::error!(error = %e, "Failed to record scheduled deployment");
            }
        }
    });
    scheduler.start_scheduler_loop(sched_bus);

    // 7. Bring back the apps that were running before the daemon stopped.
    let restored = control.recover().await;
    tracing::info!(restored, "Recovered app processes");

    // 8. HTTP API (optional; authenticated; public only with TLS)
    let (web_shutdown, web_shutdown_rx) = tokio::sync::watch::channel(false);
    let mut web_url = None;
    let web_task = if config.web.enabled {
        prepare_admin_account(&config, &auth).await?;
        let listener = tokio::net::TcpListener::bind((config.web.host.as_str(), config.web.port))
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Cannot listen on {}:{} for the HTTP API: {}",
                    config.web.host,
                    config.web.port,
                    e
                )
            })?;
        let local = listener.local_addr()?;
        let scheme = if config.web.tls.enabled() {
            "https"
        } else {
            "http"
        };
        let url = format!("{}://{}", scheme, local);
        tracing::info!(url = %url, "HTTP API listening");
        web_url = Some(url);

        // Expired sessions are rejected on use; this just keeps the table small.
        let purge_auth = auth.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3600));
            loop {
                tick.tick().await;
                if let Err(e) = purge_auth.purge_expired_sessions().await {
                    tracing::warn!(error = %e, "Failed to purge expired sessions");
                }
            }
        });

        let state = aegis_web::AppState::new(
            control.clone(),
            auth.clone(),
            aegis_web::WebSettings {
                allowed_hosts: config.web.allowed_hosts.clone(),
                https: config.web.https(),
                trust_proxy: config.web.behind_proxy,
                ..Default::default()
            },
            DAEMON_VERSION.to_string(),
            web_shutdown_rx.clone(),
        );
        let mut stop = web_shutdown_rx;
        let shutdown = async move {
            let _ = stop.wait_for(|stopping| *stopping).await;
        };
        Some(
            match (&config.web.tls.cert_path, &config.web.tls.key_path) {
                (Some(cert), Some(key)) => {
                    let tls = aegis_web::tls::server_config(
                        cert.clone(),
                        key.clone(),
                        Duration::from_secs(60),
                    )?;
                    tokio::spawn(aegis_web::tls::serve_tls(listener, state, tls, shutdown))
                }
                _ => tokio::spawn(aegis_web::serve(listener, state, shutdown)),
            },
        )
    } else {
        None
    };

    // 9. gRPC server
    // Loopback only (enforced by DaemonConfig::validate): it is unauthenticated.
    let addr: SocketAddr =
        tokio::net::lookup_host((config.daemon.host.as_str(), config.daemon.port))
            .await?
            .next()
            .ok_or_else(|| anyhow::anyhow!("Cannot resolve daemon.host {}", config.daemon.host))?;
    tracing::info!(grpc_bind = %addr, "Starting gRPC service server");
    let service = DaemonService {
        control: control.clone(),
        plugin_manager,
        web_url,
    };
    Server::builder()
        .add_service(AegisDaemonServer::new(service))
        .serve_with_shutdown(addr, async {
            shutdown_signal().await;
            tracing::info!("Received shutdown signal. Gracefully shutting down Aegis Daemon...");
        })
        .await?;

    // 10. Stop the HTTP API (ends open event/log streams), then the apps;
    // apps are restarted on the next boot.
    let _ = web_shutdown.send(true);
    if let Some(task) = web_task {
        match tokio::time::timeout(Duration::from_secs(10), task).await {
            Ok(Ok(Err(e))) => tracing::warn!(error = %e, "HTTP API stopped with an error"),
            Err(_) => tracing::warn!("HTTP API did not stop within 5s"),
            _ => {}
        }
    }
    control.shutdown().await;
    tracing::info!("Aegis Daemon stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_api::aegis::aegis_daemon_server::AegisDaemon;
    use aegis_api::aegis::{
        DeployRequest, EmitEventRequest, ListProjectsRequest, RegisterProjectRequest,
        StatusRequest, StreamEventsRequest,
    };
    use tokio_stream::StreamExt;
    use tonic::Request;

    async fn create_test_service(data_dir: &std::path::Path) -> DaemonService {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        EventStore::initialize_db(&pool).await.unwrap();
        let event_store = EventStore::new(pool);

        let mut plugin_manager = PluginManager::new();
        plugin_manager.register(Arc::new(DemoPlugin));

        let mut settings = ControlSettings::new(data_dir.to_path_buf());
        // Keep background samples (and their alerts) out of these tests.
        settings.sample_interval = Duration::from_secs(3600);
        let control = ControlPlane::new(event_store, ProcessSupervisor::new(), settings)
            .await
            .unwrap();
        control.start();
        DaemonService {
            control,
            plugin_manager: Arc::new(plugin_manager),
            web_url: None,
        }
    }

    #[tokio::test]
    async fn test_daemon_service_grpc_handlers() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = create_test_service(dir.path()).await;

        // 1. Test get_status
        let status_res = service
            .get_status(Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert!(status_res.initialized);
        assert_eq!(status_res.event_count, 0);
        assert_eq!(status_res.loaded_plugins, vec!["demo-system-plugin"]);
        assert_eq!(status_res.version, DAEMON_VERSION);

        // 2. Test emit_event invalid json
        let invalid_req = Request::new(EmitEventRequest {
            event_type: "TestEvent".to_string(),
            payload_json: "{ bad }".to_string(),
        });
        assert!(service.emit_event(invalid_req).await.is_err());

        // 3. Test emit_event valid
        let valid_req = Request::new(EmitEventRequest {
            event_type: "TestEvent".to_string(),
            payload_json: r#"{"foo":"bar"}"#.to_string(),
        });
        let emit_res = service.emit_event(valid_req).await.unwrap().into_inner();
        assert!(emit_res.success);
        assert!(!emit_res.event_id.is_empty());

        // 4. Verify event count updated
        let status_res2 = service
            .get_status(Request::new(StatusRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(status_res2.event_count, 1);

        // 5. Test stream_events
        let mut stream = service
            .stream_events(Request::new(StreamEventsRequest {}))
            .await
            .unwrap()
            .into_inner();
        let emit_req = Request::new(EmitEventRequest {
            event_type: "StreamedEvent".to_string(),
            payload_json: r#"{"num":42}"#.to_string(),
        });
        service.emit_event(emit_req).await.unwrap();
        let streamed_msg = stream.next().await.unwrap().unwrap();
        assert_eq!(streamed_msg.event_type, "StreamedEvent");
        assert!(streamed_msg.payload_json.contains("42"));
    }

    #[tokio::test]
    async fn test_register_and_deploy_validation() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = create_test_service(dir.path()).await;

        let project = service
            .register_project(Request::new(RegisterProjectRequest {
                name: "api".into(),
                source_dir: dir.path().to_string_lossy().to_string(),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(project.name, "api");

        let listed = service
            .list_projects(Request::new(ListProjectsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.projects.len(), 1);

        // Unknown projects and unsupported strategies map to proper status codes.
        let err = service
            .deploy(Request::new(DeployRequest {
                project: "nope".into(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
        let err = service
            .deploy(Request::new(DeployRequest {
                project: "api".into(),
                strategy: "BlueGreen".into(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }
}
