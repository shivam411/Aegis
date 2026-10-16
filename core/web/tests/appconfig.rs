//! App configuration from the dashboard: secrets, plain environment
//! variables, the aegis.toml preview, and webhook setup.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{json, Value};
use std::time::Duration;
use tempfile::TempDir;

/// A signed-in user's session (cookie, CSRF token).
struct Session {
    cookie: String,
    csrf: String,
}

async fn login(api: &Api) -> Session {
    let res = router_call(
        api,
        Request::post("/api/v1/auth/login")
            .header("host", "127.0.0.1:8420")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"username": "admin", "password": ADMIN_PASSWORD}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(res.0, StatusCode::OK, "{}", res.2);
    Session {
        cookie: res.1.split(';').next().unwrap().to_string(),
        csrf: res.2["csrf_token"].as_str().unwrap().to_string(),
    }
}

/// (status, Set-Cookie, body)
async fn router_call(api: &Api, req: Request<Body>) -> (StatusCode, String, Value) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let res = aegis_web::router(api.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

impl Session {
    async fn call(&self, api: &Api, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:8420")
            .header("cookie", &self.cookie)
            .header("x-csrf-token", &self.csrf);
        let body = if method == "GET" {
            Body::empty()
        } else {
            req = req.header("content-type", "application/json");
            Body::from(body.to_string())
        };
        let (status, _, value) = router_call(api, req.body(body).unwrap()).await;
        (status, value)
    }
}

async fn wait_for_file(path: &std::path::Path, want: &str) -> String {
    let mut last = String::new();
    for _ in 0..100 {
        last = std::fs::read_to_string(path).unwrap_or_default();
        if last == want {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    last
}

#[tokio::test]
async fn secrets_and_variables_reach_the_app_and_never_leak() {
    let api = api().await;
    let app = TempDir::new().unwrap();
    let seen = app.path().join("seen.txt");
    std::fs::write(
        app.path().join("aegis.toml"),
        format!(
            "[build]\ninstall_command = \"\"\nbuild_command = \"\"\n\
             start_command = \"printf '%s|%s' \\\"$DB_URL\\\" \\\"$MODE\\\" > {}; exec sleep 60\"\n\
             [deploy]\nhealth_check_url = \"\"\nhealth_check_timeout_secs = 5\ndrain_timeout_secs = 1\n\
             [env]\nMODE = \"file\"\n",
            seen.display()
        ),
    )
    .unwrap();
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "vault", "source_dir": app.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let user = login(&api).await;

    // Secrets are for signed-in users only, never API tokens.
    for (method, uri) in [
        ("GET", "/api/v1/projects/vault/secrets"),
        ("PUT", "/api/v1/projects/vault/secrets/DB_URL"),
        ("DELETE", "/api/v1/projects/vault/secrets/DB_URL"),
        ("GET", "/api/v1/projects/vault/webhook"),
        ("POST", "/api/v1/projects/vault/webhook/test"),
    ] {
        let (status, _) = api.call(method, uri, json!({"value": "x"})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    // A project named like a section isn't caught by that rule.
    assert_eq!(
        api.get("/api/v1/projects/secrets").await.0,
        StatusCode::NOT_FOUND
    );

    let (status, body) = user
        .call(
            &api,
            "PUT",
            "/api/v1/projects/vault/secrets/DB_URL",
            json!({"value": "postgres://first"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], true);
    assert_eq!(body["restart_required"], false);
    for (name, value) in [("PORT", "1"), ("1BAD", "x"), ("OK", "nul\u{0}")] {
        let (status, _) = user
            .call(
                &api,
                "PUT",
                &format!("/api/v1/projects/vault/secrets/{name}"),
                json!({ "value": value }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
    }

    // A plain variable from the dashboard overrides [env].
    let (status, body) = api
        .call(
            "PUT",
            "/api/v1/projects/vault/settings",
            json!({"env": {"MODE": "dashboard"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settings"]["env"]["MODE"], "dashboard");
    let (status, _) = api
        .call(
            "PUT",
            "/api/v1/projects/vault/settings",
            json!({"env": {"BAD-NAME": "x"}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, queued) = api
        .call("POST", "/api/v1/projects/vault/deploy", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let d = wait_deployment(&api, queued["deployment_id"].as_str().unwrap()).await;
    assert_eq!(d["status"], "Success", "{d}");
    assert_eq!(
        wait_for_file(&seen, "postgres://first|dashboard").await,
        "postgres://first|dashboard"
    );

    // Changing a secret needs a restart, which picks up the new value.
    let (_, body) = user
        .call(
            &api,
            "PUT",
            "/api/v1/projects/vault/secrets/DB_URL",
            json!({"value": "postgres://second"}),
        )
        .await;
    assert_eq!(body["created"], false);
    assert_eq!(body["restart_required"], true);
    let (status, _) = api
        .call("POST", "/api/v1/processes/vault/restart", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        wait_for_file(&seen, "postgres://second|dashboard").await,
        "postgres://second|dashboard"
    );

    // Values never come back: not in the list, not in events.
    let (status, list) = user
        .call(&api, "GET", "/api/v1/projects/vault/secrets", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], "DB_URL");
    let (_, events) = api.get("/api/v1/events?project=vault&limit=500").await;
    let text = events.to_string();
    assert!(
        !text.contains("postgres://"),
        "secret value leaked into events"
    );
    assert!(text.contains("SecretUpdated"));
    // ...nor in the release directory.
    let releases = api.state.control.data_dir().join("projects");
    let mut stack = vec![releases];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "toml") {
                assert!(!std::fs::read_to_string(&path)
                    .unwrap()
                    .contains("postgres://"));
            }
        }
    }

    let (status, _) = user
        .call(
            &api,
            "DELETE",
            "/api/v1/projects/vault/secrets/DB_URL",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = user
        .call(
            &api,
            "DELETE",
            "/api/v1/projects/vault/secrets/DB_URL",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    api.state.control.shutdown().await;
}

#[tokio::test]
async fn settings_preview_shows_the_aegis_toml_diff() {
    let api = api().await;
    let app = TempDir::new().unwrap();
    std::fs::write(
        app.path().join("aegis.toml"),
        "[build]\nstart_command = \"./run\"\n[deploy]\nport = 3000\n",
    )
    .unwrap();
    api.call(
        "POST",
        "/api/v1/projects",
        json!({"name": "web", "source_dir": app.path()}),
    )
    .await;

    let (status, preview) = api
        .call(
            "POST",
            "/api/v1/projects/web/settings/preview",
            json!({"port": 4000, "env": {"LOG": "debug"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["base"], "source");
    assert_eq!(preview["changed"], true);
    let lines: Vec<String> = preview["diff"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            format!(
                "{}{}",
                l["op"].as_str().unwrap(),
                l["text"].as_str().unwrap()
            )
        })
        .collect();
    assert!(lines.contains(&"-port = 3000".to_string()), "{lines:?}");
    assert!(lines.contains(&"+port = 4000".to_string()), "{lines:?}");
    assert!(lines.contains(&"+LOG = \"debug\"".to_string()), "{lines:?}");
    assert!(
        lines.contains(&" start_command = \"./run\"".to_string()),
        "{lines:?}"
    );
    assert!(preview["current"].as_str().unwrap().contains("port = 3000"));

    // The preview validates like a save, and saves nothing.
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects/web/settings/preview",
            json!({"port": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, project) = api.get("/api/v1/projects/web").await;
    assert_eq!(project["settings"]["port"], Value::Null);

    // Unchanged settings: no change.
    let (_, same) = api
        .call("POST", "/api/v1/projects/web/settings/preview", json!({}))
        .await;
    assert_eq!(same["changed"], false);
    api.state.control.shutdown().await;
}

#[tokio::test]
async fn webhook_status_and_test_delivery() {
    let api = api().await;
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "hooked", "repository_url": "https://github.com/example/app.git"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let user = login(&api).await;

    let (status, hook) = user
        .call(&api, "GET", "/api/v1/projects/hooked/webhook", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{hook}");
    assert_eq!(hook["configured"], false);
    assert_eq!(hook["branch"], "main");
    assert!(hook["path"].as_str().unwrap().starts_with("/hooks/github/"));
    let (status, _) = user
        .call(
            &api,
            "POST",
            "/api/v1/projects/hooked/webhook/test",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = user
        .call(&api, "POST", "/api/v1/projects/hooked/webhook", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, result) = user
        .call(
            &api,
            "POST",
            "/api/v1/projects/hooked/webhook/test",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["status"], "pong");

    let (_, hook) = user
        .call(&api, "GET", "/api/v1/projects/hooked/webhook", json!({}))
        .await;
    assert_eq!(hook["configured"], true);
    let recent = hook["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 1, "{hook}");
    assert_eq!(recent[0]["event"], "ping");
    assert_eq!(recent[0]["status"], "pong");
    assert!(recent[0]["delivery"]
        .as_str()
        .unwrap()
        .starts_with("aegis-test-"));
    api.state.control.shutdown().await;
}
