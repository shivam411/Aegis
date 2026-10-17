//! The HTTP and HTTPS listeners and request forwarding.

use crate::headers;
use crate::routes::{InFlightGuard, Resolution, TlsMode};
use crate::Ingress;
use bytes::Bytes;
use http::header::{self, HeaderValue};
use http::{Request, Response, StatusCode, Version};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Body, Incoming};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type ProxyBody = BoxBody<Bytes, BoxError>;
pub(crate) type UpstreamClient = Client<HttpConnector, ProxyBody>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

pub(crate) fn upstream_client() -> UpstreamClient {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(CONNECT_TIMEOUT));
    connector.set_nodelay(true);
    Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_timer(TokioTimer::new())
        .build(connector)
}

fn full(text: impl Into<Bytes>) -> ProxyBody {
    Full::new(text.into())
        .map_err(|never| match never {})
        .boxed()
}

fn empty() -> ProxyBody {
    Empty::new().map_err(|never| match never {}).boxed()
}

/// A small error page. It never echoes request data.
fn page(status: StatusCode, title: &str, detail: &str) -> Response<ProxyBody> {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{status} {title}</title></head>\
         <body><h1>{title}</h1><p>{detail}</p><hr><p><small>Aegis</small></p></body></html>\n",
        status = status.as_u16()
    );
    let mut res = Response::new(full(html));
    *res.status_mut() = status;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn text(status: StatusCode, body: String) -> Response<ProxyBody> {
    let mut res = Response::new(full(body));
    *res.status_mut() = status;
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    res
}

