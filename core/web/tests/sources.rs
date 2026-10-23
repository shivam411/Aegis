//! Deploy keys, folder scans and bulk import over HTTP: signed-in admins
//! only, the private key never leaves the daemon, and imports don't deploy.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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
    let text = String::from_utf8_lossy(&bytes).to_string();
    assert!(
        !text.contains("PRIVATE KEY"),
        "a private key leaked into a response: {text}"
    );
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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A folder with two checkouts: a Next.js app with a hard-coded port from
/// a GitHub repository, and a Python app without git.
fn scan_folder() -> TempDir {
    let dir = TempDir::new().unwrap();
    let img = dir.path().join("img-man");
    write(
        &img.join("package.json"),
        r#"{"scripts":{"build":"next build","start":"next start -p 4000"}}"#,
    );
    write(&img.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(
        &img.join(".git/config"),
        "[remote \"origin\"]\n\turl = https://x-token:ghp_hidden@github.com/acme/img-man.git\n",
    );
    write(
        &img.join(".env"),
        "MONGODB_URI=mongodb://u:pw-value@db/x\nPORT=4000\n",
    );
    write(&dir.path().join("notes/requirements.txt"), "");
    dir
}

async fn events_of(api: &Api, kind: &str) -> Vec<Value> {
    api.state
        .control
        .list_events(None, usize::MAX)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == kind)
        .map(|e| serde_json::from_str(&e.payload_json).unwrap())
        .collect()
}

fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if std::fs::read(&path)
                .map(|b| String::from_utf8_lossy(&b).contains(needle))
                .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }
    found
}

