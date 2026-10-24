//! Several instances per app, and the Rolling and BlueGreen strategies,
//! with real app processes (`python -m http.server`) behind the built-in
//! proxy. Each build writes its release version into `version.txt`, so
//! `GET /version.txt` shows which release answered. A client loop hammers
//! the proxy during every rollout and must never see a failed request.

use aegis_control::{ControlPlane, ControlSettings, DeployRequest, ProcessAction, RegisterProject};
use aegis_event_store::EventStore;
use aegis_ingress::{Ingress, IngressConfig};
use aegis_process::ProcessSupervisor;
use aegis_projection::DeploymentState;
use aegis_types::{DeploymentId, ProjectId};
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DOMAIN: &str = "web.example.com";

#[cfg(unix)]
const SERVE: &str = "exec python3 -m http.server $PORT --bind 127.0.0.1";
#[cfg(windows)]
const SERVE: &str = "python -m http.server %PORT% --bind 127.0.0.1";
/// Only the first instance of a release comes up; the others exit.
#[cfg(unix)]
const FIRST_ONLY: &str = "if mkdir started.lock 2>/dev/null; then exec python3 -m http.server $PORT --bind 127.0.0.1; else exit 3; fi";
#[cfg(windows)]
const FIRST_ONLY: &str =
    "mkdir started.lock 2>nul && python -m http.server %PORT% --bind 127.0.0.1";
#[cfg(unix)]
const BUILD: &str = "echo $AEGIS_RELEASE_VERSION > version.txt";
#[cfg(windows)]
const BUILD: &str = "echo %AEGIS_RELEASE_VERSION%> version.txt";

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