/// The host a request is for: the URI authority (HTTP/2, absolute-form)
/// or the Host header.
fn request_host<B>(req: &Request<B>) -> Option<String> {
    req.uri()
        .authority()
        .map(|a| a.as_str().to_string())
        .or_else(|| {
            req.headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .filter(|h| !h.is_empty())
}

/// Hands an upstream response to the client. The in-flight guard lives as
/// long as the (streamed) body.
fn relay(response: Response<Incoming>, guard: InFlightGuard) -> Response<ProxyBody> {
    let (mut parts, body) = response.into_parts();
    headers::prepare_response_headers(&mut parts.headers, false);
    let body = body
        .map_err(move |e| {
            let _ = &guard;
            BoxError::from(e)
        })
        .boxed();
    Response::from_parts(parts, body)
}

pub(crate) async fn handle(
    ingress: Ingress,
    mut req: Request<Incoming>,
    peer: SocketAddr,
    https: bool,
) -> Response<ProxyBody> {
    let inner = &ingress.inner;
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());

    if !https && req.uri().path().starts_with(crate::acme::CHALLENGE_PREFIX) {
        return match inner.challenges.respond(req.uri().path()) {
            Some(key_authorization) => text(StatusCode::OK, key_authorization),
            None => page(
                StatusCode::NOT_FOUND,
                "Not found",
                "Unknown ACME challenge.",
            ),
        };
    }

    let Some(host) = request_host(&req) else {
        return page(
            StatusCode::BAD_REQUEST,
            "Bad request",
            "The request has no Host header.",
        );
    };
    let (domain, route, port) = match inner.routes.resolve(&host) {
        Resolution::Unknown => {
            return page(
                StatusCode::NOT_FOUND,
                "Unknown site",
                "No app is configured for this domain on this server.",
            )
        }
        Resolution::NoUpstream { domain, route } => (domain, route, None),
        Resolution::Upstream {
            domain,
            route,
            port,
        } => (domain, route, Some(port)),
    };

    if !https && route.tls != TlsMode::Off && inner.certs.contains(&domain) {
        let port_suffix = match inner.config.https_port {
            443 => String::new(),
            p => format!(":{}", p),
        };
        let location = format!("https://{}{}{}", domain, port_suffix, path);
        let mut res = Response::new(empty());
        *res.status_mut() = StatusCode::PERMANENT_REDIRECT;
        if let Ok(value) = HeaderValue::from_str(&location) {
            res.headers_mut().insert(header::LOCATION, value);
        }
        return res;
    }

    let Some(mut port) = port else {
        return page(
            StatusCode::BAD_GATEWAY,
            "App not running",
            "The app for this domain is not running right now.",
        );
    };

    // Count the request against its upstream, then make sure the port is
    // still in the app's set: a cutover (or a rolling step) that saw this
    // port idle must not have this request sent to the process it is about
    // to stop.
    let mut guard = inner.inflight.begin(port);
    for _ in 0..3 {
        if inner.routes.serves(&route.target, port) {
            break;
        }
        match inner.routes.pick_for(&route.target, None) {
            Some(next) => {
                port = next;
                guard = inner.inflight.begin(port);
            }
            None => break,
        }
    }
    let upgrade = if req.version() <= Version::HTTP_11 {
        headers::upgrade_protocol(req.headers())
    } else {
        None
    };
    let client_upgrade = upgrade.as_ref().map(|_| hyper::upgrade::on(&mut req));
    let (mut parts, body) = req.into_parts();
    headers::prepare_request_headers(
        &mut parts.headers,
        peer.ip(),
        if https { "https" } else { "http" },
        &host,
        upgrade.as_ref(),
    );
    parts.version = Version::HTTP_11;

    let timeout = inner.config.upstream_timeout;
    if let Some(client_upgrade) = client_upgrade {
        parts.uri = match path.parse() {
            Ok(uri) => uri,
            Err(_) => return page(StatusCode::BAD_REQUEST, "Bad request", "Invalid path."),
        };
        return match forward_upgrade(
            port,
            Request::from_parts(parts, body),
            client_upgrade,
            guard,
            timeout,
        )
        .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::debug!(port, error = %e, "Upgrade to upstream failed");
                page(
                    StatusCode::BAD_GATEWAY,
                    "Bad gateway",
                    "The app did not accept the connection.",
                )
            }
        };
    }

    let upstream_uri = |port: u16| format!("http://127.0.0.1:{}{}", port, path).parse();
    parts.uri = match upstream_uri(port) {
        Ok(uri) => uri,
        Err(_) => return page(StatusCode::BAD_REQUEST, "Bad request", "Invalid path."),
    };
    // A request without a body can be sent to another instance when the
    // first one refuses the connection (it never saw the request).
    let retry = body
        .is_end_stream()
        .then(|| (parts.method.clone(), parts.headers.clone()));
    let body: ProxyBody = if retry.is_some() {
        empty()
    } else {
        body.map_err(BoxError::from).boxed()
    };
    match tokio::time::timeout(
        timeout,
        inner.client.request(Request::from_parts(parts, body)),
    )
    .await
    {
        Ok(Ok(response)) => relay(response, guard),
        Ok(Err(e)) => {
            tracing::debug!(port, error = %e, "Upstream request failed");
            if e.is_connect() {
                inner.routes.mark_down(port);
                let other = inner
                    .routes
                    .pick_for(&route.target, Some(port))
                    .filter(|p| *p != port);
                if let (Some((method, headers)), Some(other)) = (retry, other) {
                    drop(guard);
                    let guard = inner.inflight.begin(other);
                    let mut req = Request::new(empty());
                    *req.method_mut() = method;
                    *req.headers_mut() = headers;
                    *req.version_mut() = Version::HTTP_11;
                    if let Ok(uri) = upstream_uri(other) {
                        *req.uri_mut() = uri;
                        match tokio::time::timeout(timeout, inner.client.request(req)).await {
                            Ok(Ok(response)) => return relay(response, guard),
                            Ok(Err(e)) => {
                                if e.is_connect() {
                                    inner.routes.mark_down(other);
                                }
                                tracing::debug!(port = other, error = %e, "Retry failed");
                            }
                            Err(_) => {
                                return page(
                                    StatusCode::GATEWAY_TIMEOUT,
                                    "Gateway timeout",
                                    "The app took too long to respond.",
                                )
                            }
                        }
                    }
                }
            }
            page(
                StatusCode::BAD_GATEWAY,
                "Bad gateway",
                "The app is not responding.",
            )
        }
        Err(_) => page(
            StatusCode::GATEWAY_TIMEOUT,
            "Gateway timeout",
            "The app took too long to respond.",
        ),
    }
}

