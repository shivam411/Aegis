//! App groups: two apps of one repository (a monorepo with `api/` and
//! `web/`) deployed together at one commit, in order, from one clone; only
//! the apps whose watch paths changed; a failing member stops the rest and
//! rolls back the ones already switched; group rollback.
#![cfg(unix)]

use aegis_control::{ControlPlane, ControlSettings, GroupSpec, MemberSpec, RegisterProject};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_projection::GroupDeploymentState;
use aegis_types::ProjectId;
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_app(dir: &Path, port: u16, start: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("aegis.toml"),
        format!(
            "[build]\ninstall_command = \"\"\nbuild_command = \"\"\nstart_command = \"{start}\"\n\
             [deploy]\nport = {port}\nhealth_check_url = \"http://127.0.0.1:{port}/\"\n\
             health_check_timeout_secs = 10\ndrain_timeout_secs = 1\n"
        ),
    )
    .unwrap();
}

const SERVE: &str = "exec python3 -m http.server $PORT --bind 127.0.0.1";

fn commit(repo: &Path, message: &str) -> String {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

async fn control(data: &Path) -> ControlPlane {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    EventStore::initialize_db(&pool).await.unwrap();
    let mut settings = ControlSettings::new(data.to_path_buf());
    settings.health_poll_interval = Duration::from_millis(100);
    settings.liveness_window = Duration::from_millis(500);
    settings.sample_interval = Duration::from_secs(3600);
    let control = ControlPlane::new(EventStore::new(pool), ProcessSupervisor::new(), settings)
        .await
        .unwrap();
    control.start();
    control
}

async fn wait_group(control: &ControlPlane, id: &str) -> GroupDeploymentState {
    for _ in 0..1200 {
        if let Some(run) = control.get_group_deployment(id) {
            if run.status != "Running" {
                return run;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("group deployment {id} did not finish");
}

async fn events(control: &ControlPlane, kind: &str) -> Vec<Value> {
    control
        .list_events(None, usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == kind)
        .map(|e| serde_json::from_str(&e.payload_json).unwrap())
        .collect()
}

fn live(control: &ControlPlane, id: ProjectId) -> (Option<String>, String) {
    let project = control.projection().get_project(&id).unwrap();
    let release = project.current_release.clone();
    let commit = release
        .as_ref()
        .and_then(|v| control.projection().find_release(&id, v))
        .map(|r| r.commit_sha)
        .unwrap_or_default();
    (release, commit)
}

fn status_of(run: &GroupDeploymentState, id: ProjectId) -> String {
    run.members
        .iter()
        .find(|m| m.project_id == id)
        .map(|m| m.status.clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_group_deploys_its_apps_in_order_from_one_clone() {
    let repo = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let r = repo.path();
    git(r, &["init", "-q", "-b", "main"]);
    let (api_port, web_port) = (free_port(), free_port());
    write_app(&r.join("api"), api_port, SERVE);
    write_app(&r.join("web"), web_port, SERVE);
    std::fs::create_dir_all(r.join("shared")).unwrap();
    std::fs::write(r.join("shared/util.txt"), "1").unwrap();
    std::fs::write(r.join("README.md"), "ty").unwrap();
    let c1 = commit(r, "c1");
    let url = r.to_string_lossy().to_string();

    let control = control(data.path()).await;
    let register = |name: &str, sub: &str| RegisterProject {
        name: name.into(),
        repository_url: url.clone(),
        branch: "main".into(),
        source_subdir: Some(sub.into()),
        ..Default::default()
    };
    let api = control
        .register_project(register("ty-back", "api"))
        .await
        .unwrap()
        .id;
    let web = control
        .register_project(register("ty-front", "web"))
        .await
        .unwrap()
        .id;
    let other = control
        .register_project(RegisterProject {
            name: "other".into(),
            repository_url: "https://github.com/acme/other.git".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;

    let spec = GroupSpec {
        name: "ty".into(),
        repository_url: url.clone(),
        branch: "main".into(),
        rollback_on_failure: None,
    };
    // Only apps of the same repository and branch.
    let err = control
        .create_group(
            spec.clone(),
            vec![MemberSpec {
                project_id: other,
                watch_paths: vec![],
            }],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not from the group's repository"), "{err}");
    let bad_path = control
        .create_group(
            spec.clone(),
            vec![MemberSpec {
                project_id: api,
                watch_paths: vec!["../etc/**".into()],
            }],
        )
        .await;
    assert!(bad_path.is_err());

    let group = control
        .create_group(
            spec.clone(),
            vec![
                MemberSpec {
                    project_id: web,
                    watch_paths: vec!["web/**".into(), "shared/**".into()],
                },
                MemberSpec {
                    project_id: api,
                    watch_paths: vec!["api/**".into(), "shared/**".into()],
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(control.group_of(&web).unwrap().id, group.id);
    // An app is in one group only.
    let err = control
        .create_group(
            GroupSpec {
                name: "ty2".into(),
                ..spec.clone()
            },
            vec![MemberSpec {
                project_id: web,
                watch_paths: vec![],
            }],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("already in group 'ty'"), "{err}");

    // 1. First deployment: both apps (nothing live yet), web then api, one
    // clone, one commit.
    let run1 = control
        .deploy_group("ty", None, true, "test")
        .await
        .unwrap();
    let run = wait_group(&control, &run1).await;
    assert_eq!(run.status, "Success", "{run:?}");
    assert_eq!(run.commit.as_deref(), Some(c1.as_str()));
    let deployed = events(&control, "GroupMemberDeployed").await;
    let order: Vec<&str> = deployed
        .iter()
        .map(|e| e["project_id"].as_str().unwrap())
        .collect();
    assert_eq!(order, vec![web.to_string(), api.to_string()]);
    assert_eq!(live(&control, web).1, c1);
    assert_eq!(live(&control, api).1, c1);
    let checkouts = events(&control, "GroupCheckoutReady").await;
    assert_eq!(checkouts.len(), 1, "one clone per group deployment");
    let checkout = checkouts[0]["checkout"].as_str().unwrap().to_string();
    let queued = events(&control, "DeploymentQueued").await;
    assert_eq!(queued.len(), 2);
    for q in &queued {
        assert!(
            q["source_dir"].as_str().unwrap().starts_with(&checkout),
            "member built from the group's checkout: {q}"
        );
    }
    assert!(
        !Path::new(&checkout).exists(),
        "the checkout is removed afterwards"
    );
    // A second request while nothing runs, with nothing changed: all skipped.
    let run_same = control
        .deploy_group("ty", None, true, "test")
        .await
        .unwrap();
    let same = wait_group(&control, &run_same).await;
    assert_eq!(same.status, "Success");
    assert_eq!(status_of(&same, web), "skipped");
    assert_eq!(status_of(&same, api), "skipped");

    // 2. Only api/ changed: web is skipped.
    std::fs::write(r.join("api/version.txt"), "2").unwrap();
    let c2 = commit(r, "c2 api only");
    let (web_before, _) = live(&control, web);
    let (api_before, _) = live(&control, api);
    let run2 = control
        .deploy_group("ty", None, true, "test")
        .await
        .unwrap();
    let run = wait_group(&control, &run2).await;
    assert_eq!(run.status, "Success", "{run:?}");
    assert_eq!(status_of(&run, web), "skipped");
    assert_eq!(status_of(&run, api), "deployed");
    assert_eq!(live(&control, web).0, web_before);
    assert_eq!(live(&control, api).1, c2);
    let skipped = events(&control, "GroupMemberSkipped").await;
    assert!(skipped
        .iter()
        .any(|e| e["project_id"] == web.to_string().as_str()
            && e["reason"].as_str().unwrap().contains("watch paths")));

    // 3. Group rollback: api goes back to the release it had before run 2.
    let rolled = control.rollback_group("ty").await.unwrap();
    assert_eq!(rolled.len(), 1);
    assert_eq!(rolled[0].project_id, api);
    assert_eq!(live(&control, api).0, api_before);
    assert_eq!(
        control.get_group_deployment(&run2).unwrap().status,
        "RolledBack"
    );

    // 4. api first, then a broken web: web fails, the rest stops, and api
    // (already switched) is rolled back.
    control
        .set_group_members(
            "ty",
            vec![
                MemberSpec {
                    project_id: api,
                    watch_paths: vec!["api/**".into(), "shared/**".into()],
                },
                MemberSpec {
                    project_id: web,
                    watch_paths: vec!["web/**".into(), "shared/**".into()],
                },
            ],
        )
        .await
        .unwrap();
    std::fs::write(r.join("shared/util.txt"), "3").unwrap();
    write_app(&r.join("web"), web_port, "exit 3");
    commit(r, "c3 breaks web");
    let (api_before, _) = live(&control, api);
    let run3 = control
        .deploy_group("ty", None, true, "test")
        .await
        .unwrap();
    let run = wait_group(&control, &run3).await;
    assert_eq!(run.status, "Failed", "{run:?}");
    assert_eq!(status_of(&run, api), "rolled_back");
    assert_eq!(status_of(&run, web), "failed");
    assert_eq!(live(&control, api).0, api_before);
    let failed = events(&control, "GroupDeploymentFailed").await;
    assert_eq!(
        failed.last().unwrap()["failed_project_id"],
        web.to_string().as_str()
    );
    assert!(failed.last().unwrap()["reason"]
        .as_str()
        .unwrap()
        .contains("ty-front"));

    // Deleting a member removes it from the group.
    control.delete_project(web, false).await.unwrap();
    assert_eq!(control.resolve_group("ty").unwrap().members.len(), 1);
    control.shutdown().await;
}
