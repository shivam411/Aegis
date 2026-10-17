//! Container apps (`[run] runtime = "docker"`) against a real Docker
//! daemon: build, health check, proxy cutover, secrets, live limits, crash
//! restarts, rollback, image pruning, daemon restarts and stop.
//!
//! The app is `examples/docker-app` (BusyBox httpd). Each build writes its
//! release version into `www/version.txt`, so `GET /version.txt` shows
//! which release answered.
//!
//! Without a usable Docker daemon the test prints why and passes, unless
//! `AEGIS_REQUIRE_DOCKER=1` (set in CI), where it fails instead.
#![cfg(unix)]

use aegis_config::ResourceLimits;
use aegis_control::{ControlPlane, ControlSettings, DeployRequest, ProcessAction, RegisterProject};
use aegis_docker::args::{image_repository, image_tag, LABEL_PROJECT};
use aegis_event_store::EventStore;
use aegis_ingress::{Ingress, IngressConfig};
use aegis_process::ProcessSupervisor;
use aegis_projection::DeploymentState;
use aegis_types::{DeploymentId, ProjectId};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DOMAIN: &str = "boxed.example.com";
const SECRET: &str = "hello-from-a-secret";
const SERVE: &str = r#"["sh", "-c", "printf '%s' \"$GREETING\" > /www/greeting.txt; echo \"serving on $PORT\"; exec httpd -f -v -p \"$PORT\" -h /www"]"#;
const BROKEN: &str = r#"["sh", "-c", "echo broken; exit 3"]"#;

async fn docker_ready() -> bool {
    let status = aegis_docker::probe(&aegis_docker::docker_bin()).await;
    if status.available {
        return true;
    }
    let reason = status.reason.unwrap_or_default();
    if std::env::var("AEGIS_REQUIRE_DOCKER").is_ok_and(|v| v == "1") {
        panic!("AEGIS_REQUIRE_DOCKER=1 but Docker is not usable: {reason}");
    }
    eprintln!(
        "SKIPPED: Docker is not usable here ({reason}); set AEGIS_REQUIRE_DOCKER=1 to fail instead"
    );
    false
}

