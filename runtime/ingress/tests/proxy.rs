//! The proxy against real local upstreams: routing, errors, forwarded
//! headers, streaming bodies, WebSocket-style upgrades and HTTPS with
//! certificates from files.

use aegis_ingress::{DomainRoute, Ingress, IngressConfig, Target, TlsMode};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// An app that describes the request it got: `name`, host and forwarding
/// headers, method, path and body.
async fn echo_upstream(name: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let service = hyper::service::service_fn(
                    move |req: Request<Incoming>| async move {
                        let header = |n: &str| {
                            req.headers()
                                .get(n)
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("-")
                                .to_string()
                        };
                        let summary = format!(
                        "app={name}\nhost={}\nxff={}\nproto={}\nxfhost={}\nte={}\nmethod={}\npath={}\n",
                        header("host"),
                        header("x-forwarded-for"),
                        header("x-forwarded-proto"),
                        header("x-forwarded-host"),
                        header("te"),
                        req.method(),
                        req.uri()
                    );
                        if req.uri().path() == "/slow" {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                        }
                        let body = req.into_body().collect().await.unwrap().to_bytes();
                        let mut out = summary.into_bytes();
                        out.extend_from_slice(b"body=");
                        out.extend_from_slice(&body);
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(out))))
                    },
                );
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(tcp), service)
                    .await;
            });
        }
    });
    port
}

/// An app that accepts any upgrade and then echoes bytes back, uppercased.
async fn upgrade_upstream() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if tcp.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_lowercase();
                assert!(head.contains("upgrade: echo"), "{head}");
                assert!(head.contains("connection: upgrade"), "{head}");
                tcp.write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: echo\r\n\r\n",
                )
                .await
                .unwrap();
                let mut buf = [0u8; 1024];
                loop {
                    let n = tcp.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let upper: Vec<u8> = buf[..n].to_ascii_uppercase();
                    if tcp.write_all(&upper).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

struct Proxy {
    ingress: Ingress,
    http_port: u16,
    https_port: u16,
    _certs: tempfile::TempDir,
    _stop: tokio::sync::watch::Sender<bool>,
}

async fn proxy() -> Proxy {
    let certs = tempfile::TempDir::new().unwrap();
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let https = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = IngressConfig::new(certs.path().to_path_buf());
    config.http_port = http.local_addr().unwrap().port();
    config.https_port = https.local_addr().unwrap().port();
    config.upstream_timeout = Duration::from_secs(5);
    let ingress = Ingress::new(config.clone());
    let (stop, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(ingress.clone().serve_http(http, stopped(&rx)));
    tokio::spawn(ingress.clone().serve_https(https, stopped(&rx)));
    Proxy {
        ingress,
        http_port: config.http_port,
        https_port: config.https_port,
        _certs: certs,
        _stop: stop,
    }
}

fn stopped(
    rx: &tokio::sync::watch::Receiver<bool>,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let mut rx = rx.clone();
    async move {
        let _ = rx.wait_for(|v| *v).await;
    }
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

async fn exchange<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut stream: S,
    request: &[u8],
) -> Reply {
    stream.write_all(request).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut buf))
        .await
        .expect("response in time")
        .ok();
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Reply {
        status: head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        head: head.to_string(),
        body: body.to_string(),
    }
}

async fn get(port: u16, host: &str, path: &str) -> Reply {
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    exchange(
        stream,
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nTE: trailers\r\n\r\n")
            .as_bytes(),
    )
    .await
}

