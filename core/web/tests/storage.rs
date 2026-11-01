//! Disk use and cleanup, and who may look at the server's Docker containers.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const PW: &str = "a-long-enough-password";

/// A signed-in user's cookie and CSRF token.
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

#[tokio::test]
async fn storage_report_and_cleanup_are_admin_only_and_free_old_temp_files() {
    let api = api().await;
    // API tokens can't read or clean.
    let (status, _) = api.get("/api/v1/system/storage").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = api
        .call("POST", "/api/v1/system/storage/cleanup", json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    let tmp = api.state.control.data_dir().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let old = tmp.join("leftover.bin");
    std::fs::write(&old, vec![0u8; 100_000]).unwrap();
    let three_days = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86400);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(three_days)
        .unwrap();
    let fresh = tmp.join("fresh.bin");
    std::fs::write(&fresh, b"in use").unwrap();

    let (status, report) = admin
        .call(&api, "GET", "/api/v1/system/storage", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let keys: Vec<&str> = report["categories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "releases",
            "old_builds",
            "logs",
            "backups",
            "database",
            "temp"
        ]
    );
    let temp = report["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "temp")
        .unwrap();
    assert_eq!(temp["reclaimable_bytes"], 100_000);
    assert_eq!(report["auto_cleanup"], false);

    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/system/storage/cleanup",
            json!({"targets": ["everything"]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, done) = admin
        .call(
            &api,
            "POST",
            "/api/v1/system/storage/cleanup",
            json!({"targets": ["temp"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["freed_bytes"], 100_000);
    assert!(!old.exists(), "old temp file removed");
    assert!(fresh.exists(), "recent temp file kept");

    // Automatic cleanup is off until switched on; the log age is checked.
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/system/storage/auto",
            json!({"log_days": 0}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, set) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/system/storage/auto",
            json!({"enabled": true, "log_days": 7}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!(set["auto_cleanup"], true);
    assert_eq!(set["log_days"], 7);

    let (_, events) = admin
        .call(&api, "GET", "/api/v1/events?limit=1000", json!({}))
        .await;
    assert!(events.to_string().contains("StorageCleaned"));
}

#[tokio::test]
async fn docker_is_for_the_owner_unless_shared() {
    let api = api().await;
    let owner = login(&api, "admin", ADMIN_PASSWORD).await;
    for (name, role) in [("bob", "admin"), ("vic", "viewer")] {
        let (status, body) = owner
            .call(
                &api,
                "POST",
                "/api/v1/users",
                json!({"username": name, "password": PW, "role": role}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["docker_access"], false);
    }
    let (_, me) = owner.call(&api, "GET", "/api/v1/users", json!({})).await;
    let admin_row = me
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["username"] == "admin")
        .unwrap();
    assert_eq!(admin_row["owner"], true);
    assert_eq!(admin_row["docker_access"], true);

    // The owner gets an answer (Docker may be missing here: `available: false`).
    let (status, body) = owner
        .call(&api, "GET", "/api/v1/docker/containers", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["available"].is_boolean());
    // Not even another admin, and never a token.
    let bob = login(&api, "bob", PW).await;
    let vic = login(&api, "vic", PW).await;
    for user in [&bob, &vic] {
        let (status, _) = user
            .call(&api, "GET", "/api/v1/docker/containers", json!({}))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, _) = api.get("/api/v1/docker/containers").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Shared with a viewer: allowed to look.
    let (status, body) = owner
        .call(
            &api,
            "PUT",
            "/api/v1/users/vic",
            json!({"docker_access": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["docker_access"], true);
    let (status, _) = vic
        .call(&api, "GET", "/api/v1/docker/containers", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    // A container name that isn't one is refused before docker runs.
    let (status, _) = vic
        .call(
            &api,
            "GET",
            "/api/v1/docker/containers/--all/logs",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Taking it back works too.
    owner
        .call(
            &api,
            "PUT",
            "/api/v1/users/vic",
            json!({"docker_access": false}),
        )
        .await;
    let (status, _) = vic
        .call(&api, "GET", "/api/v1/docker/containers", json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
