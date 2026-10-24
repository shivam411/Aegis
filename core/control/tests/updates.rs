//! "New commits" checks: a repository app compared with its branch (commits
//! behind, the commits, the changed files filtered by watch paths), and a
//! server-folder app compared with the folder's HEAD and uncommitted files.
#![cfg(unix)]

use aegis_control::{
    ControlPlane, ControlSettings, DeployRequest, GroupSpec, MemberSpec, RegisterProject,
};
use aegis_event_store::EventStore;
use aegis_process::ProcessSupervisor;
use aegis_types::{DeploymentId, ProjectId};
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
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

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
    settings.update_check_interval = Duration::ZERO;
    let control = ControlPlane::new(EventStore::new(pool), ProcessSupervisor::new(), settings)
        .await
        .unwrap();
    control.start();
    control
}

async fn deploy(control: &ControlPlane, project: ProjectId, version: &str) {
    let id = control
        .queue_deployment(DeployRequest {
            project_id: project,
            version: Some(version.into()),
            trigger: "test".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    wait(control, id).await;
}

async fn wait(control: &ControlPlane, id: DeploymentId) {
    for _ in 0..600 {
        if let Some(d) = control.get_deployment(&id) {
            if d.status == "Success" {
                return;
            }
            assert!(
                matches!(d.status.as_str(), "Queued" | "InProgress"),
                "deployment failed: {d:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("deployment did not finish");
}

/// Past the per-repository fetch rate limit.
async fn after_rate_limit() {
    tokio::time::sleep(Duration::from_millis(3200)).await;
}

#[tokio::test]
async fn new_commits_on_the_branch_and_in_a_folder() {
    let repo = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let r = repo.path();
    git(r, &["init", "-q", "-b", "main"]);
    let port = free_port();
    std::fs::write(
        r.join("aegis.toml"),
        format!(
            "[build]\ninstall_command = \"\"\nbuild_command = \"\"\n\
             start_command = \"exec python3 -m http.server $PORT --bind 127.0.0.1\"\n\
             [deploy]\nport = {port}\nhealth_check_url = \"http://127.0.0.1:{port}/\"\n\
             health_check_timeout_secs = 10\ndrain_timeout_secs = 1\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::write(r.join("src/app.txt"), "1").unwrap();
    let c1 = commit(r, "first");
    let control = control(data.path()).await;

    // A repository app, deployed at c1.
    let app = control
        .register_project(RegisterProject {
            name: "web".into(),
            repository_url: r.to_string_lossy().to_string(),
            branch: "main".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    // Before its first deployment: nothing live, so it is "relevant".
    let before = control.check_updates(&app).await.unwrap();
    assert!(!before.deployed);
    assert!(before.relevant);
    assert_eq!(before.head.as_deref(), Some(c1.as_str()));
    deploy(&control, app, "r1").await;
    after_rate_limit().await;
    let now = control.check_updates(&app).await.unwrap();
    assert_eq!(now.kind, "git");
    assert_eq!(now.live_commit.as_deref(), Some(c1.as_str()));
    assert!(now.up_to_date, "{now:?}");
    assert_eq!(now.behind, Some(0));

    // Two new commits on the branch.
    std::fs::write(r.join("README.md"), "docs").unwrap();
    commit(r, "docs only");
    std::fs::write(r.join("src/app.txt"), "2").unwrap();
    let c3 = commit(r, "change the app");
    after_rate_limit().await;
    let behind = control.check_updates(&app).await.unwrap();
    assert_eq!(behind.head.as_deref(), Some(c3.as_str()));
    assert_eq!(behind.behind, Some(2), "{behind:?}");
    assert!(!behind.up_to_date);
    assert!(behind.relevant);
    assert_eq!(behind.commits.len(), 2);
    assert_eq!(behind.commits[0].message, "change the app");
    assert_eq!(behind.commits[0].sha, c3);
    assert_eq!(behind.commits[0].author, "Ada");
    assert_eq!(behind.commits[1].message, "docs only");
    let mut files = behind.changed_files.clone();
    files.sort();
    assert_eq!(files, vec!["README.md", "src/app.txt"]);
    // The cached result is what the project list shows.
    assert_eq!(control.cached_updates(&app).unwrap().behind, Some(2));

    // In a group, only files matching the app's watch paths count.
    control
        .create_group(
            GroupSpec {
                name: "g".into(),
                repository_url: r.to_string_lossy().to_string(),
                branch: "main".into(),
                rollback_on_failure: None,
            },
            vec![MemberSpec {
                project_id: app,
                watch_paths: vec!["docs/**".into()],
            }],
        )
        .await
        .unwrap();
    after_rate_limit().await;
    let group = control.check_group_updates("g").await.unwrap();
    let (_, member) = &group.members[0];
    assert_eq!(member.behind, Some(2));
    assert!(member.changed_files.is_empty(), "{member:?}");
    assert!(!member.relevant);
    assert_eq!(group.changed_apps, 0);
    control
        .set_group_members(
            "g",
            vec![MemberSpec {
                project_id: app,
                watch_paths: vec!["src/**".into()],
            }],
        )
        .await
        .unwrap();
    after_rate_limit().await;
    let group = control.check_group_updates("g").await.unwrap();
    assert_eq!(group.members[0].1.changed_files, vec!["src/app.txt"]);
    assert_eq!(group.changed_apps, 1);

    // A folder app: its HEAD is read from .git; uncommitted files counted.
    let folder = control
        .register_project(RegisterProject {
            name: "folder".into(),
            source_dir: Some(r.to_path_buf()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let mut settings = aegis_config::ProjectOverrides {
        port: Some(free_port()),
        ..Default::default()
    };
    settings.health_check_url = Some(String::new());
    control
        .update_project_settings(folder, settings)
        .await
        .unwrap();
    deploy(&control, folder, "f1").await;
    let release = control.list_releases(&folder).pop().unwrap();
    assert_eq!(release.commit_sha, c3);
    assert_eq!(release.source_dirty, Some(false));
    assert_eq!(
        aegis_control::read_folder_head(r).as_deref(),
        Some(c3.as_str())
    );
    let clean = control.check_updates(&folder).await.unwrap();
    assert_eq!(clean.kind, "folder");
    assert!(clean.up_to_date, "{clean:?}");
    assert_eq!(clean.dirty, Some(0));

    std::fs::write(r.join("src/app.txt"), "3").unwrap();
    let dirty = control.check_updates(&folder).await.unwrap();
    assert_eq!(dirty.dirty, Some(1));
    assert!(!dirty.up_to_date);
    let c4 = commit(r, "folder change");
    let newer = control.check_updates(&folder).await.unwrap();
    assert_eq!(newer.head.as_deref(), Some(c4.as_str()));
    assert_eq!(newer.behind, Some(1));
    assert_eq!(newer.commits[0].message, "folder change");
    assert_eq!(newer.dirty, Some(0));
    control.shutdown().await;
}
