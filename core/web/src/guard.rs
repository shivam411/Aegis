//! Request checks that keep a browser from being used against the local API.
//!
//! The API listens on loopback only, but any web page the operator opens can
//! still send requests to `127.0.0.1`. Two attacks matter:
//!
//! - **DNS rebinding:** `evil.example` resolves to 127.0.0.1, so the attacker's
//!   page can read responses. Blocked by requiring a loopback `Host` header
//!   (or a name listed in `web.allowed_hosts`).
//! - **Cross-site requests:** a page POSTs to `http://127.0.0.1:8420/...` to
//!   trigger a deploy. Blocked by requiring `Origin` (when sent) to match the
//!   host, rejecting `Sec-Fetch-Site: cross-site`, and requiring a JSON
//!   content type on mutating requests. A JSON content type forces a CORS
//!   preflight, and this API never grants one.

use crate::{ApiError, AppState};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The hostname part of a Host header value ("[::1]:8420" -> "::1").
fn host_name(host: &str) -> &str {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let name = match host.rsplit_once(':') {
        Some((name, port)) if !name.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    name.trim_start_matches('[').trim_end_matches(']')
}

/// Loopback names, or a hostname listed in `web.allowed_hosts`.
fn is_allowed_host(host: &str, allowed: &[String]) -> bool {
    is_loopback_host(host)
        || (!host.is_empty()
            && allowed
                .iter()
                .any(|a| a.eq_ignore_ascii_case(host_name(host))))
}

/// Hostnames that always mean "this machine".
fn is_loopback_host(host: &str) -> bool {
    // A bare IPv6 address like "::1" has colons but no port.
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    let name = match host.rsplit_once(':') {
        // "[::1]:8420" or "localhost:8420"; a bare "::1" has several colons.
        Some((name, port)) if !name.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    let name = name.trim_start_matches('[').trim_end_matches(']');
    name.eq_ignore_ascii_case("localhost")
        || name
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn forbidden(message: &str) -> Response {
    ApiError::forbidden(message).into_response()
}

pub async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    // HTTP/2 carries the host as the :authority pseudo-header (in the URI),
    // not as a Host header.
    let authority = req.uri().authority().map(|a| a.as_str().to_string());
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or(authority)
        .unwrap_or_default();
    let host = host.as_str();
    if !is_allowed_host(host, &st.settings.allowed_hosts) {
        return forbidden(
            "Unknown Host; use a loopback address or a name listed in web.allowed_hosts",
        );
    }

    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin.to_str().unwrap_or("");
        let same_origin = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .is_some_and(|o| o.eq_ignore_ascii_case(host));
        if !same_origin {
            return forbidden("Cross-origin requests are not allowed");
        }
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("cross-site"))
    {
        return forbidden("Cross-site requests are not allowed");
    }

    let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if mutating {
        let json = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| {
                ct.split(';')
                    .next()
                    .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
            });
        if !json {
            return ApiError::new(
                axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "invalid_argument",
                "Requests that change state must send Content-Type: application/json (an empty object {} is fine)",
            )
            .into_response();
        }
    }

    next.run(req).await
}

/// Hardening headers on every response. The API serves only JSON and event
/// streams, so the content security policy allows nothing at all.
pub async fn security_headers(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let set = |h: &mut axum::http::HeaderMap, name: &'static str, value: &'static str| {
        h.insert(name, HeaderValue::from_static(value));
    };
    set(h, "x-content-type-options", "nosniff");
    set(h, "x-frame-options", "DENY");
    set(h, "referrer-policy", "no-referrer");
    set(
        h,
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    );
    set(h, "cache-control", "no-store");
    if st.settings.https {
        set(h, "strict-transport-security", "max-age=31536000");
    }
    res
}

#[cfg(test)]
mod tests {
    use super::{is_allowed_host, is_loopback_host};

    #[test]
    fn test_allowed_hosts() {
        let allowed = vec!["aegis.example.com".to_string()];
        assert!(is_allowed_host("aegis.example.com", &allowed));
        assert!(is_allowed_host("AEGIS.example.com:443", &allowed));
        assert!(is_allowed_host("127.0.0.1:8420", &allowed));
        assert!(!is_allowed_host("evil.example.com", &allowed));
        assert!(!is_allowed_host("aegis.example.com.evil.net", &allowed));
        assert!(!is_allowed_host("", &allowed));
    }

    #[test]
    fn test_loopback_hosts() {
        for ok in [
            "127.0.0.1:8420",
            "127.0.0.1",
            "localhost:9000",
            "LOCALHOST",
            "[::1]:8420",
            "::1",
        ] {
            assert!(is_loopback_host(ok), "{ok}");
        }
        for bad in [
            "",
            "evil.example:8420",
            "10.0.0.5:8420",
            "localhost.evil.example",
            "0.0.0.0:8420",
        ] {
            assert!(!is_loopback_host(bad), "{bad}");
        }
    }
}
