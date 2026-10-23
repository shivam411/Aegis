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

/// What the app is told about the original request through
/// `X-Forwarded-Proto` and `X-Forwarded-Host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forwarded {
    /// `"http"` or `"https"`.
    pub proto: &'static str,
    /// The host (with an optional port) the client asked for.
    pub host: String,
}

impl Forwarded {
    /// Works out the forwarded values for a request that reached the proxy
    /// over `proto` with `host` (from the URI or the `Host` header).
    ///
    /// When the peer is a trusted front proxy (`trusted`), the incoming
    /// `X-Forwarded-Proto` and `X-Forwarded-Host` are used if each holds
    /// exactly one well-formed value; anything else falls back to the values
    /// computed here. For any other peer the incoming headers are ignored,
    /// so clients cannot spoof them.
    pub fn from_request(
        headers: &HeaderMap,
        trusted: bool,
        proto: &'static str,
        host: &str,
    ) -> Self {
        let mut forwarded = Self {
            proto,
            host: host.to_string(),
        };
        if trusted {
            if let Some(proto) = single_value(headers, "x-forwarded-proto").and_then(parse_proto) {
                forwarded.proto = proto;
            }
            if let Some(host) = single_value(headers, "x-forwarded-host").filter(|h| valid_host(h))
            {
                forwarded.host = host.to_ascii_lowercase();
            }
        }
        forwarded
    }

    pub fn is_https(&self) -> bool {
        self.proto == "https"
    }
}

/// The value of a header that appears exactly once, as a single value (no
/// comma-separated list), trimmed.
fn single_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let value = value.to_str().ok()?.trim();
    (!value.is_empty() && !value.contains(',')).then_some(value)
}

fn parse_proto(value: &str) -> Option<&'static str> {
    if value.eq_ignore_ascii_case("https") {
        Some("https")
    } else if value.eq_ignore_ascii_case("http") {
        Some("http")
    } else {
        None
    }
}