#[tokio::test]
async fn routes_by_host_and_reports_errors() {
    let p = proxy().await;
    let a = echo_upstream("a").await;
    let b = echo_upstream("b").await;
    p.ingress
        .set_domain("a.example.com", DomainRoute::project("pa", TlsMode::Off));
    p.ingress
        .set_domain("b.example.com", DomainRoute::project("pb", TlsMode::Off));
    p.ingress.set_upstream("pa", Some(a));
    p.ingress.set_upstream("pb", Some(b));

    let r = get(p.http_port, "a.example.com", "/x?y=1").await;
    assert_eq!(r.status, 200, "{}", r.head);
    assert!(r.body.contains("app=a"), "{}", r.body);
    assert!(r.body.contains("path=/x?y=1"), "{}", r.body);
    assert!(r.body.contains("host=a.example.com\n"), "{}", r.body);
    assert!(r.body.contains("xff=127.0.0.1\n"), "{}", r.body);
    assert!(r.body.contains("proto=http\n"), "{}", r.body);
    assert!(r.body.contains("xfhost=a.example.com\n"), "{}", r.body);
    assert!(
        r.body.contains("te=-\n"),
        "hop-by-hop TE must be dropped: {}",
        r.body
    );

    let r = get(p.http_port, "B.Example.com:80", "/").await;
    assert!(r.body.contains("app=b"), "{}", r.body);

    // Unknown host: the proxy's own 404 page.
    let r = get(p.http_port, "nope.example.com", "/").await;
    assert_eq!(r.status, 404);
    assert!(r.body.contains("No app is configured"));

    // Mapped but nothing listening: 502.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    p.ingress.set_upstream("pa", Some(dead_port));
    let r = get(p.http_port, "a.example.com", "/").await;
    assert_eq!(r.status, 502);
    // No process at all: 502 too.
    p.ingress.set_upstream("pa", None);
    let r = get(p.http_port, "a.example.com", "/").await;
    assert_eq!(r.status, 502);
    assert!(r.body.contains("not running"));

    // Flipping the upstream takes effect for the next request.
    p.ingress.set_upstream("pa", Some(b));
    assert!(get(p.http_port, "a.example.com", "/")
        .await
        .body
        .contains("app=b"));

    // Client-supplied X-Forwarded-For is kept, with the real peer appended.
    let stream = TcpStream::connect(("127.0.0.1", p.http_port))
        .await
        .unwrap();
    let r = exchange(
        stream,
        b"GET / HTTP/1.1\r\nHost: a.example.com\r\nX-Forwarded-For: 198.51.100.7\r\nX-Forwarded-Proto: https\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(
        r.body.contains("xff=198.51.100.7, 127.0.0.1\n"),
        "{}",
        r.body
    );
    assert!(r.body.contains("proto=http\n"), "{}", r.body);
}

