//! The endpoints the web dashboard relies on, and the dashboard's own files.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::json;
use std::process::Command;
use tempfile::TempDir;
use tower::ServiceExt;

#[tokio::test]
async fn dashboard_files_are_public_with_a_strict_policy() {
    let api = api().await;
    for (path, content_type, needle) in [
        ("/", "text/html", "/assets/app.js"),
        ("/assets/app.js", "text/javascript", "EventSource"),
        ("/assets/style.css", "text/css", "--accent"),
        ("/assets/favicon.svg", "image/svg+xml", "<svg"),
    ] {
        // No credentials: the files hold no data, the API behind them does.
        let res = aegis_web::router(api.state.clone())
            .oneshot(
                Request::get(path)
                    .header("host", "127.0.0.1:8420")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        let headers = res.headers().clone();
        assert!(
            headers["content-type"]
                .to_str()
                .unwrap()
                .starts_with(content_type),
            "{path}"
        );
        let csp = headers["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("script-src 'self'"), "{path}: {csp}");
        assert!(!csp.contains("unsafe-inline"), "{path}: {csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{path}: {csp}");
        assert_eq!(headers["x-frame-options"], "DENY");
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains(needle), "{path}");
    }

    // No inline script or style in the page itself (the policy would block it).
    let res = aegis_web::router(api.state.clone())
        .oneshot(
            Request::get("/")
                .header("host", "127.0.0.1:8420")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html = res.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(html.to_vec()).unwrap();
    assert!(!html.contains("style="));
    assert!(!html.contains("<script>"));

    let (status, _) = api.get("/assets/nope.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = api.get("/assets/..%2Fsrc%2Flib.rs").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The API keeps its deny-everything policy.
    let res = aegis_web::router(api.state.clone())
        .oneshot(
            Request::get("/api/v1/status")
                .header("host", "127.0.0.1:8420")
                .header("authorization", api.bearer())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.headers()["content-security-policy"],
        "default-src 'none'; frame-ancestors 'none'"
    );
    // The dashboard page is still subject to the Host check.
    let res = aegis_web::router(api.state.clone())
        .oneshot(
            Request::get("/")
                .header("host", "evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn host_metrics() {
    let api = api().await;
    let (status, host) = api.get("/api/v1/host").await;
    assert_eq!(status, StatusCode::OK, "{host}");
    assert!(host["cpu_count"].as_u64().unwrap() >= 1);
    assert!(host["memory_total_bytes"].as_u64().unwrap() > 0);
    assert!(
        host["memory_used_bytes"].as_u64().unwrap() <= host["memory_total_bytes"].as_u64().unwrap()
    );
    assert!(host["disk_total_bytes"].as_u64().unwrap() > 0);
    assert_eq!(host["load_average"].as_array().unwrap().len(), 3);
    let cpu = host["cpu_percent"].as_f64().unwrap();
    assert!((0.0..=100.0).contains(&cpu), "{cpu}");
}

#[tokio::test]
async fn detect_from_a_directory_and_a_git_repository() {
    let api = api().await;
    let src = TempDir::new().unwrap();
    std::fs::write(
        src.path().join("package.json"),
        r#"{"name":"shop","scripts":{"build":"echo build","start":"node server.js"}}"#,
    )
    .unwrap();

    let (status, d) = api
        .call("POST", "/api/v1/detect", json!({"source_dir": src.path()}))
        .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    assert_eq!(d["runtime"], "Node.js");
    assert!(
        d["start_command"].as_str().unwrap().contains("start"),
        "{d}"
    );
    assert!(d["port"].as_u64().is_some());

    // The same code as a git repository: cloned into scratch space.
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(src.path())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    let (status, d) = api
        .call(
            "POST",
            "/api/v1/detect",
            json!({"repository_url": src.path(), "branch": "main"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    assert_eq!(d["runtime"], "Node.js");
    // Scratch clones are cleaned up.
    let tmp = api.state.control.data_dir().join("tmp");
    let leftovers = std::fs::read_dir(&tmp).map(|d| d.count()).unwrap_or(0);
    assert_eq!(leftovers, 0);

    for bad in [
        json!({}),
        json!({"source_dir": "/definitely/not/here"}),
        json!({"repository_url": "--upload-pack=touch /tmp/x"}),
        json!({"source_dir": src.path(), "repository_url": "https://example.com/x.git"}),
    ] {
        let (status, body) = api.call("POST", "/api/v1/detect", bad.clone()).await;
        assert!(status.is_client_error(), "{bad}: {status} {body}");
    }
}

#[tokio::test]
async fn settings_build_log_and_deployment_events() {
    let api = api().await;
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_app(src.path(), port);
    let (status, project) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "web", "source_dir": src.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        project["settings"],
        json!({
            "install_command": null, "build_command": null, "test_command": null,
            "start_command": null, "port": null, "health_check_url": null
        })
    );

    let (status, body) = api
        .call("PUT", "/api/v1/projects/web/settings", json!({"port": 0}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = api
        .call(
            "PUT",
            "/api/v1/projects/web/settings",
            json!({"health_check_url": "ftp://x"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = api
        .call(
            "PUT",
            "/api/v1/projects/web/settings",
            json!({"colour": "blue"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = api
        .call("PUT", "/api/v1/projects/nope/settings", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, project) = api
        .call(
            "PUT",
            "/api/v1/projects/web/settings",
            json!({"build_command": " echo $AEGIS_RELEASE_VERSION-dash > version.txt "}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{project}");
    assert_eq!(
        project["settings"]["build_command"],
        "echo $AEGIS_RELEASE_VERSION-dash > version.txt"
    );

    let (_, accepted) = api
        .call(
            "POST",
            "/api/v1/projects/web/deploy",
            json!({"version": "r1"}),
        )
        .await;
    let id = accepted["deployment_id"].as_str().unwrap().to_string();
    let d = wait_deployment(&api, &id).await;
    assert_eq!(d["status"], "Success", "{d}");
    assert_eq!(serving(port).await.as_deref(), Some("r1-dash"));

    let (status, log) = api
        .get(&format!("/api/v1/deployments/{id}/log?lines=200"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(log["deployment_id"], id.as_str());
    let lines: Vec<&str> = log["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(
        lines.iter().any(|l| l.contains("Applied project settings")),
        "{lines:?}"
    );
    let (_, short) = api
        .get(&format!("/api/v1/deployments/{id}/log?lines=1"))
        .await;
    assert_eq!(short["lines"].as_array().unwrap().len(), 1);

    let (status, events) = api.get(&format!("/api/v1/deployments/{id}/events")).await;
    assert_eq!(status, StatusCode::OK);
    let types: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    for want in [
        "DeploymentQueued",
        "BuildStageClone",
        "BuildStagePromote",
        "DeploymentCompleted",
    ] {
        assert!(types.contains(&want), "{want} missing from {types:?}");
    }
    assert!(events
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["payload"]["deployment_id"] == id.as_str()));
    assert!(!types.contains(&"ProjectSettingsUpdated"));

    for path in [
        "/api/v1/deployments/nope/log",
        "/api/v1/deployments/nope/events",
    ] {
        let (status, _) = api.get(path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
    let unknown = "0190a0b0-0000-7000-8000-000000000000";
    let (status, _) = api.get(&format!("/api/v1/deployments/{unknown}/log")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The settings change is recorded with who made it.
    let (_, events) = api.get("/api/v1/events?project=web&limit=500").await;
    let change = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_type"] == "ProjectSettingsUpdated")
        .expect("settings event");
    assert_eq!(change["payload"]["actor"], "token:tests");
    api.state.control.shutdown().await;
}
