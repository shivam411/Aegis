//! Authentication, authorization, webhooks and TLS for the HTTP API.
#![cfg(unix)]

mod common;

use aegis_web::{router, WebSettings};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tower::ServiceExt;

/// A response with headers, for tests that need more than the JSON body.
struct Full {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

async fn raw(api: &Api, req: Request<Body>) -> Full {
    let res = router(api.state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Full {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn req(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:8420")
        .header("content-type", "application/json")
}

/// Signs in and returns (cookie header value, csrf token, Set-Cookie header).
async fn login(api: &Api) -> (String, String, String) {
    let res = raw(
        api,
        req("POST", "/api/v1/auth/login")
            .body(Body::from(
                json!({"username": "admin", "password": ADMIN_PASSWORD}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let set_cookie = res.headers["set-cookie"].to_str().unwrap().to_string();
    let cookie = set_cookie.split(';').next().unwrap().to_string();
    (
        cookie,
        res.body["csrf_token"].as_str().unwrap().to_string(),
        set_cookie,
    )
}

#[tokio::test]
async fn every_api_route_requires_authentication() {
    let api = api().await;
    let routes = [
        ("GET", "/api/v1"),
        ("GET", "/api/v1/status"),
        ("GET", "/api/v1/projects"),
        ("POST", "/api/v1/projects"),
        ("GET", "/api/v1/projects/x"),
        ("POST", "/api/v1/projects/x/deploy"),
        ("POST", "/api/v1/projects/x/rollback"),
        ("PUT", "/api/v1/projects/x/schedule"),
        ("POST", "/api/v1/projects/x/webhook"),
        ("GET", "/api/v1/projects/x/releases"),
        ("GET", "/api/v1/deployments"),
        ("GET", "/api/v1/deployments/x"),
        ("GET", "/api/v1/processes"),
        ("POST", "/api/v1/processes/x/stop"),
        ("GET", "/api/v1/processes/x/logs"),
        ("GET", "/api/v1/processes/x/logs/stream"),
        ("GET", "/api/v1/events"),
        ("GET", "/api/v1/events/stream"),
        ("GET", "/api/v1/openapi.json"),
        ("GET", "/api/v1/tokens"),
        ("POST", "/api/v1/tokens"),
        ("DELETE", "/api/v1/tokens/x"),
        ("GET", "/api/v1/auth/session"),
        ("POST", "/api/v1/auth/logout"),
        ("POST", "/api/v1/auth/password"),
        ("GET", "/api/v1/does-not-exist"),
    ];
    for (method, path) in routes {
        let res = raw(&api, req(method, path).body(Body::from("{}")).unwrap()).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(
            res.body["error"]["code"], "unauthenticated",
            "{method} {path}"
        );
        assert!(res.headers.contains_key("www-authenticate"));
    }
    // Wrong credentials of either kind are also 401.
    for auth in [
        "Bearer aegis_not-a-token",
        "Bearer nonsense",
        "Basic YWRtaW46YWRtaW4=",
    ] {
        let res = raw(
            &api,
            req("GET", "/api/v1/status")
                .header("authorization", auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{auth}");
    }
    let res = raw(
        &api,
        req("GET", "/api/v1/status")
            .header("cookie", "aegis_session=forged")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    // Only the health check and the login endpoint are public.
    let res = raw(&api, req("GET", "/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(res.status, StatusCode::OK);
    let res = raw(
        &api,
        req("POST", "/api/v1/auth/login")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "login is reachable");
}

#[tokio::test]
async fn session_cookie_csrf_and_logout() {
    let api = api().await;
    let (cookie, csrf, set_cookie) = login(&api).await;
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Path=/"));
    assert!(!set_cookie.contains("Secure"), "plain HTTP on loopback");

    let session = raw(
        &api,
        req("GET", "/api/v1/auth/session")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(session.body["kind"], "user");
    assert_eq!(session.body["name"], "admin");
    assert_eq!(session.body["csrf_token"], csrf.as_str());

    let src = TempDir::new().unwrap();
    let create = |csrf: Option<&str>| {
        let mut r = req("POST", "/api/v1/projects").header("cookie", &cookie);
        if let Some(c) = csrf {
            r = r.header("x-csrf-token", c);
        }
        r.body(Body::from(
            json!({"name": "shop", "source_dir": src.path()}).to_string(),
        ))
        .unwrap()
    };
    let res = raw(&api, create(None)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(res.body["error"]["code"], "csrf");
    let res = raw(&api, create(Some("wrong"))).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = raw(&api, create(Some(&csrf))).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);

    // The action is attributed to the user in the audit log.
    let (_, events) = api.get("/api/v1/events?limit=100").await;
    let created = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_type"] == "ProjectCreated")
        .unwrap();
    assert_eq!(created["payload"]["actor"], "user:admin");
    assert!(events
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["event_type"] == "UserLoggedIn" && e["payload"]["actor"] == "user:admin"));

    let res = raw(
        &api,
        req("POST", "/api/v1/auth/logout")
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert!(res.headers["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    let res = raw(
        &api,
        req("GET", "/api/v1/auth/session")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "session ended");
}

#[tokio::test]
async fn failed_logins_are_throttled_and_recorded() {
    let api = api().await;
    let attempt = |password: &str| {
        req("POST", "/api/v1/auth/login")
            .body(Body::from(
                json!({"username": "admin", "password": password}).to_string(),
            ))
            .unwrap()
    };
    let res = raw(&api, attempt("wrong")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(!res.headers.contains_key("set-cookie"));
    for _ in 0..4 {
        raw(&api, attempt("wrong")).await;
    }
    // Locked out, even with the right password.
    let res = raw(&api, attempt(ADMIN_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res.body["error"]["code"], "rate_limited");
    assert!(res.headers.contains_key("retry-after"));

    let (_, events) = api.get("/api/v1/events?limit=100").await;
    let failures: Vec<&Value> = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event_type"] == "UserLoginFailed")
        .collect();
    assert_eq!(failures.len(), 5);
    assert!(
        !failures[0]["payload"].to_string().contains("wrong"),
        "attempted passwords are never recorded"
    );
}

#[tokio::test]
async fn https_mode_sets_secure_cookie_and_security_headers() {
    let api = api_with(WebSettings {
        https: true,
        ..Default::default()
    })
    .await;
    let (_, _, set_cookie) = login(&api).await;
    assert!(set_cookie.contains("; Secure"));

    // Every response (including errors) carries the hardening headers.
    for uri in ["/healthz", "/api/v1/status"] {
        let res = raw(&api, req("GET", uri).body(Body::empty()).unwrap()).await;
        let h = &res.headers;
        assert_eq!(h["x-frame-options"], "DENY", "{uri}");
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["referrer-policy"], "no-referrer");
        assert_eq!(h["cache-control"], "no-store");
        assert!(h["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'"));
        assert_eq!(h["strict-transport-security"], "max-age=31536000");
    }
    let plain = common::api().await;
    let res = raw(&plain, req("GET", "/healthz").body(Body::empty()).unwrap()).await;
    assert!(!res.headers.contains_key("strict-transport-security"));
}

#[tokio::test]
async fn token_scopes_and_revocation() {
    let api = api().await;
    let (cookie, csrf, _) = login(&api).await;
    let session_post = |uri: &str, body: Value| {
        req("POST", uri)
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let res = raw(
        &api,
        session_post(
            "/api/v1/tokens",
            json!({"name": "grafana", "scope": "read"}),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED);
    let read_token = res.body["token"].as_str().unwrap().to_string();
    let read_id = res.body["info"]["id"].as_str().unwrap().to_string();
    let with = |token: &str, method: &str, uri: &str| {
        req(method, uri)
            .header("authorization", format!("Bearer {}", token))
            .body(Body::from("{}"))
            .unwrap()
    };

    assert_eq!(
        raw(&api, with(&read_token, "GET", "/api/v1/projects"))
            .await
            .status,
        StatusCode::OK
    );
    let res = raw(&api, with(&read_token, "POST", "/api/v1/projects")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("read-only"));

    // Tokens can't manage tokens or passwords, whatever their scope.
    for (method, uri) in [
        ("GET", "/api/v1/tokens"),
        ("POST", "/api/v1/tokens"),
        ("POST", "/api/v1/auth/password"),
        ("POST", "/api/v1/projects/x/webhook"),
    ] {
        let res = raw(&api, with(&api.token, method, uri)).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    let res = raw(&api, with(&api.token, "GET", "/api/v1/auth/session")).await;
    assert_eq!(res.body["kind"], "token");
    assert_eq!(res.body["scope"], "deploy");
    assert!(res.body["csrf_token"].is_null());

    // Listing never shows secrets.
    let listed = raw(
        &api,
        req("GET", "/api/v1/tokens")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(!listed.body.to_string().contains(&read_token));
    assert_eq!(listed.body.as_array().unwrap().len(), 2);

    let res = raw(
        &api,
        req("DELETE", &format!("/api/v1/tokens/{read_id}"))
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert_eq!(
        raw(&api, with(&read_token, "GET", "/api/v1/projects"))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn password_change_ends_sessions() {
    let api = api().await;
    let (cookie, csrf, _) = login(&api).await;
    let change = |current: &str, new: &str| {
        req("POST", "/api/v1/auth/password")
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .body(Body::from(
                json!({"current_password": current, "new_password": new}).to_string(),
            ))
            .unwrap()
    };
    assert_eq!(
        raw(&api, change("wrong", "a-new-long-password"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        raw(&api, change(ADMIN_PASSWORD, "short")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        raw(&api, change(ADMIN_PASSWORD, "a-new-long-password"))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let res = raw(
        &api,
        req("GET", "/api/v1/auth/session")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = raw(
        &api,
        req("POST", "/api/v1/auth/login")
            .body(Body::from(
                json!({"username": "admin", "password": "a-new-long-password"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
}

#[tokio::test]
async fn allowed_hosts_extend_the_host_check() {
    let api = api_with(WebSettings {
        allowed_hosts: vec!["aegis.example.com".into()],
        ..Default::default()
    })
    .await;
    let probe = |host: &str| {
        Request::get("/healthz")
            .header("host", host)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        raw(&api, probe("aegis.example.com")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        raw(&api, probe("127.0.0.1:8420")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        raw(&api, probe("other.example.com")).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn open_streams_end_when_the_credential_is_revoked() {
    let api = api_with(WebSettings {
        stream_auth_recheck: Duration::from_millis(100),
        ..Default::default()
    })
    .await;
    let (cookie, csrf, _) = login(&api).await;
    let res = raw(
        &api,
        req("POST", "/api/v1/tokens")
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .body(Body::from(
                json!({"name": "viewer", "scope": "read"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    let token = res.body["token"].as_str().unwrap().to_string();
    let id = res.body["info"]["id"].as_str().unwrap().to_string();

    let open = |auth_header: (&'static str, String)| {
        Request::get("/api/v1/events/stream")
            .header("host", "127.0.0.1:8420")
            .header(auth_header.0, auth_header.1)
            .body(Body::empty())
            .unwrap()
    };
    let token_stream = router(api.state.clone())
        .oneshot(open(("authorization", format!("Bearer {token}"))))
        .await
        .unwrap();
    let session_stream = router(api.state.clone())
        .oneshot(open(("cookie", cookie.clone())))
        .await
        .unwrap();
    assert_eq!(token_stream.status(), StatusCode::OK);
    assert_eq!(session_stream.status(), StatusCode::OK);

    async fn ends(body: Body) -> bool {
        let mut body = body;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = body.frame().await {
                if frame.is_err() {
                    break;
                }
            }
        })
        .await
        .is_ok()
    }

    // Revoke the token: its stream ends.
    raw(
        &api,
        req("DELETE", &format!("/api/v1/tokens/{id}"))
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        ends(token_stream.into_body()).await,
        "revoked token stream stayed open"
    );

    // Change the password: the session's stream ends.
    raw(
        &api,
        req("POST", "/api/v1/auth/password")
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .body(Body::from(
                json!({"current_password": ADMIN_PASSWORD, "new_password": "another-long-password"})
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert!(
        ends(session_stream.into_body()).await,
        "ended session stream stayed open"
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {:?}: {:?}", args, out);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[tokio::test]
async fn github_webhook_verifies_signature_and_deploys_the_pushed_commit() {
    let api = api().await;
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.path().join("aegis.toml"),
        "[build]\ninstall_command = \"\"\nbuild_command = \"\"\nstart_command = \"exec sleep 60\"\n[deploy]\nhealth_check_url = \"\"\nhealth_check_timeout_secs = 5\n",
    )
    .unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "deploy me"]);
    let sha = git(repo.path(), &["rev-parse", "HEAD"]);

    let (status, project) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "hooked", "repository_url": repo.path(), "branch": "main"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = project["id"].as_str().unwrap().to_string();

    let hook = |event: &str, delivery: &str, body: &str, secret: Option<&str>| {
        let mut r = Request::post(format!("/hooks/github/{id}"))
            .header("host", "127.0.0.1:8420")
            .header("content-type", "application/json")
            .header("x-github-event", event)
            .header("x-github-delivery", delivery);
        if let Some(s) = secret {
            r = r.header(
                "x-hub-signature-256",
                aegis_webhook::sign_github_payload(s, body.as_bytes()),
            );
        }
        r.body(Body::from(body.to_string())).unwrap()
    };

    // No secret configured yet: the webhook doesn't exist.
    let res = raw(&api, hook("ping", "d0", "{}", Some("guess"))).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    let (cookie, csrf, _) = login(&api).await;
    let res = raw(
        &api,
        req("POST", "/api/v1/projects/hooked/webhook")
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["path"], format!("/hooks/github/{id}"));
    let secret = res.body["secret"].as_str().unwrap().to_string();

    let ping = raw(&api, hook("ping", "d1", "{}", Some(&secret))).await;
    assert_eq!(ping.status, StatusCode::OK);
    assert_eq!(ping.body["status"], "pong");

    let push = |branch: &str| {
        json!({"ref": format!("refs/heads/{branch}"), "after": sha, "deleted": false,
               "repository": {"clone_url": "https://evil.example/other.git"}})
        .to_string()
    };
    assert_eq!(
        raw(&api, hook("push", "d2", &push("main"), None))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        raw(
            &api,
            hook("push", "d3", &push("main"), Some("wrong-secret"))
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    let ignored = raw(&api, hook("push", "d4", &push("feature"), Some(&secret))).await;
    assert_eq!(ignored.status, StatusCode::ACCEPTED);
    assert_eq!(ignored.body["status"], "ignored");

    let queued = raw(&api, hook("push", "d5", &push("main"), Some(&secret))).await;
    assert_eq!(queued.status, StatusCode::ACCEPTED, "{}", queued.body);
    assert_eq!(queued.body["status"], "queued");
    let dep = queued.body["deployment_id"].as_str().unwrap().to_string();
    let redelivered = raw(&api, hook("push", "d5", &push("main"), Some(&secret))).await;
    assert_eq!(redelivered.body["status"], "duplicate");

    let d = wait_deployment(&api, &dep).await;
    assert_eq!(d["status"], "Success", "{d}");
    // A replay of the same signed body with a fresh delivery id doesn't redeploy.
    let replay = raw(&api, hook("push", "d6", &push("main"), Some(&secret))).await;
    assert_eq!(replay.body["status"], "ignored", "{}", replay.body);
    let (_, releases) = api.get("/api/v1/projects/hooked/releases").await;
    assert_eq!(releases[0]["commit_sha"], sha.as_str());
    assert_eq!(releases[0]["commit_message"], "deploy me");

    let (_, events) = api.get("/api/v1/events?project=hooked&limit=200").await;
    let queued_event = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_type"] == "DeploymentQueued")
        .unwrap();
    assert_eq!(queued_event["payload"]["actor"], "webhook:github");
    // The configured repository is cloned, never the payload's URL.
    assert_eq!(
        queued_event["payload"]["repository_url"],
        repo.path().to_string_lossy().as_ref()
    );
    api.state.control.shutdown().await;
}

mod tls {
    use super::*;
    use aegis_web::tls::{serve_tls, server_config};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn write_cert(dir: &Path) -> Vec<u8> {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        std::fs::write(dir.join("cert.pem"), generated.cert.pem()).unwrap();
        std::fs::write(dir.join("key.pem"), generated.key_pair.serialize_pem()).unwrap();
        generated.cert.der().to_vec()
    }

    async fn https_get(port: u16, trusted_der: &[u8]) -> Result<String, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(
                trusted_der.to_vec(),
            ))
            .unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| e.to_string())?;
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut tls = connector
            .connect(name, tcp)
            .await
            .map_err(|e| e.to_string())?;
        tls.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let _ = tls.read_to_end(&mut out).await;
        Ok(String::from_utf8_lossy(&out).to_string())
    }

    /// GET /healthz over HTTP/2 (ALPN h2), as browsers do with TLS.
    async fn h2_get(port: u16, trusted_der: &[u8]) -> u16 {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(
                trusted_der.to_vec(),
            ))
            .unwrap();
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let tls = connector.connect(name, tcp).await.unwrap();
        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
        let (mut sender, conn) = hyper::client::conn::http2::handshake(
            hyper_util::rt::TokioExecutor::new(),
            hyper_util::rt::TokioIo::new(tls),
        )
        .await
        .unwrap();
        tokio::spawn(conn);
        let req = hyper::Request::get(format!("https://localhost:{port}/healthz"))
            .body(http_body_util::Empty::<axum::body::Bytes>::new())
            .unwrap();
        sender.send_request(req).await.unwrap().status().as_u16()
    }

    #[tokio::test]
    async fn serves_http2_requests() {
        let api = api().await;
        let dir = TempDir::new().unwrap();
        let cert = write_cert(dir.path());
        let config = server_config(
            dir.path().join("cert.pem"),
            dir.path().join("key.pem"),
            Duration::ZERO,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(serve_tls(listener, api.state.clone(), config, async move {
            let _ = stop_rx.wait_for(|v| *v).await;
        }));
        assert_eq!(h2_get(port, &cert).await, 200);
        stop.send(true).unwrap();
    }

    #[tokio::test]
    async fn serves_https_and_picks_up_renewed_certificates() {
        let api = api().await;
        let dir = TempDir::new().unwrap();
        let first = write_cert(dir.path());
        let config = server_config(
            dir.path().join("cert.pem"),
            dir.path().join("key.pem"),
            Duration::ZERO,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(serve_tls(listener, api.state.clone(), config, async move {
            let _ = stop_rx.wait_for(|v| *v).await;
        }));

        let response = https_get(port, &first).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.trim_end().ends_with("ok"), "{response}");

        // Renew: new key pair and certificate in the same files.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let second = write_cert(dir.path());
        let response = https_get(port, &second).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            https_get(port, &first).await.is_err(),
            "the old certificate is no longer served"
        );

        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("serve_tls stops on shutdown")
            .unwrap()
            .unwrap();
    }

    #[test]
    fn rejects_missing_or_invalid_files() {
        let dir = TempDir::new().unwrap();
        assert!(server_config(
            dir.path().join("nope.pem"),
            dir.path().join("nope.key"),
            Duration::ZERO
        )
        .is_err());
        std::fs::write(dir.path().join("cert.pem"), "not a cert").unwrap();
        std::fs::write(dir.path().join("key.pem"), "not a key").unwrap();
        assert!(server_config(
            dir.path().join("cert.pem"),
            dir.path().join("key.pem"),
            Duration::ZERO
        )
        .is_err());
    }
}
