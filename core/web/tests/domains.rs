//! Domain mapping and proxy status over the HTTP API.

mod common;

use aegis_web::router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send(api: &Api, req: Request<Body>) -> (StatusCode, Value) {
    let res = router(api.state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Signs in; returns (cookie, csrf token).
async fn login(api: &Api) -> (String, String) {
    let res = router(api.state.clone())
        .oneshot(
            Request::post("/api/v1/auth/login")
                .header("host", "127.0.0.1:8420")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "admin", "password": ADMIN_PASSWORD}).to_string(),
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
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, body["csrf_token"].as_str().unwrap().to_string())
}

async fn as_user(
    api: &Api,
    session: &(String, String),
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    send(
        api,
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:8420")
            .header("content-type", "application/json")
            .header("cookie", &session.0)
            .header("x-csrf-token", &session.1)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn domains_are_mapped_by_users_and_read_by_tokens() {
    let api = api().await;
    let session = login(&api).await;
    for name in ["shop", "blog"] {
        let (status, _) = api
            .call("POST", "/api/v1/projects", json!({"name": name}))
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    // Proxy disabled: status says so, and domains can still be recorded.
    let (status, proxy) = api.get("/api/v1/proxy").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(proxy["enabled"], false);

    let (status, domain) = as_user(
        &api,
        &session,
        "POST",
        "/api/v1/projects/shop/domains",
        json!({"domain": "Shop.Example.com", "tls": "off"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{domain}");
    assert_eq!(domain["domain"], "shop.example.com");
    assert_eq!(domain["tls"], "off");
    assert!(domain["cert"].is_null());

    // Default TLS mode is ACME.
    let (status, domain) = as_user(
        &api,
        &session,
        "POST",
        "/api/v1/projects/shop/domains",
        json!({"domain": "www.shop.example.com"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{domain}");
    assert_eq!(domain["tls"], "acme");

    for (body, code) in [
        (json!({"domain": "*.example.com"}), StatusCode::BAD_REQUEST),
        (json!({"domain": "10.0.0.1"}), StatusCode::BAD_REQUEST),
        (
            json!({"domain": "a.example.com", "tls": "maybe"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"domain": "a.example.com", "tls": "files"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"domain": "a.example.com", "tls": "files", "cert_path": "/nope/c.pem", "key_path": "/nope/k.pem"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, err) = as_user(
            &api,
            &session,
            "POST",
            "/api/v1/projects/blog/domains",
            body.clone(),
        )
        .await;
        assert_eq!(status, code, "{body}: {err}");
    }
    // One app per domain.
    let (status, err) = as_user(
        &api,
        &session,
        "POST",
        "/api/v1/projects/blog/domains",
        json!({"domain": "shop.example.com", "tls": "off"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert!(err["error"]["message"].as_str().unwrap().contains("shop"));

    // Tokens may read but not change domains, even with deploy scope.
    let (status, list) = api.get("/api/v1/projects/shop/domains").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["domain"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["shop.example.com", "www.shop.example.com"]);
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects/shop/domains",
            json!({"domain": "token.example.com", "tls": "off"}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = api
        .call(
            "DELETE",
            "/api/v1/projects/shop/domains/shop.example.com",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Unmapping, by a user; unknown domains are 404.
    let (status, _) = as_user(
        &api,
        &session,
        "DELETE",
        "/api/v1/projects/shop/domains/shop.example.com",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = as_user(
        &api,
        &session,
        "DELETE",
        "/api/v1/projects/shop/domains/shop.example.com",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = api.get("/api/v1/projects/shop/domains").await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    // Audited with the actor.
    let (_, events) = api.get("/api/v1/events?project=shop").await;
    let kinds: Vec<_> = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event_type"].as_str().unwrap().starts_with("Domain"))
        .map(|e| (e["event_type"].clone(), e["payload"]["actor"].clone()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (json!("DomainMapped"), json!("user:admin")),
            (json!("DomainMapped"), json!("user:admin")),
            (json!("DomainUnmapped"), json!("user:admin")),
        ]
    );
}

#[tokio::test]
async fn proxy_status_lists_routes_and_certificates() {
    let api = api().await;
    let session = login(&api).await;
    let certs = tempfile::TempDir::new().unwrap();
    let mut config = aegis_ingress::IngressConfig::new(certs.path().to_path_buf());
    config.http_port = 8080;
    config.https_port = 8443;
    api.state
        .control
        .set_ingress(aegis_ingress::Ingress::new(config));
    api.call("POST", "/api/v1/projects", json!({"name": "shop"}))
        .await;
    let (status, domain) = as_user(
        &api,
        &session,
        "POST",
        "/api/v1/projects/shop/domains",
        json!({"domain": "shop.example.com"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // No [proxy.acme]: the certificate can't be requested.
    assert_eq!(domain["cert"]["state"], "disabled");

    let (_, proxy) = api.get("/api/v1/proxy").await;
    assert_eq!(proxy["enabled"], true);
    assert_eq!(proxy["http_port"], 8080);
    assert_eq!(proxy["https_port"], 8443);
    assert_eq!(proxy["acme_enabled"], false);
    let routes = proxy["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["domain"], "shop.example.com");
    assert_eq!(routes[0]["project_name"], "shop");
    assert_eq!(routes[0]["target"], "project");
    assert!(routes[0]["upstream_port"].is_null());
}