/// Runs `docker <args>`: (success, trimmed stdout).
fn docker(args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new(aegis_docker::docker_bin())
        .args(args)
        .output()
        .expect("docker runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

fn inspect(container: &str, format: &str) -> String {
    let (ok, out) = docker(&["inspect", "--format", format, container]);
    assert!(ok, "docker inspect {container}");
    out
}

fn container_exists(name: &str) -> bool {
    docker(&["inspect", "--type", "container", name]).0
}

fn image_exists(image: &str) -> bool {
    docker(&["image", "inspect", image]).0
}

/// Removes the test's containers and images even if it fails half-way.
struct Cleanup(ProjectId);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let label = format!("label={}={}", LABEL_PROJECT, self.0);
        let (_, ids) = docker(&["ps", "--all", "--quiet", "--filter", &label]);
        for id in ids.lines().filter(|l| !l.is_empty()) {
            docker(&["rm", "--force", id]);
        }
        let (_, images) = docker(&[
            "image",
            "ls",
            "--format",
            "{{.Repository}}:{{.Tag}}",
            &image_repository(&self.0),
        ]);
        for image in images.lines().filter(|l| !l.is_empty()) {
            docker(&["image", "rm", "--force", image]);
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn settings(data_dir: &Path) -> ControlSettings {
    let mut settings = ControlSettings::new(data_dir.to_path_buf());
    settings.health_poll_interval = Duration::from_millis(100);
    settings.liveness_window = Duration::from_millis(500);
    settings.sample_interval = Duration::from_secs(3600);
    settings
}

fn example_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/docker-app")
}

fn write_app(src: &Path, port: u16, command: &str) {
    std::fs::create_dir_all(src.join("www")).unwrap();
    std::fs::copy(example_dir().join("Dockerfile"), src.join("Dockerfile")).unwrap();
    std::fs::copy(
        example_dir().join("www/index.html"),
        src.join("www/index.html"),
    )
    .unwrap();
    std::fs::write(
        src.join("aegis.toml"),
        format!(
            r#"
[project]
name = "boxed"

[build]
build_command = "echo $AEGIS_RELEASE_VERSION > www/version.txt"

[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/version.txt"
health_check_timeout_secs = 60
drain_timeout_secs = 5
max_retained_versions = 2

[run]
runtime = "docker"
container_port = 8080
command = {command}
"#
        ),
    )
    .unwrap();
}

struct Proxy {
    port: u16,
    _stop: tokio::sync::watch::Sender<bool>,
}

async fn start_proxy(control: &ControlPlane, certs: &Path) -> Proxy {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = IngressConfig::new(certs.to_path_buf());
    config.http_port = port;
    config.https_port = free_port();
    let ingress = Ingress::new(config);
    let (stop, mut rx) = tokio::sync::watch::channel(false);
    tokio::spawn(ingress.clone().serve_http(listener, async move {
        let _ = rx.wait_for(|v| *v).await;
    }));
    control.set_ingress(ingress);
    Proxy { port, _stop: stop }
}

async fn new_control(store: &EventStore, data: &Path) -> ControlPlane {
    let control = ControlPlane::new(store.clone(), ProcessSupervisor::new(), settings(data))
        .await
        .unwrap();
    control.start();
    control
}

async fn deploy(control: &ControlPlane, project: ProjectId, version: &str) -> DeploymentState {
    let id = control
        .queue_deployment(DeployRequest {
            project_id: project,
            version: Some(version.to_string()),
            trigger: "test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    wait_deployment(control, id).await
}

async fn wait_deployment(control: &ControlPlane, id: DeploymentId) -> DeploymentState {
    // Image builds (and the first base image pull) can take a while.
    for _ in 0..3000 {
        if let Some(d) = control.get_deployment(&id) {
            if !matches!(d.status.as_str(), "Queued" | "InProgress") {
                return d;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("deployment {id} did not finish");
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..600 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

/// GET through the proxy: (status, body).
async fn via_proxy(port: u16, path: &str) -> Result<(u16, String), String> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {DOMAIN}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = String::new();
    stream
        .read_to_string(&mut buf)
        .await
        .map_err(|e| e.to_string())?;
    let (head, body) = buf.split_once("\r\n\r\n").ok_or("no response")?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("no status")?;
    Ok((status, body.trim().to_string()))
}

async fn version_via_proxy(port: u16) -> Option<String> {
    match via_proxy(port, "/version.txt").await {
        Ok((200, body)) => Some(body),
        _ => None,
    }
}

/// Requests the page back to back until stopped; returns (versions seen in
/// order, failures).
fn hammer(port: u16, stop: Arc<AtomicBool>) -> tokio::task::JoinHandle<(Vec<String>, Vec<String>)> {
    tokio::spawn(async move {
        let mut seen: Vec<String> = Vec::new();
        let mut failures = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            match via_proxy(port, "/version.txt").await {
                Ok((200, body)) => {
                    if seen.last() != Some(&body) {
                        seen.push(body);
                    }
                }
                Ok((status, body)) => failures.push(format!("{status}: {body}")),
                Err(e) => failures.push(e),
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        (seen, failures)
    })
}

async fn events_of(control: &ControlPlane, project: ProjectId, kind: &str) -> Vec<Value> {
    control
        .list_events(Some(&project), usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == kind)
        .map(|e| serde_json::from_str(&e.payload_json).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn container_app_lifecycle() {
    if !docker_ready().await {
        return;
    }
    let data = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_app(src.path(), port, SERVE);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    let store = EventStore::new(pool);
    let control = new_control(&store, data.path()).await;
    let proxy = start_proxy(&control, &data.path().join("certs")).await;
    let project = control
        .register_project(RegisterProject {
            name: "boxed".into(),
            source_dir: Some(src.path().to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let _cleanup = Cleanup(project);
    control
        .set_secret(project, "GREETING", SECRET)
        .await
        .unwrap();
    control
        .map_domain(project, DOMAIN, "off", None, None)
        .await
        .unwrap();

    // 1. First deployment: image built, container healthy, served by the proxy.
    let d1 = deploy(&control, project, "r1").await;
    assert_eq!(d1.status, "Success", "{d1:?}");
    assert_eq!(version_via_proxy(proxy.port).await.as_deref(), Some("r1"));
    // The secret reached the container through its environment.
    assert_eq!(
        via_proxy(proxy.port, "/greeting.txt").await.unwrap(),
        (200, SECRET.to_string())
    );
    let p1 = control.resolve_process("boxed").unwrap();
    assert_eq!(p1.runtime.as_deref(), Some("docker"));
    assert_eq!(p1.image, Some(image_tag(&project, "r1")));
    let container1 = p1.container.clone().unwrap();
    // The pid shown is the container's main process.
    assert_eq!(
        inspect(&container1, "{{.State.Pid}}"),
        p1.pid.unwrap().to_string()
    );
    // Published on loopback only, on the port allocated for the proxy.
    let bindings = inspect(&container1, "{{json .HostConfig.PortBindings}}");
    assert!(bindings.contains("\"HostIp\":\"127.0.0.1\""), "{bindings}");
    assert!(
        bindings.contains(&format!("\"HostPort\":\"{}\"", p1.port.unwrap())),
        "{bindings}"
    );
    // Secrets never sit on a command line, and the env file is gone.
    let ps = std::process::Command::new("ps")
        .args(["-eo", "args"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&ps.stdout).contains(SECRET));
    let leftovers: Vec<_> = std::fs::read_dir(data.path().join("run/docker"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "env files left: {leftovers:?}");
    let started = events_of(&control, project, "ProcessStarted").await;
    assert_eq!(started.last().unwrap()["runtime"], "docker");
    assert!(!started.iter().any(|e| e.to_string().contains(SECRET)));
    // The container's output is in the process log.
    wait_for("container output in the log", || {
        control
            .process_logs(&p1, 100)
            .iter()
            .any(|l| l.line.contains("serving on 8080"))
    })
    .await;

    // 2. Live limits: `docker update`, visible in `docker inspect`.
    let project_state = control.resolve_project("boxed").unwrap();
    let caps = control.project_capabilities(&project_state);
    assert_eq!(caps.backend.as_str(), "docker");
    assert!(caps.cpu && caps.memory && caps.pids, "{caps:?}");
    let limits = ResourceLimits {
        cpu_cores: Some(0.5),
        memory_bytes: Some(128 * 1024 * 1024),
        pids_max: Some(64),
    };
    control
        .set_resource_limits(project, limits, true)
        .await
        .unwrap();
    assert_eq!(
        inspect(&container1, "{{.HostConfig.NanoCpus}}"),
        "500000000"
    );
    assert_eq!(inspect(&container1, "{{.HostConfig.Memory}}"), "134217728");
    assert_eq!(inspect(&container1, "{{.HostConfig.PidsLimit}}"), "64");

    // 3. Redeploy under load: the proxy switches without dropping a request,
    // and the old container is removed.
    let stop = Arc::new(AtomicBool::new(false));
    let load = hammer(proxy.port, stop.clone());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let d2 = deploy(&control, project, "r2").await;
    assert_eq!(d2.status, "Success", "{d2:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.store(true, Ordering::SeqCst);
    let (seen, failures) = load.await.unwrap();
    assert!(
        failures.is_empty(),
        "requests failed during cutover: {failures:?}"
    );
    assert_eq!(seen, vec!["r1", "r2"]);
    wait_for("the old container to be removed", || {
        !container_exists(&container1)
    })
    .await;
    let p2 = control.resolve_process("boxed").unwrap();
    let container2 = p2.container.clone().unwrap();
    // A new container starts with the project's limits.
    assert_eq!(
        inspect(&container2, "{{.HostConfig.NanoCpus}}"),
        "500000000"
    );
    assert_eq!(inspect(&container2, "{{.HostConfig.PidsLimit}}"), "64");

    // 4. A crash: the container is killed, Aegis records it and restarts it.
    assert!(docker(&["kill", &container2]).0);
    let id2 = p2.id.to_string();
    let mut crashed = Vec::new();
    for _ in 0..300 {
        crashed = events_of(&control, project, "ProcessCrashed").await;
        let running = control.resolve_process("boxed").unwrap();
        if !crashed.is_empty() && running.status == "Running" && running.restart_count >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let crash = crashed.last().expect("a ProcessCrashed event");
    assert_eq!(crash["process_id"], id2);
    assert_eq!(crash["will_restart"], true);
    let restarted = control.resolve_process("boxed").unwrap();
    assert_eq!(restarted.id, p2.id);
    assert_eq!(restarted.status, "Running");
    assert!(restarted.restart_count >= 1);
    let mut answer = None;
    for _ in 0..100 {
        answer = version_via_proxy(proxy.port).await;
        if answer.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(answer.as_deref(), Some("r2"));

    // 5. A release that never gets healthy is rolled back; r2 keeps serving.
    write_app(src.path(), port, BROKEN);
    let d3 = deploy(&control, project, "r3").await;
    assert_eq!(d3.status, "RolledBack", "{d3:?}");
    assert_eq!(version_via_proxy(proxy.port).await.as_deref(), Some("r2"));
    write_app(src.path(), port, SERVE);

    // 6. Rollback reuses r1's image: nothing is built.
    assert!(image_exists(&image_tag(&project, "r1")));
    let builds_before = events_of(&control, project, "BuildStageBuild").await.len();
    assert_eq!(
        control.rollback(project, Some("r1".into())).await.unwrap(),
        "r1"
    );
    assert_eq!(
        events_of(&control, project, "BuildStageBuild").await.len(),
        builds_before
    );
    assert_eq!(version_via_proxy(proxy.port).await.as_deref(), Some("r1"));

    // 7. Retention (max 2): deploying r4 keeps r4 and r1; the pruned
    // releases' images are removed.
    let d4 = deploy(&control, project, "r4").await;
    assert_eq!(d4.status, "Success", "{d4:?}");
    assert_eq!(version_via_proxy(proxy.port).await.as_deref(), Some("r4"));
    let r2_image = image_tag(&project, "r2");
    wait_for("the pruned release's image to be removed", || {
        !image_exists(&r2_image)
    })
    .await;
    assert!(image_exists(&image_tag(&project, "r1")));
    assert!(image_exists(&image_tag(&project, "r4")));

    // 8. Daemon restart: containers are stopped and removed on shutdown,
    // and started again on boot.
    let p4 = control.resolve_process("boxed").unwrap();
    let container4 = p4.container.clone().unwrap();
    control.shutdown().await;
    drop(proxy);
    assert!(!container_exists(&container4));
    let control = new_control(&store, data.path()).await;
    let proxy = start_proxy(&control, &data.path().join("certs")).await;
    assert_eq!(control.recover().await, 1);
    let mut answer = None;
    for _ in 0..100 {
        answer = version_via_proxy(proxy.port).await;
        if answer.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(answer.as_deref(), Some("r4"));
    let after_boot = control.resolve_process("boxed").unwrap();
    assert_eq!(after_boot.id, p4.id);
    assert!(container_exists(&container4));

    // 9. Stop: the container goes away and the app stays down.
    let stopped = control
        .process_action("boxed", ProcessAction::Stop)
        .await
        .unwrap();
    assert_eq!(stopped.status, "Stopped");
    assert!(!container_exists(&container4));
    control.shutdown().await;
}
