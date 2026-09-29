//! The dashboard: static files compiled into the binary. It is a client of
//! the JSON API, so these routes need no authentication and return no data.

use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

struct Asset {
    name: &'static str,
    content_type: &'static str,
    body: &'static str,
}

const ASSETS: &[Asset] = &[
    Asset {
        name: "app.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_str!("../ui/app.js"),
    },
    Asset {
        name: "style.css",
        content_type: "text/css; charset=utf-8",
        body: include_str!("../ui/style.css"),
    },
    Asset {
        name: "favicon.svg",
        content_type: "image/svg+xml",
        body: include_str!("../ui/favicon.svg"),
    },
];

pub(crate) async fn index() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../ui/index.html"),
    )
        .into_response()
}

pub(crate) async fn asset(Path(file): Path<String>) -> Response {
    match ASSETS.iter().find(|a| a.name == file) {
        Some(asset) => ([(header::CONTENT_TYPE, asset.content_type)], asset.body).into_response(),
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}
