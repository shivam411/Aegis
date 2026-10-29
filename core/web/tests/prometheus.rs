//! Prometheus metrics, on demand: off until an admin turns them on, and
//! limited to the caller's apps.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::json;

async fn metrics(api: &Api, auth: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::get("/metrics").header("host", "127.0.0.1:8420");
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    let res = aegis_web::router(api.state.clone())
        .oneshot_req(req.body(Body::empty()).unwrap())
        .await;
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

trait OneShot {
    async fn oneshot_req(self, req: Request<Body>) -> axum::response::Response;
}

impl OneShot for axum::Router {
    async fn oneshot_req(self, req: Request<Body>) -> axum::response::Response {
        use tower::ServiceExt;
        self.oneshot(req).await.unwrap()
    }
}

#[tokio::test]
async fn metrics_are_off_until_enabled_and_need_a_token() {
    let api = api().await;
    let src = tempfile::TempDir::new().unwrap();
    api.call(
        "POST",
        "/api/v1/projects",
        json!({"name": "shop", "source_dir": src.path()}),
    )
    .await;

    // Off by default: indistinguishable from a missing endpoint, even with a token.
    let (status, _) = metrics(&api, Some(&api.bearer())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A token can't flip the switch; it is for signed-in admins.
    let (status, _) = api
        .call("PUT", "/api/v1/system/prometheus", json!({"enabled": true}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    api.state
        .auth
        .set_setting("prometheus_enabled", "1")
        .await
        .unwrap();

    let (status, _) = metrics(&api, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = metrics(&api, Some(&api.bearer())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("# TYPE aegis_app_up gauge"), "{body}");
    assert!(body.contains("aegis_app_up{app=\"shop\"} 0"), "{body}");
    assert!(body.contains("aegis_apps 1"), "{body}");
    assert!(body.contains("aegis_host_memory_total_bytes"), "{body}");
    assert!(
        body.contains("aegis_build_info{version=\"test\"} 1"),
        "{body}"
    );

    // The Grafana dashboard is valid JSON that points at these metrics.
    let (status, dash) = api.get("/api/v1/system/grafana-dashboard").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dash["uid"], "aegis-overview");
    assert!(dash.to_string().contains("aegis_app_cpu_cores"));
}