/// Forwards an upgrade request (e.g. WebSocket) on its own connection and,
/// when the app switches protocols, splices the two connections together.
async fn forward_upgrade(
    port: u16,
    req: Request<Incoming>,
    client_upgrade: hyper::upgrade::OnUpgrade,
    guard: InFlightGuard,
    timeout: Duration,
) -> Result<Response<ProxyBody>, BoxError> {
    let stream = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await??;
    stream.set_nodelay(true)?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.with_upgrades().await {
            tracing::debug!(error = %e, "Upstream upgrade connection ended");
        }
    });
    let mut response = tokio::time::timeout(timeout, sender.send_request(req)).await??;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        return Ok(relay(response, guard));
    }
    let upstream_upgrade = hyper::upgrade::on(&mut response);
    tokio::spawn(async move {
        let _guard = guard;
        let (client, upstream) = tokio::join!(client_upgrade, upstream_upgrade);
        let (Ok(client), Ok(upstream)) = (client, upstream) else {
            return;
        };
        let mut client = TokioIo::new(client);
        let mut upstream = TokioIo::new(upstream);
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });
    let (mut parts, _) = response.into_parts();
    headers::prepare_response_headers(&mut parts.headers, true);
    Ok(Response::from_parts(parts, empty()))
}

impl Ingress {
    /// A rustls server config that picks certificates by SNI.
    pub fn tls_config(&self) -> Result<Arc<rustls::ServerConfig>, anyhow::Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(crate::certs::SniResolver(
                self.inner.certs.clone(),
            )));
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }

    /// Serves plain HTTP (apps, ACME challenges, redirects to HTTPS) until
    /// `shutdown` resolves.
    pub async fn serve_http(
        self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        self.serve(listener, None, shutdown).await
    }

    /// Serves HTTPS until `shutdown` resolves.
    pub async fn serve_https(
        self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let config = self.tls_config().map_err(std::io::Error::other)?;
        self.serve(listener, Some(TlsAcceptor::from(config)), shutdown)
            .await
    }

    async fn serve(
        self,
        listener: TcpListener,
        tls: Option<TlsAcceptor>,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let graceful = hyper_util::server::graceful::GracefulShutdown::new();
        tokio::pin!(shutdown);
        loop {
            let (tcp, peer) = tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!(error = %e, "Proxy failed to accept a connection");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                },
                _ = &mut shutdown => break,
            };
            let _ = tcp.set_nodelay(true);
            let ingress = self.clone();
            let tls = tls.clone();
            let watcher = graceful.watcher();
            tokio::spawn(async move {
                let https = tls.is_some();
                let service = hyper::service::service_fn(move |req| {
                    let ingress = ingress.clone();
                    async move { Ok::<_, Infallible>(handle(ingress, req, peer, https).await) }
                });
                let mut builder = Builder::new(TokioExecutor::new());
                builder
                    .http1()
                    .timer(TokioTimer::new())
                    .header_read_timeout(HEADER_READ_TIMEOUT);
                let result = match tls {
                    Some(acceptor) => {
                        let stream = match tokio::time::timeout(
                            TLS_HANDSHAKE_TIMEOUT,
                            acceptor.accept(tcp),
                        )
                        .await
                        {
                            Ok(Ok(stream)) => stream,
                            Ok(Err(e)) => {
                                tracing::debug!(error = %e, peer = %peer, "TLS handshake failed");
                                return;
                            }
                            Err(_) => return,
                        };
                        let conn =
                            builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
                        watcher.watch(conn.into_owned()).await
                    }
                    None => {
                        let conn =
                            builder.serve_connection_with_upgrades(TokioIo::new(tcp), service);
                        watcher.watch(conn.into_owned()).await
                    }
                };
                if let Err(e) = result {
                    tracing::debug!(error = %e, peer = %peer, "Proxy connection ended with an error");
                }
            });
        }
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, graceful.shutdown()).await;
        Ok(())
    }
}