#[tokio::test]
async fn deploy_keys_scans_and_imports_are_for_signed_in_admins() {
    let folder = scan_folder();
    let allowed = folder.path().to_path_buf();
    let api = api_configured(Default::default(), move |s| s.import_dirs = vec![allowed]).await;
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "img", "repository_url": "https://github.com/acme/img-man.git"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    for (user, role) in [("vic", "viewer"), ("dee", "deployer")] {
        let (status, body) = admin
            .call(
                &api,
                "POST",
                "/api/v1/users",
                json!({"username": user, "password": "a-long-enough-password", "role": role}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let viewer = login(&api, "vic", "a-long-enough-password").await;
    let deployer = login(&api, "dee", "a-long-enough-password").await;
    let scan_path = folder.path().to_string_lossy().to_string();
    let requests = [
        ("GET", "/api/v1/projects/img/deploy-key", json!({})),
        ("POST", "/api/v1/projects/img/deploy-key", json!({})),
        ("DELETE", "/api/v1/projects/img/deploy-key", json!({})),
        ("POST", "/api/v1/projects/img/deploy-key/test", json!({})),
        ("GET", "/api/v1/apps/scan", json!({})),
        ("POST", "/api/v1/apps/scan", json!({"path": scan_path})),
        (
            "POST",
            "/api/v1/apps/import",
            json!({"apps": [{"name": "x", "source_dir": scan_path}]}),
        ),
        (
            "POST",
            "/api/v1/apps/check-access",
            json!({"repository_url": "https://github.com/acme/img-man.git"}),
        ),
    ];
    for (method, uri, body) in &requests {
        let (status, _) = api.call(method, uri, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "token: {method} {uri}");
        for user in [&viewer, &deployer] {
            let (status, _) = user.call(&api, method, uri, body.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }
    assert_eq!(api.state.control.list_projects().len(), 1);

    // --- Deploy keys (admin) ---
    let (status, _) = admin
        .call(&api, "GET", "/api/v1/projects/img/deploy-key", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, key) = admin
        .call(&api, "POST", "/api/v1/projects/img/deploy-key", json!({}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{key}");
    let public = key["public_key"].as_str().unwrap();
    assert!(public.starts_with("ssh-ed25519 AAAA"), "{public}");
    assert!(public.ends_with(" aegis:img"), "{public}");
    let fingerprint = key["fingerprint"].as_str().unwrap().to_string();
    assert!(fingerprint.starts_with("SHA256:"));
    // GitHub HTTPS URLs are cloned over SSH with the key.
    assert_eq!(key["clone_url"], "git@github.com:acme/img-man.git");
    assert_eq!(
        key["add_key_url"],
        "https://github.com/acme/img-man/settings/keys/new"
    );
    let (status, again) = admin
        .call(&api, "GET", "/api/v1/projects/img/deploy-key", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["fingerprint"], fingerprint.as_str());
    // A second key needs `replace`.
    let (status, _) = admin
        .call(&api, "POST", "/api/v1/projects/img/deploy-key", json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, replaced) = admin
        .call(
            &api,
            "POST",
            "/api/v1/projects/img/deploy-key",
            json!({"replace": true}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(replaced["fingerprint"], fingerprint.as_str());

    // Events name keys by fingerprint; nothing on disk holds a private key
    // in clear (the database is sealed, see aegis-secrets).
    let created = events_of(&api, "DeployKeyCreated").await;
    assert_eq!(created.len(), 2);
    assert_eq!(created[1]["replaced"], fingerprint.as_str());
    assert_eq!(created[1]["actor"], "user:admin");
    for e in api
        .state
        .control
        .list_events(None, usize::MAX)
        .await
        .unwrap()
    {
        assert!(!e.payload_json.contains("PRIVATE"), "{}", e.payload_json);
    }
    let data_dir = api.state.control.data_dir().clone();
    assert!(files_containing(&data_dir, "OPENSSH PRIVATE KEY").is_empty());

    let (status, _) = admin
        .call(&api, "DELETE", "/api/v1/projects/img/deploy-key", json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = admin
        .call(&api, "GET", "/api/v1/projects/img/deploy-key", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(events_of(&api, "DeployKeyDeleted").await.len(), 1);
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/projects/img/deploy-key/test",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Access checks are for GitHub repositories only.
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/apps/check-access",
            json!({"repository_url": "https://internal.example/x.git"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // --- Scan (admin) ---
    let (status, info) = admin
        .call(&api, "GET", "/api/v1/apps/scan", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["allowed_dirs"][0], scan_path.as_str());
    let outside = TempDir::new().unwrap();
    let (status, err) = admin
        .call(
            &api,
            "POST",
            "/api/v1/apps/scan",
            json!({"path": outside.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    let (status, scan) = admin
        .call(
            &api,
            "POST",
            "/api/v1/apps/scan",
            json!({"path": scan_path}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{scan}");
    let text = scan.to_string();
    assert!(!text.contains("ghp_hidden"), "credentials leaked: {text}");
    assert!(!text.contains("pw-value"), ".env value leaked: {text}");
    let candidates = scan["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2, "{scan}");
    let img = candidates.iter().find(|c| c["name"] == "img-man").unwrap();
    assert_eq!(img["repository_url"], "https://github.com/acme/img-man.git");
    assert_eq!(img["branch"], "main");
    assert_eq!(img["start_command"], "npx next start -p $PORT");
    assert_eq!(img["port"], 4000);
    assert_eq!(img["deploy_key"], "check");
    assert_eq!(img["env_names"], json!(["MONGODB_URI", "PORT"]));
    // The project registered above has the same repository and branch.
    assert_eq!(img["registered_as"], "img");
    let codes: Vec<&str> = img["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"hardcoded_port"), "{codes:?}");
    assert!(codes.contains(&"credentials_in_url"), "{codes:?}");
    let notes = candidates.iter().find(|c| c["name"] == "notes").unwrap();
    assert_eq!(notes["runtime"], "Python");
    assert_eq!(notes["registered_as"], Value::Null);

    // --- Import (admin): registered, not deployed ---
    let (status, result) = admin
        .call(
            &api,
            "POST",
            "/api/v1/apps/import",
            json!({"apps": [
                {"name": "notes", "source_dir": notes["path"], "runtime": "Python",
                 "settings": {"start_command": "python app.py", "port": 5055}},
                {"name": "img", "repository_url": "git@github.com:acme/img-man.git"},
                {"name": "bad", "repository_url": "git@github.com:acme/x.git", "source_dir": notes["path"]},
            ]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let apps = result["apps"].as_array().unwrap();
    assert!(apps[0]["project_id"].is_string(), "{result}");
    assert!(apps[0]["next_steps"].as_array().unwrap().len() >= 2);
    assert!(apps[1]["error"]
        .as_str()
        .unwrap()
        .contains("already exists"));
    assert!(apps[2]["error"].as_str().unwrap().contains("not both"));
    let (_, project) = admin
        .call(&api, "GET", "/api/v1/projects/notes", json!({}))
        .await;
    assert_eq!(project["settings"]["start_command"], "python app.py");
    assert_eq!(project["settings"]["port"], 5055);
    assert_eq!(project["current_release"], Value::Null);
    let (_, deployments) = admin
        .call(&api, "GET", "/api/v1/deployments", json!({}))
        .await;
    assert_eq!(deployments, json!([]));
    let imported = events_of(&api, "AppsImported").await;
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0]["count"], 1);
    assert_eq!(imported[0]["apps"][0]["name"], "notes");
}

#[tokio::test]
async fn scanning_is_off_without_allowed_folders() {
    let api = api().await;
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    let (status, info) = admin
        .call(&api, "GET", "/api/v1/apps/scan", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["allowed_dirs"], json!([]));
    let (status, err) = admin
        .call(&api, "POST", "/api/v1/apps/scan", json!({"path": "/opt"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("import_dirs"));
}

#[tokio::test]
async fn monorepo_apps_are_created_with_their_folder() {
    let api = api().await;
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    let (status, project) = admin
        .call(
            &api,
            "POST",
            "/api/v1/projects",
            json!({"name": "shop-web", "repository_url": "https://github.com/acme/shop.git",
                   "source_subdir": "apps/web/"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{project}");
    assert_eq!(project["source_subdir"], "apps/web");
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/projects",
            json!({"name": "evil", "repository_url": "https://github.com/acme/shop.git",
                   "source_subdir": "../etc"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
