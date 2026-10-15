//! Zero-downtime deployments behind the built-in proxy, with real app
//! processes (`python3 -m http.server`). Each build writes its release
//! version into `version.txt`, so `GET /version.txt` shows which release
//! answered.
#![cfg(unix)]

use aegis_control::{ControlPlane, ControlSettings, DeployRequest, RegisterProject};
use aegis_event_store::EventStore;
use aegis_ingress::{Ingress, IngressConfig};
use aegis_process::ProcessSupervisor;
use aegis_projection::DeploymentState;
use aegis_types::{DeploymentId, ProjectId};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DOMAIN: &str = "web.example.com";

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

fn write_app(src: &Path, port: u16, start: &str) {
    std::fs::write(
        src.join("aegis.toml"),
        format!(
            r#"
[project]
name = "web"

[build]
install_command = ""
build_command = "echo $AEGIS_RELEASE_VERSION > version.txt"
start_command = "{start}"

[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/"
health_check_timeout_secs = 5
drain_timeout_secs = 5
"#
        ),
    )
    .unwrap();
}

const SERVE: &str = "exec python3 -m http.server $PORT --bind 127.0.0.1";

struct Proxy {
    ingress: Ingress,
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
    control.set_ingress(ingress.clone());
    Proxy {
        ingress,
        port,
        _stop: stop,
    }
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
    for _ in 0..600 {
        if let Some(d) = control.get_deployment(&id) {
            if !matches!(d.status.as_str(), "Queued" | "InProgress") {
                return d;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("deployment {id} did not finish");
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

#[tokio::test]
async fn deploys_behind_the_proxy_never_drop_a_request() {
    let data = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let configured = free_port();
    write_app(src.path(), configured, SERVE);
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
            name: "web".into(),
            source_dir: Some(src.path().to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;

    // Domain validation and uniqueness.
    assert!(control
        .map_domain(project, "*.example.com", "off", None, None)
        .await
        .is_err());
    assert!(control
        .map_domain(project, DOMAIN, "sometimes", None, None)
        .await
        .is_err());
    let mapped = control
        .map_domain(project, "Web.Example.com", "off", None, None)
        .await
        .unwrap();
    assert_eq!(mapped.domain, DOMAIN);
    assert_eq!(mapped.cert.unwrap().state, "off");
    let other = control
        .register_project(RegisterProject {
            name: "other".into(),
            source_dir: Some(src.path().to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    assert!(control
        .map_domain(other, DOMAIN, "off", None, None)
        .await
        .is_err());

    // Mapped but not deployed: the proxy says so.
    assert_eq!(via_proxy(proxy.port, "/").await.unwrap().0, 502);

    let d1 = deploy(&control, project, "r1").await;
    assert_eq!(d1.status, "Success", "{d1:?}");
    assert_eq!(
        via_proxy(proxy.port, "/version.txt").await.unwrap(),
        (200, "r1".to_string())
    );
    // Behind the proxy each release gets its own port, not the configured one.
    let first = control.resolve_process("web").unwrap();
    assert!(first.proxied);
    assert_ne!(first.port, Some(configured));
    assert_eq!(proxy.ingress.upstream(&project.to_string()), first.port);

    let stop = Arc::new(AtomicBool::new(false));
    let load = hammer(proxy.port, stop.clone());
    tokio::time::sleep(Duration::from_millis(300)).await;
    for v in ["r2", "r3"] {
        let d = deploy(&control, project, v).await;
        assert_eq!(d.status, "Success", "{d:?}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    // A release that never gets healthy: the live one keeps serving.
    write_app(src.path(), configured, "echo broken; exit 3");
    let d4 = deploy(&control, project, "r4").await;
    assert_eq!(d4.status, "RolledBack", "{d4:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.store(true, Ordering::SeqCst);
    let (seen, failures) = load.await.unwrap();
    assert!(
        failures.is_empty(),
        "requests failed during cutover: {failures:?}"
    );
    assert_eq!(seen, vec!["r1", "r2", "r3"]);

    let project_state = control.resolve_project("web").unwrap();
    assert_eq!(project_state.current_release.as_deref(), Some("r3"));
    let live = control.resolve_process("web").unwrap();
    assert_eq!(live.release_version.as_deref(), Some("r3"));
    assert_eq!(live.status, "Running");
    // Only the live release is still running.
    let running: Vec<_> = control
        .list_processes(Some(&project))
        .into_iter()
        .filter(|p| p.status == "Running")
        .collect();
    assert_eq!(running.len(), 1, "{running:?}");
    // Promotions record the port, which is what the proxy points at.
    let promoted = events_of(&control, project, "ReleasePromoted").await;
    assert_eq!(promoted.len(), 3);
    let last_port = promoted.last().unwrap()["port"].as_u64().unwrap() as u16;
    assert_eq!(Some(last_port), live.port);
    assert_eq!(
        proxy.ingress.upstream(&project.to_string()),
        Some(last_port)
    );
    let ports: std::collections::HashSet<u64> =
        promoted.iter().filter_map(|p| p["port"].as_u64()).collect();
    assert_eq!(ports.len(), 3, "each release had its own port");

    // Rollback goes through the same side-by-side cutover.
    write_app(src.path(), configured, SERVE);
    assert_eq!(control.rollback(project, None).await.unwrap(), "r2");
    assert_eq!(
        via_proxy(proxy.port, "/version.txt").await.unwrap(),
        (200, "r2".to_string())
    );

    // Daemon restart: routes and ports come back from the recorded events.
    let port_before = control.resolve_process("web").unwrap().port;
    control.shutdown().await;
    drop(proxy);
    let control = new_control(&store, data.path()).await;
    let proxy = start_proxy(&control, &data.path().join("certs")).await;
    assert_eq!(control.recover().await, 1);
    assert_eq!(
        control.list_domains(&project).unwrap()[0].domain,
        DOMAIN.to_string()
    );
    let mut answer = None;
    for _ in 0..50 {
        answer = via_proxy(proxy.port, "/version.txt").await.ok();
        if answer.as_ref().is_some_and(|(s, _)| *s == 200) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(answer, Some((200, "r2".to_string())));
    assert_eq!(control.resolve_process("web").unwrap().port, port_before);

    // Without a domain, deployments go back to the configured port.
    control.unmap_domain(project, DOMAIN).await.unwrap();
    assert!(control.unmap_domain(project, DOMAIN).await.is_err());
    assert_eq!(via_proxy(proxy.port, "/").await.unwrap().0, 404);
    let d5 = deploy(&control, project, "r5").await;
    assert_eq!(d5.status, "Success", "{d5:?}");
    let live = control.resolve_process("web").unwrap();
    assert!(!live.proxied);
    assert_eq!(live.port, Some(configured));
    control.shutdown().await;
}
