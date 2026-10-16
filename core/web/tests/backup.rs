//! Backups over the HTTP API: create, list, download, delete, retention,
//! name validation, and no access for API tokens.

mod common;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Session {
    cookie: String,
    csrf: String,
}

async fn raw(api: &Api, req: Request<Body>) -> (StatusCode, HeaderMap, Bytes) {
    let res = aegis_web::router(api.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    (
        status,
        headers,
        res.into_body().collect().await.unwrap().to_bytes(),
    )
}

async fn login(api: &Api) -> Session {
    let (status, headers, body) = raw(
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
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    Session {
        cookie: headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string(),
        csrf: body["csrf_token"].as_str().unwrap().to_string(),
    }
}

impl Session {
    async fn raw(
        &self,
        api: &Api,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, HeaderMap, Bytes) {
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
        raw(api, req.body(body).unwrap()).await
    }

    async fn call(&self, api: &Api, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let (status, _, bytes) = self.raw(api, method, uri, body).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

async fn event_types(api: &Api) -> Vec<(String, Value)> {
    api.state
        .control
        .store()
        .get_events()
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type.starts_with("Backup"))
        .map(|e| (e.event_type, serde_json::from_str(&e.payload_json).unwrap()))
        .collect()
}

#[tokio::test]
async fn backups_can_be_created_downloaded_and_deleted() {
    let api = api().await;
    let data_dir = api.state.control.data_dir().clone();
    std::fs::write(data_dir.join("projects-marker"), b"x").unwrap();
    let release = data_dir.join("projects/p1/releases/v1");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(release.join("app.js"), b"console.log(1)").unwrap();
    let user = login(&api).await;

    let (status, list) = user.call(&api, "GET", "/api/v1/backups", json!(null)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([]));

    // Database and keys only.
    let (status, small) = user
        .call(
            &api,
            "POST",
            "/api/v1/backups",
            json!({"include_releases": false}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{small}");
    assert_eq!(small["includes_releases"], false);
    assert!(small["size_bytes"].as_u64().unwrap() > 0);
    // The default ([backup] include_releases) includes releases.
    let (status, full) = user.call(&api, "POST", "/api/v1/backups", json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{full}");
    assert_eq!(full["includes_releases"], true);
    assert!(full["file_count"].as_u64().unwrap() > small["file_count"].as_u64().unwrap());

    let (_, list) = user.call(&api, "GET", "/api/v1/backups", json!(null)).await;
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            full["name"].as_str().unwrap(),
            small["name"].as_str().unwrap()
        ],
        "newest first"
    );

    // Download: a valid archive with the database, key and release.
    let name = full["name"].as_str().unwrap();
    let (status, headers, bytes) = user
        .raw(
            &api,
            "GET",
            &format!("/api/v1/backups/{name}/download"),
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/gzip");
    assert_eq!(
        headers["content-disposition"].to_str().unwrap(),
        format!("attachment; filename=\"{name}\"")
    );
    assert_eq!(bytes.len() as u64, full["size_bytes"].as_u64().unwrap());
    let tmp = tempfile::TempDir::new().unwrap();
    let copy = tmp.path().join(name);
    std::fs::write(&copy, &bytes).unwrap();
    let manifest = aegis_control::backup::verify_archive(&copy).unwrap();
    let paths: Vec<&str> = manifest.contents.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"aegis.db"), "{paths:?}");
    assert!(paths.contains(&"secrets.key"), "{paths:?}");
    assert!(
        paths.contains(&"projects/p1/releases/v1/app.js"),
        "{paths:?}"
    );
    assert!(!paths.contains(&"projects-marker"));

    // Names are validated: no paths, no other files.
    for bad in [
        "..%2F..%2Fsecrets.key",
        "secrets.key",
        "aegis-backup-..%2F..%2Fx.tar.gz",
        "aegis-backup-x.tar.gz%00",
    ] {
        let (status, _) = user
            .call(
                &api,
                "GET",
                &format!("/api/v1/backups/{bad}/download"),
                json!(null),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        let (status, _) = user
            .call(&api, "DELETE", &format!("/api/v1/backups/{bad}"), json!({}))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _) = user
        .call(
            &api,
            "GET",
            "/api/v1/backups/aegis-backup-19990101T000000000Z.tar.gz/download",
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Delete.
    let small_name = small["name"].as_str().unwrap();
    let (status, _) = user
        .call(
            &api,
            "DELETE",
            &format!("/api/v1/backups/{small_name}"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = user
        .call(
            &api,
            "DELETE",
            &format!("/api/v1/backups/{small_name}"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = user.call(&api, "GET", "/api/v1/backups", json!(null)).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    // Recorded, with who did it and nothing secret.
    let events = event_types(&api).await;
    let types: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        types,
        vec!["BackupCreated", "BackupCreated", "BackupDeleted"]
    );
    assert_eq!(events[0].1["trigger"], "manual");
    assert_eq!(events[0].1["actor"], "user:admin");
    assert_eq!(events[2].1["reason"], "manual");
    let mut fields: Vec<&str> = events[0]
        .1
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort();
    assert_eq!(
        fields,
        vec![
            "actor",
            "file_count",
            "includes_releases",
            "name",
            "size_bytes",
            "trigger"
        ]
    );
}

#[tokio::test]
async fn api_tokens_cannot_touch_backups() {
    let api = api().await;
    let user = login(&api).await;
    let (_, created) = user.call(&api, "POST", "/api/v1/backups", json!({})).await;
    let name = created["name"].as_str().unwrap();

    let (status, body) = api.get("/api/v1/backups").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = api.get(&format!("/api/v1/backups/{name}/download")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = api.call("POST", "/api/v1/backups", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = api
        .call("DELETE", &format!("/api/v1/backups/{name}"), json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Still there.
    let (_, list) = user.call(&api, "GET", "/api/v1/backups", json!(null)).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn old_backups_are_deleted_beyond_keep() {
    let api = api_configured(Default::default(), |s| s.backup.keep = 2).await;
    let user = login(&api).await;
    let mut names = Vec::new();
    for _ in 0..3 {
        let (status, b) = user
            .call(
                &api,
                "POST",
                "/api/v1/backups",
                json!({"include_releases": false}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{b}");
        names.push(b["name"].as_str().unwrap().to_string());
        // Names have millisecond resolution.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let (_, list) = user.call(&api, "GET", "/api/v1/backups", json!(null)).await;
    let left: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(left, vec![names[2].as_str(), names[1].as_str()]);
    let events = event_types(&api).await;
    let deleted: Vec<&Value> = events
        .iter()
        .filter(|(t, _)| t == "BackupDeleted")
        .map(|(_, p)| p)
        .collect();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0]["name"], names[0].as_str());
    assert_eq!(deleted[0]["reason"], "retention");
}
