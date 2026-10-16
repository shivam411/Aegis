//! User accounts and roles: who may read, operate and administer.

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
    body: Value,
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
        body,
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
async fn roles_limit_what_each_user_can_do() {
    let api = api().await;
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    assert_eq!(admin.body["role"], "admin");

    // Admins manage users.
    for (name, role) in [("vic", "viewer"), ("dee", "deployer"), ("ada", "admin")] {
        let (status, body) = admin
            .call(
                &api,
                "POST",
                "/api/v1/users",
                json!({"username": name, "password": PW, "role": role}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["role"], role);
    }
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/users",
            json!({"username": "vic", "password": PW, "role": "viewer"}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    for bad in [
        json!({"username": "Bad Name", "password": PW, "role": "viewer"}),
        json!({"username": "x", "password": "short", "role": "viewer"}),
        json!({"username": "x", "password": PW, "role": "root"}),
    ] {
        let (status, _) = admin.call(&api, "POST", "/api/v1/users", bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (_, users) = admin.call(&api, "GET", "/api/v1/users", json!({})).await;
    assert_eq!(users.as_array().unwrap().len(), 4);
    assert!(!users.to_string().contains("password_hash"));

    let vic = login(&api, "vic", PW).await;
    let dee = login(&api, "dee", PW).await;
    assert_eq!(vic.body["role"], "viewer");
    let (_, session) = dee
        .call(&api, "GET", "/api/v1/auth/session", json!({}))
        .await;
    assert_eq!(session["role"], "deployer");

    // Everyone can read.
    for user in [&vic, &dee] {
        let (status, _) = user.call(&api, "GET", "/api/v1/projects", json!({})).await;
        assert_eq!(status, StatusCode::OK);
    }
    // Viewers can't operate; deployers can (404: the app doesn't exist, but
    // the request got past the role check).
    let (status, body) = vic
        .call(&api, "POST", "/api/v1/projects/ghost/deploy", json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("deployer"));
    for uri in [
        "/api/v1/projects/ghost/deploy",
        "/api/v1/processes/ghost/restart",
    ] {
        let (status, _) = dee.call(&api, "POST", uri, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    // Only admins change configuration, credentials and users.
    for (method, uri) in [
        ("POST", "/api/v1/projects"),
        ("PUT", "/api/v1/projects/ghost/settings"),
        ("GET", "/api/v1/tokens"),
        ("GET", "/api/v1/users"),
        ("POST", "/api/v1/users"),
        ("GET", "/api/v1/projects/ghost/secrets"),
    ] {
        let (status, _) = dee.call(&api, method, uri, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    // Anyone can change their own password.
    let (status, _) = vic
        .call(
            &api,
            "POST",
            "/api/v1/auth/password",
            json!({"current_password": PW, "new_password": "another-long-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // A role change applies to existing sessions at once.
    let (status, body) = admin
        .call(&api, "PUT", "/api/v1/users/dee", json!({"role": "viewer"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = dee
        .call(&api, "POST", "/api/v1/projects/ghost/deploy", json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // An admin reset signs the user out.
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/users/dee",
            json!({"password": "reset-long-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = dee.call(&api, "GET", "/api/v1/projects", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    login(&api, "dee", "reset-long-password").await;

    // The last admin stays; nobody deletes themselves.
    let (status, _) = admin
        .call(&api, "DELETE", "/api/v1/users/admin", json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let ada = login(&api, "ada", PW).await;
    let (status, _) = ada
        .call(&api, "DELETE", "/api/v1/users/admin", json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = ada
        .call(&api, "PUT", "/api/v1/users/ada", json!({"role": "viewer"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _) = admin.call(&api, "GET", "/api/v1/projects", json!({})).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "deleted users are signed out"
    );

    // API tokens never manage users.
    let (status, _) = api.call("GET", "/api/v1/users", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Everything is in the audit log, with who did it.
    let (_, events) = ada
        .call(&api, "GET", "/api/v1/events?limit=200", json!({}))
        .await;
    let types: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    for t in [
        "UserCreated",
        "UserRoleChanged",
        "PasswordReset",
        "UserDeleted",
    ] {
        assert!(types.contains(&t), "{t} missing from {types:?}");
    }
    let deleted = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_type"] == "UserDeleted")
        .unwrap();
    assert_eq!(deleted["payload"]["actor"], "user:ada");
    api.state.control.shutdown().await;
}
