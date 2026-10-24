//! Deleting an app, its health-check and drain timeouts from the dashboard,
//! and changing its source, over HTTP.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

struct User {
    cookie: String,
    csrf: String,
}

async fn send(api: &Api, req: Request<Body>) -> (StatusCode, String, Value) {
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

async fn login(api: &Api, username: &str, password: &str) -> User {
    let (status, cookie, body) = send(
        api,
        Request::post("/api/v1/auth/login")
            .header("host", "127.0.0.1:8420")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"username": username, "password": password}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    User {
        cookie: cookie.split(';').next().unwrap().to_string(),
        csrf: body["csrf_token"].as_str().unwrap().to_string(),
    }
}

impl User {
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
        let (status, _, value) = send(api, req.body(body).unwrap()).await;
        (status, value)
    }
}

async fn users(api: &Api) -> (User, User, User) {
    let admin = login(api, "admin", ADMIN_PASSWORD).await;
    for (user, role) in [("vic", "viewer"), ("dee", "deployer")] {
        let (status, body) = admin
            .call(
                api,
                "POST",
                "/api/v1/users",
                json!({"username": user, "password": "a-long-enough-password", "role": role}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let viewer = login(api, "vic", "a-long-enough-password").await;
    let deployer = login(api, "dee", "a-long-enough-password").await;
    (admin, viewer, deployer)
}

#[tokio::test]
async fn deleting_an_app_is_for_signed_in_admins_and_frees_its_name() {
    let api = api().await;
    let (admin, viewer, deployer) = users(&api).await;
    let (status, created) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "wrong", "repository_url": "https://github.com/acme/wrong.git"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/projects/wrong/secrets/TOKEN",
            json!({"value": "x"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = admin
        .call(&api, "POST", "/api/v1/projects/wrong/webhook", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/projects/wrong/schedule",
            json!({"hour": 3, "minute": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let uri = "/api/v1/projects/wrong?confirm=wrong";

    // Not with an API token, not as a viewer or deployer.
    let (status, body) = api.call("DELETE", uri, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("signed-in user"));
    for user in [&viewer, &deployer] {
        let (status, body) = user.call(&api, "DELETE", uri, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    // The name has to be repeated.
    for bad in [
        "/api/v1/projects/wrong",
        "/api/v1/projects/wrong?confirm=other",
    ] {
        let (status, body) = admin.call(&api, "DELETE", bad, json!({})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let (status, body) = admin
        .call(
            &api,
            "DELETE",
            "/api/v1/projects/nope?confirm=nope",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) = admin.call(&api, "DELETE", uri, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "wrong");
    assert_eq!(body["secrets_removed"], 1);
    assert_eq!(body["releases_kept"], false);
    let (status, _) = api.get("/api/v1/projects/wrong").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = api.get("/api/v1/projects").await;
    assert!(list.as_array().unwrap().is_empty(), "{list}");
    // Its webhook secret is gone with it.
    assert!(api.state.auth.webhook_secret(&id).await.unwrap().is_none());
    // The history stays, ending with the deletion.
    let (_, events) = api.get("/api/v1/events?limit=1000").await;
    let types: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"ProjectCreated"), "{types:?}");
    assert_eq!(types.last(), Some(&"ProjectDeleted"), "{types:?}");

    // The name can be registered again (with the right source).
    let (status, again) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "wrong", "repository_url": "https://github.com/acme/right.git"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
    assert_ne!(again["id"], created["id"]);
    assert!(again["schedule"].is_null(), "{again}");
    let (status, secrets) = admin
        .call(&api, "GET", "/api/v1/projects/wrong/secrets", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(secrets.as_array().unwrap().is_empty(), "{secrets}");
}

#[tokio::test]
async fn health_and_drain_timeouts_are_dashboard_settings() {
    let api = api().await;
    let src = TempDir::new().unwrap();
    write_app(src.path(), free_port());
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "slow", "source_dir": src.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let uri = "/api/v1/projects/slow/settings";

    for (bad, why) in [
        (
            json!({"health_check_timeout_secs": 0}),
            "between 1 and 3600",
        ),
        (
            json!({"health_check_timeout_secs": 3601}),
            "between 1 and 3600",
        ),
        (json!({"drain_timeout_secs": 3601}), "between 0 and 3600"),
    ] {
        let (status, body) = api.call("PUT", uri, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(why),
            "{body}"
        );
    }
    let (status, body) = api
        .call("PUT", uri, json!({"drain_timeout_secs": -1}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // The preview shows them overriding aegis.toml's values.
    let (status, preview) = api
        .call(
            "POST",
            "/api/v1/projects/slow/settings/preview",
            json!({"health_check_timeout_secs": 90, "drain_timeout_secs": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["changed"], true);
    let added: Vec<&str> = preview["diff"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["op"] == "+")
        .map(|l| l["text"].as_str().unwrap())
        .collect();
    assert!(
        added.contains(&"health_check_timeout_secs = 90"),
        "{added:?}"
    );
    assert!(added.contains(&"drain_timeout_secs = 0"), "{added:?}");

    let (status, saved) = api
        .call(
            "PUT",
            uri,
            json!({"health_check_timeout_secs": 90, "drain_timeout_secs": 0, "port": 3000}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["settings"]["health_check_timeout_secs"], 90);
    assert_eq!(saved["settings"]["drain_timeout_secs"], 0);
    // Like every setting, an omitted value is cleared.
    let (status, saved) = api.call("PUT", uri, json!({"port": 3000})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        saved["settings"]["health_check_timeout_secs"].is_null(),
        "{saved}"
    );
    assert!(saved["settings"]["drain_timeout_secs"].is_null(), "{saved}");
}

#[tokio::test]
async fn a_slow_app_deploys_with_a_longer_health_check_timeout() {
    let api = api().await;
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_app(src.path(), port);
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "slow", "source_dir": src.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    // aegis.toml says 10 s; the dashboard raises it, and the release built
    // next carries the dashboard's value.
    let (status, body) = api
        .call(
            "PUT",
            "/api/v1/projects/slow/settings",
            json!({"health_check_timeout_secs": 75, "drain_timeout_secs": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, queued) = api
        .call(
            "POST",
            "/api/v1/projects/slow/deploy",
            json!({"version": "r1"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let done = wait_deployment(&api, queued["deployment_id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "Success", "{done}");
    let project = api.state.control.resolve_project("slow").unwrap();
    let dir = api
        .state
        .control
        .data_dir()
        .join("projects")
        .join(project.id.to_string())
        .join("releases")
        .join("r1");
    let config = aegis_engine::ResolvedProjectConfig::load(&dir).unwrap();
    assert_eq!(config.health_check_timeout.as_secs(), 75);
    assert_eq!(config.drain_timeout.as_secs(), 1);
    let (status, _) = api
        .call("POST", "/api/v1/processes/slow/stop", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn changing_an_apps_source_is_validated_like_registration() {
    let api = api().await;
    let (admin, viewer, _) = users(&api).await;
    let (status, created) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "web", "repository_url": "https://example.invalid/wrong.git", "branch": "dev"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (status, _) = admin
        .call(&api, "POST", "/api/v1/projects/web/deploy-key", json!({}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let uri = "/api/v1/projects/web/source";
    let folder = TempDir::new().unwrap();
    let file = folder.path().join("a-file");
    std::fs::write(&file, "x").unwrap();

    // Viewers can't.
    let (status, _) = viewer
        .call(&api, "PUT", uri, json!({"source_dir": folder.path()}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for bad in [
        json!({}),
        json!({"repository_url": "https://github.com/acme/web.git", "source_dir": folder.path()}),
        json!({"source_dir": folder.path().join("missing")}),
        json!({"source_dir": file}),
        json!({"source_dir": folder.path(), "branch": "main"}),
        json!({"repository_url": "ext::sh -c touch% /tmp/pwned"}),
        json!({"repository_url": "--upload-pack=x"}),
        json!({"repository_url": "https://github.com/acme/web.git", "branch": "-x"}),
        json!({"repository_url": "https://github.com/acme/web.git", "source_subdir": "../up"}),
        json!({"bogus": 1}),
    ] {
        let (status, body) = admin.call(&api, "PUT", uri, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }

    // Git -> folder.
    let (status, body) = admin
        .call(
            &api,
            "PUT",
            uri,
            json!({"source_dir": folder.path(), "source_subdir": "apps/web/"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let canonical = folder.path().canonicalize().unwrap();
    assert_eq!(body["source_dir"], canonical.to_string_lossy().as_ref());
    assert_eq!(body["repository_url"], "");
    assert_eq!(body["source_subdir"], "apps/web");
    // The deploy key is kept.
    let (status, _) = admin
        .call(&api, "GET", "/api/v1/projects/web/deploy-key", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);

    // Folder -> git (a deploy token may, like registering an app).
    let (status, body) = api
        .call(
            "PUT",
            uri,
            json!({"repository_url": "git@github.com:acme/web.git"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["repository_url"], "git@github.com:acme/web.git");
    assert!(body["source_dir"].is_null(), "{body}");
    assert!(body["source_subdir"].is_null(), "{body}");
    // The branch stays unless given.
    assert_eq!(body["branch"], "dev");
    let (status, body) = api
        .call(
            "PUT",
            uri,
            json!({"repository_url": "git@github.com:acme/web.git", "branch": "main"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["branch"], "main");

    // Each change is recorded.
    let (_, events) = api.get("/api/v1/events?project=web&limit=100").await;
    let changes: Vec<&Value> = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event_type"] == "ProjectSourceChanged")
        .collect();
    assert_eq!(changes.len(), 3, "{events}");
    assert_eq!(changes[0]["payload"]["kind"], "folder");
    assert_eq!(
        changes[0]["payload"]["previous"]["repository_url"],
        "https://example.invalid/wrong.git"
    );
    assert_eq!(changes[1]["payload"]["kind"], "git");
}

#[tokio::test]
async fn a_changed_source_is_used_by_the_next_deployment() {
    let api = api().await;
    let (old, new) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let port = free_port();
    write_app(old.path(), port);
    write_app(new.path(), port);
    std::fs::write(new.path().join("marker.txt"), "new").unwrap();
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "moved", "source_dir": old.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = api
        .call(
            "PUT",
            "/api/v1/projects/moved/source",
            json!({"source_dir": new.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, queued) = api
        .call(
            "POST",
            "/api/v1/projects/moved/deploy",
            json!({"version": "r1"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let done = wait_deployment(&api, queued["deployment_id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "Success", "{done}");
    let project = api.state.control.resolve_project("moved").unwrap();
    let release = api
        .state
        .control
        .data_dir()
        .join("projects")
        .join(project.id.to_string())
        .join("releases")
        .join("r1");
    assert_eq!(
        std::fs::read_to_string(release.join("marker.txt")).unwrap(),
        "new"
    );
    let (status, _) = api
        .call("POST", "/api/v1/processes/moved/stop", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
}
