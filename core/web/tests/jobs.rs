//! Jobs over HTTP: who may create, run and read them.

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
async fn jobs_are_created_by_admins_and_only_run_by_tokens() {
    let api = api().await;
    let src = tempfile::TempDir::new().unwrap();
    api.call(
        "POST",
        "/api/v1/projects",
        json!({"name": "shop", "source_dir": src.path()}),
    )
    .await;
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    admin
        .call(
            &api,
            "POST",
            "/api/v1/users",
            json!({"username": "vic", "password": PW, "role": "viewer"}),
        )
        .await;
    let vic = login(&api, "vic", PW).await;

    let job = json!({"name": "nightly", "schedule": "30 2 * * *", "command": "echo hi"});
    // Tokens and viewers can't create jobs.
    let (status, _) = api
        .call("POST", "/api/v1/projects/shop/jobs", job.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = vic
        .call(&api, "POST", "/api/v1/projects/shop/jobs", job.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    for bad in [
        json!({"name": "x", "schedule": "nope", "command": "true"}),
        json!({"name": "x", "schedule": "* * * * *", "command": ""}),
        json!({"name": "x", "schedule": "* * * * *", "command": "true", "timeout_secs": 0}),
    ] {
        let (status, body) = admin
            .call(&api, "POST", "/api/v1/projects/shop/jobs", bad.clone())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, created) = admin
        .call(&api, "POST", "/api/v1/projects/shop/jobs", job.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert!(created["next_run"].as_str().unwrap().contains("02:30:00"));
    let id = created["id"].as_str().unwrap().to_string();
    let (status, _) = admin
        .call(&api, "POST", "/api/v1/projects/shop/jobs", job)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Everyone can read; tokens can't change; viewers can't run.
    let (_, list) = vic
        .call(&api, "GET", "/api/v1/projects/shop/jobs", json!({}))
        .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, _) = api
        .call(
            "PUT",
            &format!("/api/v1/projects/shop/jobs/{id}"),
            json!({"enabled": false}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = vic
        .call(
            &api,
            "POST",
            &format!("/api/v1/projects/shop/jobs/{id}/run"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A token may run it (409: the app has no live release yet).
    let (status, body) = api
        .call(
            "POST",
            &format!("/api/v1/projects/shop/jobs/{id}/run"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, body) = admin
        .call(
            &api,
            "PUT",
            &format!("/api/v1/projects/shop/jobs/{id}"),
            json!({"enabled": false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert!(body["next_run"].is_null());
    let (status, _) = admin
        .call(
            &api,
            "DELETE",
            &format!("/api/v1/projects/shop/jobs/{id}"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, runs) = admin
        .call(
            &api,
            "GET",
            &format!("/api/v1/projects/shop/jobs/{id}/runs"),
            json!({}),
        )
        .await;
    assert_eq!(runs["error"]["code"], "not_found");
}
