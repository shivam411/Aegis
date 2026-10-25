//! App access: accounts limited to a list of apps, and apps hidden behind a password.

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
async fn users_see_only_their_apps_and_hidden_apps_need_the_password() {
    let api = api().await;
    let src = tempfile::TempDir::new().unwrap();
    for name in ["shop", "blog", "wiki"] {
        let (status, body) = api
            .call(
                "POST",
                "/api/v1/projects",
                json!({"name": name, "source_dir": src.path()}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;

    // A bad app name creates nothing; a limited account needs a role below admin.
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/users",
            json!({"username": "dee", "password": PW, "role": "deployer", "apps": ["ghost"]}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/users",
            json!({"username": "boss", "password": PW, "role": "admin", "apps": ["shop"]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, users) = admin.call(&api, "GET", "/api/v1/users", json!({})).await;
    assert_eq!(users.as_array().unwrap().len(), 1);

    let (status, body) = admin
        .call(
            &api,
            "POST",
            "/api/v1/users",
            json!({"username": "dee", "password": PW, "role": "deployer", "apps": ["shop"]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["all_apps"], false);
    assert_eq!(body["apps"], json!(["shop"]));
    let (_, body) = admin
        .call(
            &api,
            "POST",
            "/api/v1/users",
            json!({"username": "all", "password": PW, "role": "viewer"}),
        )
        .await;
    assert_eq!(body["all_apps"], true);

    // dee sees one app, everywhere.
    let dee = login(&api, "dee", PW).await;
    let (_, list) = dee.call(&api, "GET", "/api/v1/projects", json!({})).await;
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["shop"]);
    let (_, status_body) = dee.call(&api, "GET", "/api/v1/status", json!({})).await;
    assert_eq!(status_body["project_count"], 1);
    for uri in [
        "/api/v1/projects/blog",
        "/api/v1/projects/blog/releases",
        "/api/v1/projects/blog/resources",
        "/api/v1/projects/blog/metrics",
        "/api/v1/projects/blog/updates",
        "/api/v1/processes/blog/logs",
        "/api/v1/deployments?project=blog",
        "/api/v1/events?project=blog",
    ] {
        let (status, _) = dee.call(&api, "GET", uri, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let (status, _) = dee
        .call(&api, "POST", "/api/v1/projects/blog/deploy", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The allowed app works (404 would mean the guard hid it).
    let (status, _) = dee
        .call(&api, "GET", "/api/v1/projects/shop", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    // Deployments and events of other apps never show, in lists or by id.
    let mut blog_deployment = String::new();
    for app in ["shop", "blog"] {
        let (status, body) = api
            .call("POST", &format!("/api/v1/projects/{app}/deploy"), json!({}))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        if app == "blog" {
            blog_deployment = body["deployment_id"].as_str().unwrap().to_string();
        }
    }
    let (_, all) = api.get("/api/v1/deployments").await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    let (_, mine) = dee
        .call(&api, "GET", "/api/v1/deployments", json!({}))
        .await;
    let mine = mine.as_array().unwrap();
    assert_eq!(mine.len(), 1, "{mine:?}");
    let (_, shop) = api.get("/api/v1/projects/shop").await;
    assert_eq!(mine[0]["project_id"], shop["id"]);
    for suffix in ["", "/log", "/events"] {
        let (status, _) = dee
            .call(
                &api,
                "GET",
                &format!("/api/v1/deployments/{blog_deployment}{suffix}"),
                json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{suffix}");
    }
    // Events about hidden apps are left out of the shared feed.
    let (_, events) = dee
        .call(&api, "GET", "/api/v1/events?limit=100", json!({}))
        .await;
    assert!(!events.to_string().contains("blog"), "{events}");
    // Unlocking an app you aren't given fails like a wrong password.
    let (status, _) = dee
        .call(
            &api,
            "POST",
            "/api/v1/projects/blog/unlock",
            json!({"password": PW}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Changing the list applies to the open session at once.
    let (status, body) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/users/dee",
            json!({"apps": ["shop", "wiki"]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, list) = dee.call(&api, "GET", "/api/v1/projects", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    let (_, body) = admin
        .call(&api, "PUT", "/api/v1/users/dee", json!({"all_apps": true}))
        .await;
    assert_eq!(body["all_apps"], true);
    let (_, list) = dee.call(&api, "GET", "/api/v1/projects", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 3);

    // Hide an app: the admin who hid it still sees it, others don't.
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/projects/blog/hide",
            json!({"password": "short"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/projects/blog/hide",
            json!({"password": "hidden-blog-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, list) = admin.call(&api, "GET", "/api/v1/projects", json!({})).await;
    let blog = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "blog")
        .expect("the admin session stays unlocked");
    assert_eq!(blog["protected"], true);
    let admin2 = login(&api, "admin", ADMIN_PASSWORD).await;
    let (_, list) = admin2
        .call(&api, "GET", "/api/v1/projects", json!({}))
        .await;
    assert_eq!(list.as_array().unwrap().len(), 2, "{list}");
    let (status, _) = admin2
        .call(&api, "DELETE", "/api/v1/projects/blog/hide", json!({}))
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "can't change what you can't see"
    );
    // API tokens are not limited.
    assert_eq!(
        api.get("/api/v1/projects")
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // Wrong password, then the right one; the unlock lasts for the session.
    let (status, _) = dee
        .call(
            &api,
            "POST",
            "/api/v1/projects/blog/unlock",
            json!({"password": "not-the-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = dee
        .call(
            &api,
            "POST",
            "/api/v1/projects/ghost/unlock",
            json!({"password": "not-the-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unknown looks like wrong");
    let (_, list) = dee.call(&api, "GET", "/api/v1/projects", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    let (status, _) = dee
        .call(
            &api,
            "POST",
            "/api/v1/projects/blog/unlock",
            json!({"password": "hidden-blog-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = dee.call(&api, "GET", "/api/v1/projects", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 3);
    let (status, _) = dee
        .call(&api, "POST", "/api/v1/projects/blog/lock", json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = dee.call(&api, "GET", "/api/v1/projects", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 2);

    // Only admins hide, and a new hide password ends earlier unlocks.
    let (status, _) = dee
        .call(
            &api,
            "PUT",
            "/api/v1/projects/shop/hide",
            json!({"password": "hidden-shop-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/projects/blog/hide",
            json!({"password": "another-hidden-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Signing out ends unlocks.
    let admin3 = login(&api, "admin", ADMIN_PASSWORD).await;
    let (_, list) = admin3
        .call(&api, "GET", "/api/v1/projects", json!({}))
        .await;
    assert_eq!(list.as_array().unwrap().len(), 2);

    // Unhiding makes it visible again for everyone.
    let (status, _) = admin
        .call(&api, "DELETE", "/api/v1/projects/blog/hide", json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = dee.call(&api, "GET", "/api/v1/projects", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["protected"] == false));

    // Tokens can't hide or unlock.
    let (status, _) = api
        .call(
            "PUT",
            "/api/v1/projects/blog/hide",
            json!({"password": "hidden-blog-password"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