#[tokio::test]
async fn streams_bodies_and_counts_in_flight_requests() {
    let p = proxy().await;
    let a = echo_upstream("a").await;
    p.ingress
        .set_domain("a.example.com", DomainRoute::project("pa", TlsMode::Off));
    p.ingress.set_upstream("pa", Some(a));

    // A chunked request body arrives intact.
    let chunk = "x".repeat(70_000);
    let mut request = b"POST /upload HTTP/1.1\r\nHost: a.example.com\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for _ in 0..3 {
        request.extend_from_slice(format!("{:x}\r\n{}\r\n", chunk.len(), chunk).as_bytes());
    }
    request.extend_from_slice(b"0\r\n\r\n");
    let stream = TcpStream::connect(("127.0.0.1", p.http_port))
        .await
        .unwrap();
    let r = exchange(stream, &request).await;
    assert_eq!(r.status, 200);
    assert!(r.body.contains("method=POST"));
    let body = r.body.split("body=").nth(1).unwrap();
    assert_eq!(body.len(), 3 * chunk.len());

    // A slow request is counted while it runs; wait_idle waits for it.
    let slow = tokio::spawn({
        let port = p.http_port;
        async move { get(port, "a.example.com", "/slow").await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(p.ingress.in_flight(a), 1);
    assert!(p.ingress.wait_idle(a, Duration::from_secs(5)).await);
    assert_eq!(slow.await.unwrap().status, 200);
    assert_eq!(p.ingress.in_flight(a), 0);
}

#[tokio::test]
async fn balances_over_instances_and_fails_over() {
    let p = proxy().await;
    let a = echo_upstream("a").await;
    let b = echo_upstream("b").await;
    let c = echo_upstream("c").await;
    p.ingress
        .set_domain("a.example.com", DomainRoute::project("pa", TlsMode::Off));
    p.ingress.set_upstreams("pa", vec![a, b, c]);
    assert_eq!(p.ingress.upstreams("pa"), vec![a, b, c]);

    // Round robin: every instance gets its share.
    let mut counts = std::collections::HashMap::new();
    for _ in 0..9 {
        let r = get(p.http_port, "a.example.com", "/").await;
        assert_eq!(r.status, 200);
        let app = r.body.lines().next().unwrap().to_string();
        *counts.entry(app).or_insert(0) += 1;
    }
    assert_eq!(counts.len(), 3, "{counts:?}");
    assert!(counts.values().all(|n| *n == 3), "{counts:?}");

    // An instance that refuses connections: requests without a body are
    // retried on another instance, and the dead one is skipped from then on.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    p.ingress.set_upstreams("pa", vec![a, dead_port, b]);
    for _ in 0..12 {
        let r = get(p.http_port, "a.example.com", "/").await;
        assert_eq!(r.status, 200, "{}", r.head);
        assert!(!r.body.contains("app=c"));
    }
    assert!(p.ingress.routes().is_down(dead_port));
    // Requests with a body are not replayed, but avoid the dead instance.
    for _ in 0..6 {
        let stream = TcpStream::connect(("127.0.0.1", p.http_port))
            .await
            .unwrap();
        let r = exchange(
            stream,
            b"POST /x HTTP/1.1\r\nHost: a.example.com\r\nConnection: close\r\nContent-Length: 2\r\n\r\nhi",
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.head);
        assert!(r.body.ends_with("body=hi"), "{}", r.body);
    }
    // Only dead instances left: a clear 502.
    p.ingress.set_upstreams("pa", vec![dead_port]);
    assert_eq!(get(p.http_port, "a.example.com", "/").await.status, 502);
    // The status API lists every instance's port.
    p.ingress.set_upstreams("pa", vec![a, b]);
    let status = p.ingress.status();
    let route = status
        .routes
        .iter()
        .find(|r| r.domain == "a.example.com")
        .unwrap();
    assert_eq!(route.upstream_ports, vec![a, b]);
    assert_eq!(route.upstream_port, Some(a));
}

#[tokio::test]
async fn upgrades_are_spliced_through() {
    let p = proxy().await;
    let up = upgrade_upstream().await;
    p.ingress
        .set_domain("ws.example.com", DomainRoute::project("pw", TlsMode::Off));
    p.ingress.set_upstream("pw", Some(up));

    let mut stream = TcpStream::connect(("127.0.0.1", p.http_port))
        .await
        .unwrap();
    stream
        .write_all(b"GET /socket HTTP/1.1\r\nHost: ws.example.com\r\nConnection: Upgrade\r\nUpgrade: echo\r\n\r\n")
        .await
        .unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_lowercase();
    assert!(head.starts_with("http/1.1 101"), "{head}");
    assert!(head.contains("upgrade: echo"), "{head}");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        p.ingress.in_flight(up),
        1,
        "an open upgrade counts as in flight"
    );

    for msg in ["hello", "zero downtime"] {
        stream.write_all(msg.as_bytes()).await.unwrap();
        let mut buf = vec![0u8; msg.len()];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, msg.to_uppercase().as_bytes());
    }
    drop(stream);
    assert!(p.ingress.wait_idle(up, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn acme_challenges_are_answered_on_http() {
    let p = proxy().await;
    p.ingress.challenges().insert("tok-1", "tok-1.thumb");
    // Any host, even unmapped ones, before routing.
    let r = get(
        p.http_port,
        "new.example.com",
        "/.well-known/acme-challenge/tok-1",
    )
    .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "tok-1.thumb");
    let r = get(
        p.http_port,
        "new.example.com",
        "/.well-known/acme-challenge/tok-2",
    )
    .await;
    assert_eq!(r.status, 404);
}

#[tokio::test]
async fn https_with_certificate_files_and_redirect() {
    let p = proxy().await;
    let a = echo_upstream("secure").await;
    let dir = tempfile::TempDir::new().unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["secure.example.com".to_string()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let (cert_path, key_path) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key.serialize_pem()).unwrap();

    p.ingress.set_domain(
        "secure.example.com",
        DomainRoute {
            target: Target::Project("ps".into()),
            tls: TlsMode::Files,
            cert_path: Some(cert_path.clone()),
            key_path: Some(key_path.clone()),
        },
    );
    p.ingress.set_upstream("ps", Some(a));
    let status = p.ingress.cert_status("secure.example.com");
    assert_eq!(status.state, "valid", "{status:?}");
    assert!(status.not_after.is_some());

    // Plain HTTP now redirects to HTTPS on the configured port.
    let r = get(p.http_port, "secure.example.com", "/page?q=1").await;
    assert_eq!(r.status, 308, "{}", r.head);
    assert!(
        r.head.contains(&format!(
            "location: https://secure.example.com:{}/page?q=1",
            p.https_port
        )) || r.head.contains(&format!(
            "Location: https://secure.example.com:{}/page?q=1",
            p.https_port
        )),
        "{}",
        r.head
    );

    // HTTPS with SNI serves the app with X-Forwarded-Proto: https.
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tcp = TcpStream::connect(("127.0.0.1", p.https_port))
        .await
        .unwrap();
    let tls = connector
        .connect("secure.example.com".try_into().unwrap(), tcp)
        .await
        .unwrap();
    let r = exchange(
        tls,
        b"GET /hello HTTP/1.1\r\nHost: secure.example.com\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.head);
    assert!(r.body.contains("app=secure"));
    assert!(r.body.contains("proto=https\n"), "{}", r.body);

    // A name without a certificate fails the handshake.
    let tcp = TcpStream::connect(("127.0.0.1", p.https_port))
        .await
        .unwrap();
    assert!(connector
        .connect("other.example.com".try_into().unwrap(), tcp)
        .await
        .is_err());

    // Replacing the files is picked up by maintenance.
    let key2 = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["secure.example.com".to_string()]).unwrap();
    params.not_after = rcgen::date_time_ymd(2040, 1, 2);
    let cert2 = params.self_signed(&key2).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(&key_path, key2.serialize_pem()).unwrap();
    std::fs::write(&cert_path, cert2.pem()).unwrap();
    // Make sure the modification time changes even on coarse filesystems.
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&cert_path)
        .unwrap()
        .set_modified(later)
        .unwrap();
    p.ingress.maintain().await;
    let status = p.ingress.cert_status("secure.example.com");
    assert!(
        status
            .not_after
            .as_deref()
            .unwrap_or("")
            .starts_with("2040-01-02"),
        "{status:?}"
    );

    // Broken files are reported, and the domain keeps no certificate.
    p.ingress.set_domain(
        "broken.example.com",
        DomainRoute {
            target: Target::Project("ps".into()),
            tls: TlsMode::Files,
            cert_path: Some(dir.path().join("missing.pem")),
            key_path: Some(key_path),
        },
    );
    let status = p.ingress.cert_status("broken.example.com");
    assert_eq!(status.state, "error");
    assert!(status.message.unwrap().contains("missing.pem"));
    // ...so plain HTTP is served rather than redirecting to a broken HTTPS.
    assert_eq!(
        get(p.http_port, "broken.example.com", "/").await.status,
        200
    );
}

#[tokio::test]
async fn acme_domains_without_acme_are_reported() {
    let p = proxy().await;
    p.ingress
        .set_domain("auto.example.com", DomainRoute::project("p", TlsMode::Acme));
    let status = p.ingress.cert_status("auto.example.com");
    assert_eq!(status.state, "disabled");
    let off = {
        p.ingress
            .set_domain("plain.example.com", DomainRoute::project("p", TlsMode::Off));
        p.ingress.cert_status("plain.example.com")
    };
    assert_eq!(off.state, "off");
    let all = p.ingress.status();
    assert!(!all.acme_enabled);
    assert_eq!(all.routes.len(), 2);
    assert_eq!(all.routes[0].domain, "auto.example.com");
    assert_eq!(all.routes[0].project_id.as_deref(), Some("p"));
    // Nothing is due without ACME settings, and maintenance makes no requests.
    assert!(p.ingress.maintain().await.is_empty());
}
