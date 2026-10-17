//! Self-update over the HTTP API: everyone can see the status, only signed-in
//! admins can check now or apply, and applying is refused with the admin
//! command when the daemon can't replace its binary. No network access: the
//! "GitHub API" is a closed loopback port.

mod common;

use aegis_auth::Role;
use aegis_update::{UpdateManager, UpdateSettings};
use aegis_web::update::ControlUpdateHooks;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

struct Session {
    cookie: String,
    csrf: String,
}

async fn login(api: &Api, username: &str, password: &str) -> Session {
    let res = aegis_web::router(api.state.clone())
        .oneshot(
            Request::post("/api/v1/auth/login")
                .header("host", "127.0.0.1:8420")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": username, "password": password}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let body: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    Session {
        cookie,
        csrf: body["csrf_token"].as_str().unwrap().to_string(),
    }
}

impl Session {
    async fn call(&self, api: &Api, method: &str, uri: &str) -> (StatusCode, Value) {
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
            Body::from("{}")
        };
        api.send(req.body(body).unwrap()).await
    }
}

async fn user(api: &Api, name: &str, role: Role) -> Session {
    let password = format!("{name}-password-123");
    api.state
        .auth
        .create_user(name, &password, role)
        .await
        .unwrap()
        .unwrap();
    login(api, name, &password).await
}

/// An API whose updater already knows of v0.99.0 (same
/// major version as the test build) and can't write its "binary".
const TEST_RELEASE_KEY: &str = "RWQBAgMEBQYHCAcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH";
const TEST_RELEASE_KEY_ID: &str = "0807060504030201";

async fn api_with_updates(data: &std::path::Path) -> Api {
    api_with_keys(data, TEST_RELEASE_KEY).await
}

async fn api_with_keys(data: &std::path::Path, keys: &str) -> Api {
    let mut api = api().await;
    let state_dir = data.join("update");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(
        state_dir.join("state.json"),
        json!({"last_check": {"checked_at": "2026-10-16T00:00:00Z", "release": {
            "tag": "v0.99.0", "name": "Aegis 0.99.0",
            "html_url": "https://github.com/shivam411/Aegis/releases/tag/v0.99.0",
            "published_at": "2026-10-15T00:00:00Z", "prerelease": false, "draft": false,
            "assets": []}}})
        .to_string(),
    )
    .unwrap();
    let mut settings = UpdateSettings::new(
        "0.1.0",
        data.join("not-there").join("aegis-daemon"),
        state_dir,
    );
    // Nothing listens there, so "Check now" fails fast without network access.
    settings.api_base = Some(format!("http://127.0.0.1:{}", free_port()));
    settings.helper_unit = data.join("no-helper.path");
    // A platform with tar.gz releases, whatever runs the test.
    settings.target = aegis_update::Target::from_consts("linux", "x86_64");
    settings.manual_command = "sudo aegis-daemon self-update".into();
    // A release key (well-formed; nothing is signed in these tests).
    settings.keyring = aegis_update::Keyring::parse(keys).unwrap();
    let manager = UpdateManager::new(
        settings,
        Arc::new(ControlUpdateHooks(api.state.control.clone())),
    );
    api.state = api.state.clone().with_updates(manager);
    api
}

#[tokio::test]
async fn roles_and_refusal() {
    let data = tempfile::TempDir::new().unwrap();
    let api = api_with_updates(data.path()).await;
    let viewer = user(&api, "vera", Role::Viewer).await;
    let deployer = user(&api, "dave", Role::Deployer).await;
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;

    // Everyone signed in sees that an update is available.
    for who in [&viewer, &deployer, &admin] {
        let (status, body) = who.call(&api, "GET", "/api/v1/system/update").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["update_available"], true, "{body}");
        assert_eq!(body["latest"]["version"], "v0.99.0");
        assert_eq!(body["current_version"], "0.1.0");
        assert_eq!(body["mode"], "manual");
        assert_eq!(body["manual_command"], "sudo aegis-daemon self-update");
        assert_eq!(body["can_apply"], false);
        assert_eq!(body["release_keys"], json!([TEST_RELEASE_KEY_ID]));
        assert!(body["blocked_reason"].is_null(), "{body}");
    }
    // So do API tokens.
    let (status, _) = api.get("/api/v1/system/update").await;
    assert_eq!(status, StatusCode::OK);

    // Viewers and deployers can't check or apply.
    for who in [&viewer, &deployer] {
        for path in ["/api/v1/system/update/check", "/api/v1/system/update/apply"] {
            let (status, body) = who.call(&api, "POST", path).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
        }
    }
    // Neither can API tokens, even deploy-scoped ones.
    for path in ["/api/v1/system/update/check", "/api/v1/system/update/apply"] {
        let (status, _) = api.call("POST", path, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }

    // Admins can. The binary isn't writable, so applying is refused with
    // the command an administrator runs instead, and nothing is backed up.
    let (status, body) = admin
        .call(&api, "POST", "/api/v1/system/update/apply")
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("sudo aegis-daemon self-update"),
        "{body}"
    );
    assert!(api.state.control.list_backups().unwrap().is_empty());
    // A failed check is reported, not an error.
    let (status, body) = admin
        .call(&api, "POST", "/api/v1/system/update/check")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["check_error"].is_string(), "{body}");
    // The previous result is kept.
    assert_eq!(body["update_available"], true);
}

/// A build without a release key (the placeholder key file) never offers
/// "Upgrade now" and says why.
#[tokio::test]
async fn without_a_release_key_upgrades_are_blocked() {
    let data = tempfile::TempDir::new().unwrap();
    let api = api_with_keys(data.path(), "# placeholder\n").await;
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    let (status, body) = admin.call(&api, "GET", "/api/v1/system/update").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["update_available"], true);
    assert_eq!(body["release_keys"], json!([]));
    assert_eq!(body["can_apply"], false);
    let reason = body["blocked_reason"].as_str().unwrap();
    assert!(reason.contains("no release signing key"), "{reason}");
    assert!(reason.contains("--allow-unsigned"), "{reason}");
    let (status, body) = admin
        .call(&api, "POST", "/api/v1/system/update/apply")
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no release signing key"),
        "{body}"
    );
    assert!(api.state.control.list_backups().unwrap().is_empty());
}

#[tokio::test]
async fn without_an_updater_the_status_says_so() {
    let api = api().await;
    let (status, body) = api.get("/api/v1/system/update").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["check_enabled"], false);
    assert_eq!(body["update_available"], false);
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    let (status, _) = admin
        .call(&api, "POST", "/api/v1/system/update/check")
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}