/// A host name or IP literal with an optional port: `example.com`,
/// `example.com:8443`, `10.0.0.1:80`, `[::1]:8080`.
pub fn valid_host(value: &str) -> bool {
    if value.is_empty() || value.len() > 261 {
        return false;
    }
    let (name, port) = if let Some(rest) = value.strip_prefix('[') {
        let Some((addr, after)) = rest.split_once(']') else {
            return false;
        };
        if addr.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        match after.strip_prefix(':') {
            _ if after.is_empty() => return true,
            Some(port) => (None, Some(port)),
            None => return false,
        }
    } else {
        match value.split_once(':') {
            Some((name, port)) => (Some(name), Some(port)),
            None => (Some(value), None),
        }
    };
    if let Some(port) = port {
        let ok = !port.is_empty()
            && port.len() <= 5
            && port.bytes().all(|b| b.is_ascii_digit())
            && port.parse::<u16>().is_ok_and(|p| p != 0);
        if !ok {
            return false;
        }
    }
    let Some(name) = name else {
        return true;
    };
    name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// Prepares a client's request headers for the upstream app:
/// - hop-by-hop headers are removed (the upgrade handshake is kept for upgrades);
/// - `Host` is the host the client asked for;
/// - `X-Forwarded-For` gets the peer address appended (earlier entries are
///   whatever the client or a front proxy sent, so apps should trust only
///   the entries added by proxies they trust);
/// - `X-Forwarded-Proto` and `X-Forwarded-Host` are set from `forwarded`.
///   Incoming values are dropped; [`Forwarded::from_request`] decides whether
///   a trusted front proxy's values are kept.
pub fn prepare_request_headers(
    headers: &mut HeaderMap,
    client_ip: IpAddr,
    forwarded: &Forwarded,
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
    chain.push_str(&client_ip.to_canonical().to_string());
    headers.remove("x-forwarded-for");
    if let Ok(value) = HeaderValue::from_str(&chain) {
        headers.insert("x-forwarded-for", value);
    }
    headers.insert(
        "x-forwarded-proto",
        HeaderValue::from_static(forwarded.proto),
    );
    headers.remove("x-forwarded-host");
    if let Ok(value) = HeaderValue::from_str(&forwarded.host) {
        headers.insert("x-forwarded-host", value);
    }
    if let Ok(value) = HeaderValue::from_str(host) {
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

    /// Prepares headers as for a request from an untrusted peer.
    fn prep(
        headers: &mut HeaderMap,
        ip: IpAddr,
        proto: &'static str,
        host: &str,
        upgrade: Option<&HeaderValue>,
    ) {
        let forwarded = Forwarded::from_request(headers, false, proto, host);
        prepare_request_headers(headers, ip, &forwarded, host, upgrade);
    }

    /// Prepares headers as the proxy does, with the peer trusted or not.
    fn prep_from(headers: &mut HeaderMap, peer: &str, trusted: bool) -> Forwarded {
        let forwarded = Forwarded::from_request(headers, trusted, "http", "app.example.com");
        prepare_request_headers(
            headers,
            peer.parse().unwrap(),
            &forwarded,
            "app.example.com",
            None,
        );
        forwarded
    }

    #[test]
    fn trusted_proxy_headers_are_passed_through() {
        let mut headers = map(&[
            ("host", "app.example.com"),
            ("x-forwarded-for", "203.0.113.9"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "App.Example.com"),
        ]);
        let forwarded = prep_from(&mut headers, "127.0.0.1", true);
        assert!(forwarded.is_https());
        assert_eq!(headers["x-forwarded-proto"], "https");
        assert_eq!(headers["x-forwarded-host"], "app.example.com");
        assert_eq!(headers["x-forwarded-for"], "203.0.113.9, 127.0.0.1");
        assert_eq!(headers["host"], "app.example.com");

        // A port and upper-case scheme are fine.
        let mut headers = map(&[
            ("x-forwarded-proto", "HTTPS"),
            ("x-forwarded-host", "app.example.com:8443"),
        ]);
        prep_from(&mut headers, "::1", true);
        assert_eq!(headers["x-forwarded-proto"], "https");
        assert_eq!(headers["x-forwarded-host"], "app.example.com:8443");
        assert_eq!(headers["x-forwarded-for"], "::1");

        // Missing headers from a trusted proxy: computed values.
        let mut headers = map(&[]);
        let forwarded = prep_from(&mut headers, "127.0.0.1", true);
        assert!(!forwarded.is_https());
        assert_eq!(headers["x-forwarded-proto"], "http");
        assert_eq!(headers["x-forwarded-host"], "app.example.com");
    }

    #[test]
    fn untrusted_peers_cannot_spoof_forwarded_headers() {
        let mut headers = map(&[
            ("x-forwarded-for", "10.9.9.9"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "evil.example"),
        ]);
        let forwarded = prep_from(&mut headers, "198.51.100.7", false);
        assert!(!forwarded.is_https());
        assert_eq!(headers["x-forwarded-proto"], "http");
        assert_eq!(headers["x-forwarded-host"], "app.example.com");
        // The spoofed entry stays, but the real peer is always appended last.
        assert_eq!(headers["x-forwarded-for"], "10.9.9.9, 198.51.100.7");
    }

    #[test]
    fn invalid_values_from_a_trusted_proxy_fall_back() {
        for (proto, host) in [
            ("ftp", "evil example"),
            ("https, http", "a.example.com, b.example.com"),
            ("", ""),
            ("javascript", "a.example.com/path"),
            ("http s", "a.example.com:0"),
            ("httpss", "a.example.com:99999"),
            ("https\u{7f}", "user@a.example.com"),
            ("ws", "-bad.example.com"),
            ("http", "[::1"),
        ] {
            let Ok(proto_value) = HeaderValue::from_str(proto) else {
                continue;
            };
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-proto", proto_value);
            headers.insert("x-forwarded-host", HeaderValue::from_str(host).unwrap());
            prep_from(&mut headers, "127.0.0.1", true);
            assert_eq!(headers["x-forwarded-proto"], "http", "{proto:?}");
            assert_eq!(headers["x-forwarded-host"], "app.example.com", "{host:?}");
        }
        // The header repeated on two lines is ambiguous: fall back.
        let mut headers = map(&[
            ("x-forwarded-proto", "https"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "a.example.com"),
            ("x-forwarded-host", "b.example.com"),
        ]);
        let forwarded = prep_from(&mut headers, "127.0.0.1", true);
        assert!(!forwarded.is_https());
        assert_eq!(headers["x-forwarded-host"], "app.example.com");
        assert_eq!(headers.get_all("x-forwarded-proto").iter().count(), 1);
        assert_eq!(headers.get_all("x-forwarded-host").iter().count(), 1);
    }

    #[test]
    fn host_validation() {
        for good in [
            "example.com",
            "a.example.com:8443",
            "localhost",
            "10.0.0.1:80",
            "[::1]",
            "[::1]:8080",
            "my_host.internal",
        ] {
            assert!(valid_host(good), "{good}");
        }
        for bad in [
            "",
            "a..example.com",
            "example.com:",
            "example.com:0",
            "example.com:65536",
            "example.com:80:80",
            "[::1]x",
            "[nope]:80",
            "a b.example.com",
            "a.example.com/x",
            "-a.example.com",
            &"a".repeat(64),
        ] {
            assert!(!valid_host(bad), "{bad}");
        }
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
        prep(
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
        prep(
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
        prep(
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
