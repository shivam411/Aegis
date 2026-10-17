//! What the built-in reverse proxy (`runtime/ingress`) adds to a request:
//! the same small GET sent straight to an app and through the proxy, over
//! kept-alive HTTP/1.1 connections, one at a time and 64 at once.

use aegis_ingress::{DomainRoute, Ingress, IngressConfig, TlsMode};
use bytes::Bytes;
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

const DOMAIN: &str = "bench.example.com";
const CONNECTIONS: usize = 8;
const REQUESTS_PER_CONNECTION: usize = 8;

/// A minimal app answering every request with a small JSON body, like
/// `examples/node-app`.
async fn upstream() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let service = hyper::service::service_fn(|_req: Request<Incoming>| async {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header("content-type", "application/json")
                            .body(Full::new(Bytes::from_static(
                                br#"{"status":"ok","runtime":"bench","timestamp":"2026-10-16T00:00:00.000Z"}"#,
                            )))
                            .unwrap(),
                    )
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(tcp), service)
                    .await;
            });
        }
    });
    port
}

/// The proxy on a local port, routing `DOMAIN` to `app_port`.
async fn proxy(app_port: u16, certs: &std::path::Path) -> u16 {
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = IngressConfig::new(certs.to_path_buf());
    config.http_port = http.local_addr().unwrap().port();
    config.https_port = 0;
    let ingress = Ingress::new(config.clone());
    ingress.set_domain(DOMAIN, DomainRoute::project("bench", TlsMode::Off));
    ingress.set_upstream("bench", Some(app_port));
    tokio::spawn(ingress.serve_http(http, std::future::pending()));
    config.http_port
}

type Conn = Arc<Mutex<SendRequest<Empty<Bytes>>>>;

async fn connect(port: u16) -> Conn {
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(conn);
    Arc::new(Mutex::new(sender))
}

/// One request on a kept-alive connection. The (uncontended) lock costs the
/// same with and without the proxy.
async fn get(conn: Conn) {
    let mut sender = conn.lock().await;
    sender.ready().await.unwrap();
    let req = Request::get("/")
        .header("host", DOMAIN)
        .body(Empty::new())
        .unwrap();
    let res = sender.send_request(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(!body.is_empty());
}

fn bench_proxy(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let certs = tempfile::TempDir::new().unwrap();
    let (app, via_proxy) = rt.block_on(async {
        let app = upstream().await;
        let via_proxy = proxy(app, certs.path()).await;
        (app, via_proxy)
    });

    for (name, port) in [("direct", app), ("ingress", via_proxy)] {
        let mut group = c.benchmark_group("proxy");
        group.throughput(Throughput::Elements(1));
        let conn = rt.block_on(connect(port));
        group.bench_function(format!("{name}/sequential"), |b| {
            b.to_async(&rt).iter(|| get(conn.clone()));
        });

        let total = (CONNECTIONS * REQUESTS_PER_CONNECTION) as u64;
        group.throughput(Throughput::Elements(total));
        let conns: Vec<Conn> = rt.block_on(async {
            let mut v = Vec::new();
            for _ in 0..CONNECTIONS {
                v.push(connect(port).await);
            }
            v
        });
        group.bench_function(format!("{name}/concurrent_{total}"), |b| {
            b.to_async(&rt).iter(|| {
                let futures = conns.iter().cloned().map(|conn| async move {
                    for _ in 0..REQUESTS_PER_CONNECTION {
                        get(conn.clone()).await;
                    }
                });
                futures_util::future::join_all(futures)
            });
        });
        group.finish();
    }
}

criterion_group!(benches, bench_proxy);
criterion_main!(benches);
