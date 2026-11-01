//! The live URL check over HTTP: settings validation and `POST /verify`.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

#[tokio::test]
async fn verify_url_settings_are_validated_and_the_manual_check_needs_a_release() {
    let api = api().await;
    let src = tempfile::TempDir::new().unwrap();
    let (status, body) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "shop", "source_dir": src.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    for bad in [
        json!({"verify_url": "ftp://example.com/"}),
        json!({"verify_url": "https://exa mple.com/"}),
        json!({"verify_url": "https://example.com/", "verify_expect": "9999"}),
        json!({"verify_url": "https://example.com/", "verify_expect": "300-200"}),
        json!({"verify_url": "https://example.com/", "verify_timeout_secs": 0}),
    ] {
        let (status, body) = api
            .call("PUT", "/api/v1/projects/shop/settings", bad.clone())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, body) = api
        .call(
            "PUT",
            "/api/v1/projects/shop/settings",
            json!({"verify_url": "https://shop.example.com/", "verify_expect": "200-399", "verify_timeout_secs": 45}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["settings"]["verify_url"], "https://shop.example.com/");
    assert_eq!(body["settings"]["verify_expect"], "200-399");

    // Nothing is live yet.
    let (status, _) = api
        .call("POST", "/api/v1/projects/shop/verify", json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = api
        .call("POST", "/api/v1/projects/ghost/verify", json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
