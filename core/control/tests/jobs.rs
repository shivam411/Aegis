//! Jobs: commands run in the live release with the app's environment.
#![cfg(unix)]

use aegis_control::{
    ControlPlane, ControlSettings, DeployRequest, JobRun, JobSpec, RegisterProject,
};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_projection::DeploymentState;
use aegis_types::{DeploymentId, ProjectId};
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[allow(dead_code)]
struct Harness {
    control: ControlPlane,
    store: EventStore,
    data: TempDir,
    src: TempDir,
    port: u16,
    project_id: ProjectId,
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
    settings
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

fn write_app(src: &Path, port: u16, build: &str, start: Option<&str>, extra: &str) {
    let start = start
        .map(str::to_string)
        .unwrap_or_else(|| "exec python3 -m http.server $PORT --bind 127.0.0.1".into());
    std::fs::write(
        src.join("aegis.toml"),
        format!(
            r#"
[project]
name = "web"

[build]
install_command = ""
build_command = "{build}"
start_command = "{start}"

[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/"
health_check_timeout_secs = 10
drain_timeout_secs = 2
{extra}
"#
        ),
    )
    .unwrap();
}

const VERSION_BUILD: &str = "echo $AEGIS_RELEASE_VERSION > version.txt";

async fn harness() -> Harness {
    let data = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_app(src.path(), port, VERSION_BUILD, None, "");
    let store = new_store().await;
    let control = ControlPlane::new(
        store.clone(),
        ProcessSupervisor::new(),
        settings(data.path()),
    )
    .await
    .unwrap();
    control.start();
    let project = control
        .register_project(RegisterProject {
            name: "web".into(),
            source_dir: Some(src.path().to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap();
    Harness {
        control,
        store,
        data,
        src,
        port,
        project_id: project.id,
    }
}

async fn deploy(h: &Harness, version: &str) -> DeploymentState {
    let id = h
        .control
        .queue_deployment(DeployRequest {
            project_id: h.project_id,
            version: Some(version.to_string()),
            trigger: "test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    wait_deployment(&h.control, id).await
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

async fn http_get(port: u16, path: &str) -> Option<String> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .ok()?;
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).await.ok()?;
    buf.split("\r\n\r\n").nth(1).map(|b| b.trim().to_string())
}

async fn serving(port: u16) -> Option<String> {
    http_get(port, "/version.txt").await
}

#[allow(dead_code)]
async fn wait_serving(port: u16, version: &str) {
    for _ in 0..100 {
        if serving(port).await.as_deref() == Some(version) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{version} is not being served");
}

async fn finished(h: &Harness, job_id: &str) -> JobRun {
    for _ in 0..200 {
        let runs = h
            .control
            .list_job_runs(h.project_id, job_id, 5)
            .await
            .unwrap();
        if let Some(run) = runs.first() {
            if run.status != "running" {
                return run.clone();
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {job_id} did not finish");
}

fn spec(name: &str, command: &str) -> JobSpec {
    JobSpec {
        name: name.into(),
        schedule: "*/5 * * * *".into(),
        command: command.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn jobs_run_in_the_live_release_with_its_environment() {
    let h = harness().await;

    // Nothing live yet: a manual run is refused, and says why.
    let job = h
        .control
        .create_job(
            h.project_id,
            spec(
                "report",
                "echo release=$AEGIS_RELEASE_VERSION; cat version.txt; echo oops >&2",
            ),
        )
        .await
        .unwrap();
    assert!(job.next_run.is_some());
    let err = h
        .control
        .run_job_now(h.project_id, &job.id)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no live release"), "{err}");

    assert_eq!(deploy(&h, "r1").await.status, "Success");
    h.control
        .set_secret(h.project_id, "TOKEN", "s3cret")
        .await
        .unwrap();
    let env_job = h
        .control
        .create_job(h.project_id, spec("env", "echo token=$TOKEN"))
        .await
        .unwrap();

    // Output (stdout and stderr), the release directory and the version.
    let started = h.control.run_job_now(h.project_id, &job.id).await.unwrap();
    assert_eq!(started.status, "running");
    let run = finished(&h, &job.id).await;
    assert_eq!(run.status, "success", "{run:?}");
    assert_eq!(run.exit_code, Some(0));
    assert!(
        run.output_tail.contains("release=r1"),
        "{}",
        run.output_tail
    );
    assert!(run.output_tail.contains("oops"), "{}", run.output_tail);
    assert_eq!(run.trigger, "manual");

    // Secrets reach the job like the app.
    h.control
        .run_job_now(h.project_id, &env_job.id)
        .await
        .unwrap();
    let run = finished(&h, &env_job.id).await;
    assert!(
        run.output_tail.contains("token=s3cret"),
        "{}",
        run.output_tail
    );

    // A failing command keeps its exit code.
    let failing = h
        .control
        .create_job(h.project_id, spec("fail", "echo bad; exit 3"))
        .await
        .unwrap();
    h.control
        .run_job_now(h.project_id, &failing.id)
        .await
        .unwrap();
    let run = finished(&h, &failing.id).await;
    assert_eq!((run.status.as_str(), run.exit_code), ("failed", Some(3)));

    // A job that outlives its timeout is killed, children included, and
    // the same job can't run twice at once.
    let slow = h
        .control
        .create_job(
            h.project_id,
            JobSpec {
                timeout_secs: Some(1),
                ..spec("slow", "sleep 30 & echo started; wait")
            },
        )
        .await
        .unwrap();
    h.control.run_job_now(h.project_id, &slow.id).await.unwrap();
    assert!(h.control.run_job_now(h.project_id, &slow.id).await.is_err());
    let run = finished(&h, &slow.id).await;
    assert_eq!(run.status, "timeout", "{run:?}");
    assert!(run.output_tail.contains("timeout"), "{}", run.output_tail);

    // Validation, duplicates, update and delete.
    for bad in [
        JobSpec {
            schedule: "61 * * * *".into(),
            ..spec("x", "true")
        },
        JobSpec {
            name: "bad/name".into(),
            ..spec("x", "true")
        },
        spec("x", "  "),
    ] {
        assert!(h.control.create_job(h.project_id, bad).await.is_err());
    }
    assert!(h
        .control
        .create_job(h.project_id, spec("report", "true"))
        .await
        .is_err());
    let updated = h
        .control
        .update_job(
            h.project_id,
            &job.id,
            aegis_control::JobUpdate {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!updated.enabled && updated.next_run.is_none());
    h.control
        .delete_job(h.project_id, &failing.id)
        .await
        .unwrap();
    assert_eq!(h.control.list_jobs(h.project_id).await.unwrap().len(), 3);

    // The audit trail records starts and finishes, never the output.
    let events = h
        .control
        .list_events(Some(&h.project_id), 1000)
        .await
        .unwrap();
    for t in [
        "JobCreated",
        "JobRunStarted",
        "JobRunFinished",
        "JobUpdated",
        "JobDeleted",
    ] {
        assert!(events.iter().any(|e| e.event_type == t), "{t}");
    }
    assert!(!events.iter().any(|e| e.payload_json.contains("s3cret")));
}
