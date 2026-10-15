//! Header handling for proxied requests and responses (RFC 9110 §7.6).

use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use std::net::IpAddr;

/// Headers that describe one connection and must not be forwarded.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

/// The protocol a client asked to switch to (WebSocket etc.), if this is an
/// HTTP/1.1 upgrade request.
pub fn upgrade_protocol(headers: &HeaderMap) -> Option<HeaderValue> {
    let wants_upgrade = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
    if !wants_upgrade {
        return None;
    }
    headers.get(header::UPGRADE).cloned()
}

/// Removes hop-by-hop headers, including any the Connection header names.
/// With `upgrade`, the upgrade handshake headers are put back.
pub fn strip_hop_by_hop(headers: &mut HeaderMap, upgrade: Option<&HeaderValue>) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|t| HeaderName::from_bytes(t.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
    if let Some(protocol) = upgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, protocol.clone());
    }
}

/// Prepares a client's request headers for the upstream app:
/// - hop-by-hop headers are removed (the upgrade handshake is kept for upgrades);
/// - `Host` is the host the client asked for;
/// - `X-Forwarded-For` gets the client address appended (earlier entries
///   are whatever the client sent, so apps should trust only the last one);
/// - `X-Forwarded-Proto` and `X-Forwarded-Host` are set (never passed through).
pub fn prepare_request_headers(
    headers: &mut HeaderMap,
    client_ip: IpAddr,
    proto: &str,
    host: &str,
    upgrade: Option<&HeaderValue>,
) {
    strip_hop_by_hop(headers, upgrade);

    let previous: Vec<String> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    let mut chain = previous.join(", ");
    if !chain.is_empty() {
        chain.push_str(", ");
    }
    chain.push_str(&client_ip.to_string());
    headers.remove("x-forwarded-for");
    if let Ok(value) = HeaderValue::from_str(&chain) {
        headers.insert("x-forwarded-for", value);
    }
    headers.insert(
        "x-forwarded-proto",
        HeaderValue::from_str(proto).unwrap_or(HeaderValue::from_static("http")),
    );
    headers.remove("x-forwarded-host");
    if let Ok(value) = HeaderValue::from_str(host) {
        headers.insert("x-forwarded-host", value.clone());
        headers.insert(header::HOST, value);
    }
}

/// Prepares an upstream response for the client. A `101 Switching
/// Protocols` response keeps its upgrade handshake headers.
pub fn prepare_response_headers(headers: &mut HeaderMap, switching_protocols: bool) {
    let upgrade = if switching_protocols {
        headers.get(header::UPGRADE).cloned()
    } else {
        None
    };
    strip_hop_by_hop(headers, upgrade.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (k, v) in pairs {
            headers.append(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn strips_hop_by_hop_and_connection_named_headers() {
        let mut headers = map(&[
            ("connection", "keep-alive, x-private"),
            ("keep-alive", "timeout=5"),
            ("x-private", "secret"),
            ("te", "trailers"),
            ("transfer-encoding", "chunked"),
            ("proxy-authorization", "Basic abc"),
            ("accept", "text/html"),
            ("cookie", "a=b"),
        ]);
        prepare_request_headers(
            &mut headers,
            "203.0.113.9".parse().unwrap(),
            "https",
            "app.example.com",
            None,
        );
        for gone in [
            "connection",
            "keep-alive",
            "x-private",
            "te",
            "transfer-encoding",
            "proxy-authorization",
        ] {
            assert!(!headers.contains_key(gone), "{gone} should be removed");
        }
        assert_eq!(headers["accept"], "text/html");
        assert_eq!(headers["cookie"], "a=b");
        assert_eq!(headers["host"], "app.example.com");
        assert_eq!(headers["x-forwarded-proto"], "https");
        assert_eq!(headers["x-forwarded-host"], "app.example.com");
        assert_eq!(headers["x-forwarded-for"], "203.0.113.9");
    }

    #[test]
    fn appends_to_forwarded_for_and_overrides_proto_and_host() {
        let mut headers = map(&[
            ("x-forwarded-for", "10.0.0.1"),
            ("x-forwarded-for", "10.0.0.2, 10.0.0.3"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "evil.example"),
            ("host", "app.example.com:8080"),
        ]);
        prepare_request_headers(
            &mut headers,
            "::1".parse().unwrap(),
            "http",
            "app.example.com:8080",
            None,
        );
        assert_eq!(
            headers["x-forwarded-for"],
            "10.0.0.1, 10.0.0.2, 10.0.0.3, ::1"
        );
        assert_eq!(headers.get_all("x-forwarded-for").iter().count(), 1);
        assert_eq!(headers["x-forwarded-proto"], "http");
        assert_eq!(headers["x-forwarded-host"], "app.example.com:8080");
        assert_eq!(headers["host"], "app.example.com:8080");
    }

    #[test]
    fn keeps_the_upgrade_handshake() {
        let mut headers = map(&[
            ("connection", "keep-alive, Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ]);
        let upgrade = upgrade_protocol(&headers);
        assert_eq!(upgrade.as_ref().unwrap(), "websocket");
        prepare_request_headers(
            &mut headers,
            "127.0.0.1".parse().unwrap(),
            "http",
            "a.example.com",
            upgrade.as_ref(),
        );
        assert_eq!(headers["connection"], "upgrade");
        assert_eq!(headers["upgrade"], "websocket");
        assert!(headers.contains_key("sec-websocket-key"));

        assert!(upgrade_protocol(&map(&[("upgrade", "websocket")])).is_none());
        assert!(upgrade_protocol(&map(&[("connection", "upgrade")])).is_none());
    }

    #[test]
    fn response_headers() {
        let mut headers = map(&[
            ("connection", "close"),
            ("keep-alive", "timeout=5"),
            ("content-type", "text/plain"),
        ]);
        prepare_response_headers(&mut headers, false);
        assert!(!headers.contains_key("connection"));
        assert!(!headers.contains_key("keep-alive"));
        assert_eq!(headers["content-type"], "text/plain");

        let mut headers = map(&[("connection", "Upgrade"), ("upgrade", "websocket")]);
        prepare_response_headers(&mut headers, true);
        assert_eq!(headers["connection"], "upgrade");
        assert_eq!(headers["upgrade"], "websocket");
        assert!(is_hop_by_hop("Transfer-Encoding"));
        assert!(!is_hop_by_hop("content-length"));
    }
}