fn write_app(src: &Path, port: u16, start: &str, extra: &str) {
    std::fs::write(
        src.join("aegis.toml"),
        format!(
            r#"
[project]
name = "web"

[build]
install_command = ""
build_command = "{BUILD}"
start_command = "{start}"

[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/"
health_check_timeout_secs = 10
drain_timeout_secs = 5
{extra}
"#
        ),
    )
    .unwrap();
}

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

async fn new_store() -> EventStore {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    EventStore::new(pool)
}

async fn new_control(store: &EventStore, data: &Path) -> ControlPlane {
    let control = ControlPlane::new(store.clone(), ProcessSupervisor::new(), settings(data))
        .await
        .unwrap();
    control.start();
    control
}

struct App {
    control: ControlPlane,
    store: EventStore,
    proxy: Proxy,
    project: ProjectId,
    configured: u16,
    data: TempDir,
    src: TempDir,
}

/// A registered app with a domain on the proxy, not deployed yet.
async fn app(extra: &str) -> App {
    let data = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let configured = free_port();
    write_app(src.path(), configured, SERVE, extra);
    let store = new_store().await;
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
    control
        .map_domain(project, DOMAIN, "off", None, None)
        .await
        .unwrap();
    App {
        control,
        store,
        proxy,
        project,
        configured,
        data,
        src,
    }
}

async fn deploy(app: &App, version: &str, strategy: Option<&str>) -> DeploymentState {
    let id = app
        .control
        .queue_deployment(DeployRequest {
            project_id: app.project,
            version: Some(version.to_string()),
            strategy: strategy.map(str::to_string),
            trigger: "test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    wait_deployment(&app.control, id).await
}

async fn wait_deployment(control: &ControlPlane, id: DeploymentId) -> DeploymentState {
    for _ in 0..1200 {
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

/// The version serving `/version.txt`, retrying while the app comes up.
async fn serving(port: u16) -> Option<String> {
    for _ in 0..100 {
        if let Ok((200, body)) = via_proxy(port, "/version.txt").await {
            return Some(body);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

struct Load {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<(HashSet<String>, usize, Vec<String>)>,
}

/// Requests the page back to back until stopped.
fn hammer(port: u16) -> Load {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let task = tokio::spawn(async move {
        let mut seen = HashSet::new();
        let mut ok = 0;
        let mut failures = Vec::new();
        while !flag.load(Ordering::SeqCst) {
            match via_proxy(port, "/version.txt").await {
                Ok((200, body)) => {
                    seen.insert(body);
                    ok += 1;
                }
                Ok((status, body)) => failures.push(format!("{status}: {body}")),
                Err(e) => failures.push(e),
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        (seen, ok, failures)
    });
    Load { stop, task }
}

impl Load {
    /// (versions seen, successful requests); panics on any failed request.
    async fn finish(self, during: &str) -> (HashSet<String>, usize) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.stop.store(true, Ordering::SeqCst);
        let (seen, ok, failures) = self.task.await.unwrap();
        assert!(
            failures.is_empty(),
            "requests failed during {during}: {failures:?}"
        );
        assert!(ok > 0, "no requests went through during {during}");
        (seen, ok)
    }
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

fn view(control: &ControlPlane, project: ProjectId) -> aegis_control::ProjectView {
    control
        .list_projects()
        .into_iter()
        .find(|v| v.project.id == project)
        .unwrap()
}

/// Running instances: (process id, release, port).
fn running(
    control: &ControlPlane,
    project: ProjectId,
) -> Vec<(aegis_types::ProcessId, String, u16)> {
    view(control, project)
        .instances
        .into_iter()
        .filter(|i| i.process.status == "Running")
        .map(|i| {
            (
                i.process.id,
                i.process.release_version.unwrap_or_default(),
                i.process.port.unwrap_or_default(),
            )
        })
        .collect()
}

fn sorted(mut ports: Vec<u16>) -> Vec<u16> {
    ports.sort_unstable();
    ports
}

#[tokio::test]
async fn rolling_updates_three_instances_without_dropping_requests() {
    let app = app("[run]\ninstances = 3\n").await;
    let (control, project, proxy) = (&app.control, app.project, &app.proxy);

    // First deployment: three instances behind the proxy.
    let d1 = deploy(&app, "r1", None).await;
    assert_eq!(d1.status, "Success", "{d1:?}");
    let first = running(control, project);
    assert_eq!(first.len(), 3, "{first:?}");
    assert!(first.iter().all(|(_, v, _)| v == "r1"));
    let ports: Vec<u16> = first.iter().map(|(_, _, p)| *p).collect();
    assert_eq!(
        sorted(proxy.ingress.upstreams(&project.to_string())),
        sorted(ports.clone())
    );
    assert!(
        !ports.contains(&app.configured),
        "each instance has its own port"
    );
    let numbers: Vec<_> = view(control, project)
        .instances
        .iter()
        .map(|i| i.process.instance)
        .collect();
    assert_eq!(numbers, vec![Some(1), Some(2), Some(3)]);
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r1"));

    // Rolling update under load.
    let load = hammer(proxy.port);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let d2 = deploy(&app, "r2", Some("Rolling")).await;
    assert_eq!(d2.status, "Success", "{d2:?}");
    assert_eq!(d2.strategy, "Rolling");
    let (seen, _) = load.finish("the rolling update").await;
    assert!(seen.contains("r2"), "{seen:?}");
    let second = running(control, project);
    assert_eq!(second.len(), 3, "{second:?}");
    assert!(second.iter().all(|(_, v, _)| v == "r2"), "{second:?}");
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r2"));
    // One recorded step per instance, for the deployment's timeline.
    let steps: Vec<Value> = control
        .deployment_events(&d2)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == "RolloutStepCompleted")
        .map(|e| serde_json::from_str(&e.payload_json).unwrap())
        .collect();
    assert_eq!(steps.len(), 3, "{steps:?}");
    for (i, step) in steps.iter().enumerate() {
        assert_eq!(step["step"], i + 1);
        assert_eq!(step["steps"], 3);
        assert_eq!(step["started"].as_array().unwrap().len(), 1);
        assert_eq!(step["stopped"].as_array().unwrap().len(), 1);
    }
    // The old instances are gone.
    for (id, _, _) in &first {
        assert_ne!(
            control.resolve_process(&id.to_string()).unwrap().status,
            "Running"
        );
    }

    // A release whose second instance never comes up: the rollout stops
    // at step 2 and the old set is restored, with no failed request.
    write_app(
        app.src.path(),
        app.configured,
        FIRST_ONLY,
        "[run]\ninstances = 3\n",
    );
    let load = hammer(proxy.port);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let d3 = deploy(&app, "r3", Some("Rolling")).await;
    assert_eq!(d3.status, "RolledBack", "{d3:?}");
    assert!(
        d3.error.as_deref().unwrap_or("").contains("step 2 of 3"),
        "{d3:?}"
    );
    load.finish("the failed rolling update").await;
    let restored = running(control, project);
    assert_eq!(restored.len(), 3, "{restored:?}");
    assert!(restored.iter().all(|(_, v, _)| v == "r2"), "{restored:?}");
    assert_eq!(
        restored.iter().map(|r| r.0).collect::<HashSet<_>>(),
        second.iter().map(|r| r.0).collect::<HashSet<_>>(),
        "the same instances serve again"
    );
    assert_eq!(
        sorted(proxy.ingress.upstreams(&project.to_string())),
        sorted(restored.iter().map(|r| r.2).collect())
    );
    let aborted = events_of(control, project, "RolloutAborted").await;
    assert_eq!(aborted.len(), 1);
    assert_eq!(aborted[0]["step"], 2);
    assert_eq!(
        aborted[0]["restored_instances"].as_array().unwrap().len(),
        3
    );
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r2"));
    // Only the restored set (and nothing of r3) runs.
    let all_running = control
        .list_processes(Some(&project))
        .into_iter()
        .filter(|p| p.status == "Running")
        .count();
    assert_eq!(all_running, 3);
    write_app(
        app.src.path(),
        app.configured,
        SERVE,
        "[run]\ninstances = 3\n",
    );

    // Restarting the app restarts one instance at a time.
    let pids: Vec<_> = view(control, project)
        .instances
        .iter()
        .map(|i| i.process.pid)
        .collect();
    let load = hammer(proxy.port);
    tokio::time::sleep(Duration::from_millis(300)).await;
    control
        .process_action("web", ProcessAction::Restart)
        .await
        .unwrap();
    load.finish("a rolling restart").await;
    let after: Vec<_> = view(control, project)
        .instances
        .iter()
        .map(|i| i.process.pid)
        .collect();
    assert_eq!(after.len(), 3);
    for (before, after) in pids.iter().zip(&after) {
        assert_ne!(before, after, "every instance was restarted");
    }
    assert_eq!(running(control, project).len(), 3);

    // Stop and start act on every instance.
    control
        .process_action("web", ProcessAction::Stop)
        .await
        .unwrap();
    assert!(running(control, project).is_empty());
    assert_eq!(via_proxy(proxy.port, "/").await.unwrap().0, 502);
    control
        .process_action("web", ProcessAction::Start)
        .await
        .unwrap();
    assert_eq!(running(control, project).len(), 3);
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r2"));

    // A daemon restart brings back all three instances, on their ports.
    let before = sorted(running(control, project).iter().map(|r| r.2).collect());
    control.shutdown().await;
    let App {
        store, data, proxy, ..
    } = app;
    drop(proxy);
    let control = new_control(&store, data.path()).await;
    let proxy = start_proxy(&control, &data.path().join("certs")).await;
    assert_eq!(control.recover().await, 3);
    let back = running(&control, project);
    assert_eq!(back.len(), 3, "{back:?}");
    assert_eq!(sorted(back.iter().map(|r| r.2).collect()), before);
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r2"));
    control.shutdown().await;
}

#[tokio::test]
async fn a_rollout_interrupted_by_a_daemon_stop_comes_back_as_the_old_set() {
    let app = app("[run]\ninstances = 3\n").await;
    let (control, project) = (&app.control, app.project);
    assert_eq!(deploy(&app, "r1", None).await.status, "Success");
    let old: HashSet<_> = running(control, project).iter().map(|r| r.0).collect();
    assert_eq!(old.len(), 3);

    // Stop the daemon after the first rolling step.
    let id = control
        .queue_deployment(DeployRequest {
            project_id: project,
            version: Some("r2".into()),
            strategy: Some("Rolling".into()),
            trigger: "test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut stepped = false;
    for _ in 0..600 {
        if !events_of(control, project, "RolloutStepCompleted")
            .await
            .is_empty()
        {
            stepped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(stepped, "the rollout never completed a step");
    control.shutdown().await;
    let interrupted = wait_deployment(control, id).await;
    assert_ne!(interrupted.status, "Success", "{interrupted:?}");

    // On boot, the recorded live set (all of r1, including the instance
    // the rollout had already replaced) comes back.
    let App {
        store, data, proxy, ..
    } = app;
    drop(proxy);
    let control = new_control(&store, data.path()).await;
    let proxy = start_proxy(&control, &data.path().join("certs")).await;
    assert_eq!(control.recover().await, 3);
    let back = running(&control, project);
    assert_eq!(back.len(), 3, "{back:?}");
    assert!(back.iter().all(|(_, v, _)| v == "r1"), "{back:?}");
    assert_eq!(back.iter().map(|r| r.0).collect::<HashSet<_>>(), old);
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r1"));
    // Depending on when the stop came, the rollout either saw it between
    // steps and put the old set back (RolledBack) or lost its new instance
    // (Failed); it never went live.
    let after = control.get_deployment(&id).unwrap();
    assert!(
        matches!(after.status.as_str(), "Failed" | "RolledBack"),
        "{after:?}"
    );
    assert_eq!(
        control
            .resolve_project("web")
            .unwrap()
            .current_release
            .as_deref(),
        Some("r1")
    );
    control.shutdown().await;
}

#[tokio::test]
async fn blue_green_keeps_the_old_set_for_an_instant_rollback() {
    // `extra` goes at the end of [deploy].
    let app = app("blue_green_keep_secs = 120\n[run]\ninstances = 2\n").await;
    let (control, project, proxy) = (&app.control, app.project, &app.proxy);

    assert_eq!(
        deploy(&app, "r1", Some("BlueGreen")).await.status,
        "Success"
    );
    let blue = running(control, project);
    assert_eq!(blue.len(), 2);

    let load = hammer(proxy.port);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let d2 = deploy(&app, "r2", Some("BlueGreen")).await;
    assert_eq!(d2.status, "Success", "{d2:?}");
    let (seen, _) = load.finish("the BlueGreen switch").await;
    assert!(seen.contains("r2"));
    let green = running(control, project);
    assert_eq!(green.len(), 2);
    assert!(green.iter().all(|(_, v, _)| v == "r2"));
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r2"));
    // Only the new set gets traffic; the old one stays up as a standby.
    assert_eq!(
        sorted(proxy.ingress.upstreams(&project.to_string())),
        sorted(green.iter().map(|g| g.2).collect())
    );
    let standby = view(control, project).standby.expect("a standby");
    assert_eq!(standby.version, "r1");
    assert_eq!(
        standby.process_ids.iter().copied().collect::<HashSet<_>>(),
        blue.iter().map(|b| b.0).collect::<HashSet<_>>()
    );
    for (id, _, _) in &blue {
        assert_eq!(
            control.resolve_process(&id.to_string()).unwrap().status,
            "Running"
        );
    }
    let switched = events_of(control, project, "TrafficSwitched").await;
    assert_eq!(switched.last().unwrap()["to_version"], "r2");
    assert_eq!(events_of(control, project, "StandbyKept").await.len(), 1);

    // Rolling back inside the window is a switch back to the same processes.
    let load = hammer(proxy.port);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = std::time::Instant::now();
    assert_eq!(control.rollback(project, None).await.unwrap(), "r1");
    let switch_time = started.elapsed();
    load.finish("the instant rollback").await;
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r1"));
    let live = running(control, project);
    assert_eq!(
        live.iter().map(|l| l.0).collect::<HashSet<_>>(),
        blue.iter().map(|b| b.0).collect::<HashSet<_>>(),
        "no new process was started"
    );
    let completed = events_of(control, project, "RollbackCompleted").await;
    assert_eq!(completed.last().unwrap()["instant"], true);
    assert!(
        switch_time < Duration::from_secs(8),
        "instant rollback took {switch_time:?}"
    );
    assert!(view(control, project).standby.is_none());
    for (id, _, _) in &green {
        assert_ne!(
            control.resolve_process(&id.to_string()).unwrap().status,
            "Running"
        );
    }

    // With a short window the standby is stopped on its own.
    write_app(
        app.src.path(),
        app.configured,
        SERVE,
        "blue_green_keep_secs = 1\n[run]\ninstances = 2\n",
    );
    assert_eq!(
        deploy(&app, "r3", Some("BlueGreen")).await.status,
        "Success"
    );
    let previous: Vec<_> = live.iter().map(|l| l.0).collect();
    let mut stopped = Vec::new();
    for _ in 0..100 {
        stopped = events_of(control, project, "StandbyStopped").await;
        if stopped.iter().any(|s| s["reason"] == "expired") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        stopped.iter().any(|s| s["reason"] == "expired"),
        "{stopped:?}"
    );
    for id in previous {
        assert_ne!(
            control.resolve_process(&id.to_string()).unwrap().status,
            "Running"
        );
    }
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r3"));

    // Without a domain, Rolling, BlueGreen and several instances are refused.
    control.unmap_domain(project, DOMAIN).await.unwrap();
    for strategy in ["Rolling", "BlueGreen"] {
        let err = control
            .queue_deployment(DeployRequest {
                project_id: project,
                strategy: Some(strategy.into()),
                ..Default::default()
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("built-in proxy"), "{err}");
    }
    let err = control
        .update_project_settings(
            project,
            aegis_config::ProjectOverrides {
                instances: Some(3),
                ..Default::default()
            },
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("built-in proxy"), "{err}");
    // A deployment of a release asking for 2 instances fails before
    // touching the running app.
    let d4 = deploy(&app, "r4", None).await;
    assert_eq!(d4.status, "Failed", "{d4:?}");
    assert!(
        d4.error.as_deref().unwrap_or("").contains("instances"),
        "{d4:?}"
    );
    assert_eq!(running(control, project).len(), 2);
    control.shutdown().await;
}

#[tokio::test]
async fn deleting_a_running_multi_instance_app_frees_its_domain_ports_and_name() {
    let app = app("[run]\ninstances = 2\n").await;
    let (control, project, proxy) = (&app.control, app.project, &app.proxy);
    assert_eq!(deploy(&app, "r1", None).await.status, "Success");
    let instances = running(control, project);
    assert_eq!(instances.len(), 2, "{instances:?}");
    let ports: Vec<u16> = instances.iter().map(|r| r.2).collect();
    assert_eq!(serving(proxy.port).await.as_deref(), Some("r1"));
    control
        .set_secret(project, "TOKEN", "s3cret")
        .await
        .unwrap();
    let project_dir = app.data.path().join("projects").join(project.to_string());
    assert!(project_dir.is_dir());

    // Refused while a deployment is queued or running.
    let queued = control
        .queue_deployment(DeployRequest {
            project_id: project,
            version: Some("r2".into()),
            trigger: "test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    match control.delete_project(project, false).await {
        Err(aegis_control::ControlError::FailedPrecondition(m)) => {
            assert!(m.contains("deployment"), "{m}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(wait_deployment(control, queued).await.status, "Success");

    let deleted = control.delete_project(project, false).await.unwrap();
    assert_eq!(deleted.name, "web");
    assert_eq!(deleted.stopped_processes, 2);
    assert_eq!(deleted.domains, vec![DOMAIN.to_string()]);
    assert_eq!(deleted.secrets_removed, 1);

    // Nothing left in the proxy, the processes or on disk.
    assert!(proxy.ingress.routes().domain(DOMAIN).is_none());
    assert!(proxy.ingress.upstreams(&project.to_string()).is_empty());
    assert!(!matches!(
        via_proxy(proxy.port, "/version.txt").await,
        Ok((200, _))
    ));
    assert!(control.projection().get_project(&project).is_none());
    assert!(control.list_processes(Some(&project)).is_empty());
    assert!(control
        .supervisor()
        .list_processes()
        .iter()
        .all(|p| p.project_id != project));
    assert!(control.list_secrets(&project).is_empty());
    assert!(!project_dir.exists());
    let busy = busy_ports(&ports).await;
    assert!(
        busy.is_empty(),
        "ports still in use after the delete: {busy:?}"
    );
    // The history stays, ending with the deletion.
    let recorded = events_of(control, project, "ProjectDeleted").await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["name"], "web");
    assert_eq!(recorded[0]["domains"][0], DOMAIN);
    assert!(!events_of(control, project, "DeploymentCompleted")
        .await
        .is_empty());

    // The name and the domain can be used again at once.
    let again = control
        .register_project(RegisterProject {
            name: "web".into(),
            source_dir: Some(app.src.path().to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_ne!(again.id, project);
    assert_eq!(control.resolve_project("web").unwrap().id, again.id);
    control
        .map_domain(again.id, DOMAIN, "off", None, None)
        .await
        .unwrap();
    assert!(matches!(
        control.delete_project(project, false).await,
        Err(aegis_control::ControlError::NotFound(_))
    ));
    control.shutdown().await;

    // A daemon restart doesn't bring the deleted app back.
    let App {
        store, data, proxy, ..
    } = app;
    drop(proxy);
    let control = new_control(&store, data.path()).await;
    assert_eq!(control.recover().await, 0);
    assert!(control.projection().get_project(&project).is_none());
    assert!(control.list_processes(Some(&project)).is_empty());
    assert_eq!(control.resolve_project("web").unwrap().id, again.id);
    control.shutdown().await;
}

/// The ports among `ports` that can't be bound (still in use).
async fn busy_ports(ports: &[u16]) -> Vec<u16> {
    let mut busy = Vec::new();
    for port in ports {
        let mut free = false;
        for _ in 0..50 {
            if std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok() {
                free = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !free {
            busy.push(*port);
        }
    }
    busy
}

#[tokio::test]
async fn deleting_with_keep_releases_leaves_the_files() {
    let app = app("").await;
    let (control, project) = (&app.control, app.project);
    assert_eq!(deploy(&app, "r1", None).await.status, "Success");
    let deleted = control.delete_project(project, true).await.unwrap();
    assert!(deleted.releases_kept);
    let dir = app.data.path().join("projects").join(project.to_string());
    assert_eq!(deleted.kept_dir.as_deref(), Some(dir.to_str().unwrap()));
    assert!(dir.join("releases").join("r1").is_dir());
    assert!(control.projection().get_project(&project).is_none());
    // A restart's cleanup keeps them too.
    control.shutdown().await;
    let control = new_control(&app.store, app.data.path()).await;
    control.recover().await;
    assert!(dir.join("releases").join("r1").is_dir());
    control.shutdown().await;
}
