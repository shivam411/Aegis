//! The Claude assistant is never open to API tokens.
#![cfg(unix)]

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

#[tokio::test]
async fn the_assistant_is_never_open_to_api_tokens() {
    let api = api().await;
    let (status, _) = api.get("/api/v1/system/assistant").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for (method, path, body) in [
        ("PUT", "/api/v1/system/assistant", json!({"enabled": true})),
        ("POST", "/api/v1/system/assistant/install", json!({})),
        (
            "PUT",
            "/api/v1/system/assistant/credential",
            json!({"value": "sk-ant-x"}),
        ),
        (
            "POST",
            "/api/v1/system/assistant/chat",
            json!({"message": "hi"}),
        ),
    ] {
        let (status, body) = api.call(method, path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }
}
