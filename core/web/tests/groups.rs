//! App groups over HTTP: who may do what, membership rules, and the group
//! webhook (a member's own webhook ignores pushes, so a push deploys it once).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const REPO: &str = "file:///nonexistent/aegis-tests/ty.git";

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

fn hook(path: &str, event: &str, delivery: &str, body: &str, secret: &str) -> Request<Body> {
    Request::post(path)
        .header("host", "127.0.0.1:8420")
        .header("content-type", "application/json")
        .header("x-github-event", event)
        .header("x-github-delivery", delivery)
        .header(
            "x-hub-signature-256",
            aegis_webhook::sign_github_payload(secret, body.as_bytes()),
        )
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn groups_roles_membership_and_webhook() {
    let api = api().await;
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    for (user, role) in [("vic", "viewer"), ("dee", "deployer")] {
        let (status, _) = admin
            .call(
                &api,
                "POST",
                "/api/v1/users",
                json!({"username": user, "password": "a-long-enough-password", "role": role}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let viewer = login(&api, "vic", "a-long-enough-password").await;
    let deployer = login(&api, "dee", "a-long-enough-password").await;
    let mut ids = Vec::new();
    for (name, sub) in [
        ("ty-front", "frontend_v2"),
        ("ty-back", "backend"),
        ("ty-dev-front", "frontend_v2"),
    ] {
        let branch = if name.starts_with("ty-dev") {
            "develop"
        } else {
            "main"
        };
        let (status, p) = admin
            .call(
                &api,
                "POST",
                "/api/v1/projects",
                json!({"name": name, "repository_url": REPO, "branch": branch, "source_subdir": sub}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{p}");
        ids.push(p["id"].as_str().unwrap().to_string());
    }

    // Viewers read but can't create.
    let new_group = json!({"name": "ty", "repository_url": REPO, "branch": "main",
        "members": [{"project": "ty-front", "watch_paths": ["frontend_v2/**", "shared/**"]},
                    {"project": "ty-back", "watch_paths": ["backend/**"]}]});
    let (status, _) = viewer
        .call(&api, "POST", "/api/v1/groups", new_group.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = deployer
        .call(&api, "POST", "/api/v1/groups", new_group.clone())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // An app of another branch can't join.
    let mut wrong = new_group.clone();
    wrong["members"][1]["project"] = json!("ty-dev-front");
    let (status, body) = admin.call(&api, "POST", "/api/v1/groups", wrong).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("develop"),
        "{body}"
    );

    let (status, group) = admin.call(&api, "POST", "/api/v1/groups", new_group).await;
    assert_eq!(status, StatusCode::CREATED, "{group}");
    assert_eq!(group["members"][0]["name"], "ty-front");
    assert_eq!(group["members"][1]["watch_paths"], json!(["backend/**"]));
    let gid = group["id"].as_str().unwrap().to_string();
    let (status, list) = viewer
        .call(&api, "GET", "/api/v1/groups", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, front) = api.get("/api/v1/projects/ty-front").await;
    assert_eq!(front["group"]["name"], "ty");

    // Reorder and change watch paths; bad paths are refused.
    let (status, g) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/groups/ty/members/ty-back",
            json!({"position": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{g}");
    assert_eq!(g["members"][0]["name"], "ty-back");
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/groups/ty/members/ty-back",
            json!({"watch_paths": ["/etc/**"]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = viewer
        .call(
            &api,
            "PUT",
            "/api/v1/groups/ty/members/ty-back",
            json!({"position": 2}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Deployers may deploy and roll back (409s here: nothing to roll back,
    // and a group without apps); viewers may not.
    let (status, _) = viewer
        .call(&api, "POST", "/api/v1/groups/ty/rollback", json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = deployer
        .call(&api, "POST", "/api/v1/groups/ty/rollback", json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/groups",
            json!({"name": "empty", "repository_url": REPO}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = deployer
        .call(&api, "POST", "/api/v1/groups/empty/deploy", json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no apps"));

    // The group webhook secret: signed-in admins only.
    let (status, _) = api
        .call("POST", "/api/v1/groups/ty/webhook", json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, hook_secret) = admin
        .call(&api, "POST", "/api/v1/groups/ty/webhook", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hook_secret["path"], format!("/hooks/github/groups/{gid}"));
    let group_secret = hook_secret["secret"].as_str().unwrap().to_string();
    let (_, app_secret) = admin
        .call(&api, "POST", "/api/v1/projects/ty-front/webhook", json!({}))
        .await;
    let app_secret = app_secret["secret"].as_str().unwrap().to_string();

    let sha = "0123456789abcdef0123456789abcdef01234567";
    let push = |branch: &str| {
        json!({"ref": format!("refs/heads/{branch}"), "after": sha, "deleted": false}).to_string()
    };
    // A member's own webhook ignores pushes: the group deploys it.
    let (status, _, body) = send(
        &api,
        hook(
            &format!("/hooks/github/{}", ids[0]),
            "push",
            "a1",
            &push("main"),
            &app_secret,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "ignored");
    assert!(
        body["reason"].as_str().unwrap().contains("group 'ty'"),
        "{body}"
    );
    assert!(api.state.control.list_deployments(None).is_empty());

    let group_hook = format!("/hooks/github/groups/{gid}");
    let (status, _, _) = send(
        &api,
        hook(&group_hook, "push", "g0", &push("main"), "wrong"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, body) = send(
        &api,
        hook(&group_hook, "push", "g1", &push("develop"), &group_secret),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "ignored");
    let (status, _, body) = send(
        &api,
        hook(&group_hook, "push", "g2", &push("main"), &group_secret),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "queued");
    let run = body["deployment_id"].as_str().unwrap().to_string();
    // The same delivery again is a duplicate.
    let (status, _, body) = send(
        &api,
        hook(&group_hook, "push", "g2", &push("main"), &group_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "duplicate");
    // The run was started at the pushed commit (the clone fails: the
    // repository doesn't exist).
    let mut finished = Value::Null;
    for _ in 0..100 {
        let (_, runs) = api.get("/api/v1/groups/ty/deployments").await;
        let last = runs.as_array().unwrap().last().cloned().unwrap();
        if last["status"] != "Running" {
            finished = last;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(finished["id"], run.as_str());
    assert_eq!(finished["commit"], sha);
    assert_eq!(finished["trigger"], "github-push");
    assert_eq!(finished["status"], "Failed", "{finished}");
    let (status, deliveries) = admin
        .call(&api, "GET", "/api/v1/groups/ty/webhook", Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deliveries["configured"], true);
    let statuses: Vec<&str> = deliveries["recent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, vec!["duplicate", "queued", "ignored"]);

    // Deleting the group frees its apps; its webhook stops working.
    let (status, _) = admin
        .call(&api, "DELETE", "/api/v1/groups/ty", json!({}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, front) = api.get("/api/v1/projects/ty-front").await;
    assert!(front["group"].is_null());
    let (status, _, _) = send(&api, hook(&group_hook, "ping", "g3", "{}", &group_secret)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
