//! The routing table: which domain goes to which app, and which local port
//! that app's live process listens on.
//!
//! Domains point at a *project*, and each project has a set of upstream
//! ports: one per running instance of the app. Requests are spread over the
//! set round robin. A zero-downtime cutover is a single write of the
//! project's set, so every domain of the app flips at the same moment.
//!
//! Passive health: when an instance refuses a connection the proxy marks its
//! port down for [`DOWN_FOR`], and the other instances take its requests.

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// How HTTPS is provided for a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// Certificates from an ACME CA (Let's Encrypt), obtained and renewed automatically.
    Acme,
    /// Certificate and key from PEM files managed by the operator.
    Files,
    /// Plain HTTP only.
    Off,
}

impl TlsMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "acme" => Some(Self::Acme),
            "files" => Some(Self::Files),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Acme => "acme",
            Self::Files => "files",
            Self::Off => "off",
        }
    }
}

/// What a domain is served by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An app, by project id.
    Project(String),
    /// The Aegis dashboard and API.
    Dashboard,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DomainRoute {
    pub target: Target,
    pub tls: TlsMode,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
}

impl DomainRoute {
    pub fn project(project_id: impl Into<String>, tls: TlsMode) -> Self {
        Self {
            target: Target::Project(project_id.into()),
            tls,
            cert_path: None,
            key_path: None,
        }
    }
}

/// Where a request for a host should go.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// No route for this host.
    Unknown,
    /// The domain is mapped, but its app has no running process.
    NoUpstream { domain: String, route: DomainRoute },
    Upstream {
        domain: String,
        route: DomainRoute,
        port: u16,
    },
}

/// How long a port that refused a connection is skipped.
pub const DOWN_FOR: Duration = Duration::from_secs(5);

/// The instances of one app.
#[derive(Default)]
struct Pool {
    ports: Vec<u16>,
    /// Round-robin cursor.
    next: AtomicUsize,
}

#[derive(Default)]
struct Routes {
    domains: HashMap<String, DomainRoute>,
    upstreams: HashMap<String, Pool>,
    dashboard_port: Option<u16>,
}

#[derive(Default)]
pub struct RouteTable {
    routes: RwLock<Routes>,
    /// Ports that refused a connection, and when.
    down: Mutex<HashMap<u16, Instant>>,
}

/// Picks the next port of a pool, skipping ports marked down (unless all
/// are) and `avoid` (unless it is the only one).
fn pick(pool: &Pool, down: &HashMap<u16, Instant>, avoid: Option<u16>) -> Option<u16> {
    let n = pool.ports.len();
    if n == 0 {
        return None;
    }
    let start = pool.next.fetch_add(1, Ordering::Relaxed);
    let is_down = |p: u16| down.get(&p).is_some_and(|t| t.elapsed() < DOWN_FOR);
    let candidates = (0..n).map(|i| pool.ports[(start + i) % n]);
    candidates
        .clone()
        .find(|&p| Some(p) != avoid && !is_down(p))
        .or_else(|| candidates.clone().find(|&p| Some(p) != avoid))
        .or_else(|| candidates.clone().next())
}

/// The host part of a Host header or URI authority, lowercase, without
/// port or trailing dot. IP literals are returned as they are (and never
/// match a route).
pub fn host_only(host: &str) -> String {
    let host = host.trim();
    let name = if host.starts_with('[') {
        // IPv6 literal, possibly with a port.
        host.split(']').next().unwrap_or(host).to_string() + "]"
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name.to_string(),
            _ => host.to_string(),
        }
    };
    name.trim_end_matches('.').to_ascii_lowercase()
}

impl RouteTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_domain(&self, domain: &str, route: DomainRoute) {
        self.routes
            .write()
            .unwrap()
            .domains
            .insert(domain.to_string(), route);
    }

    pub fn remove_domain(&self, domain: &str) -> Option<DomainRoute> {
        self.routes.write().unwrap().domains.remove(domain)
    }

    pub fn domain(&self, domain: &str) -> Option<DomainRoute> {
        self.routes.read().unwrap().domains.get(domain).cloned()
    }

    /// Points every domain of a project at `port` (or at nothing). This is
    /// the atomic flip of a zero-downtime cutover.
    pub fn set_upstream(&self, project_id: &str, port: Option<u16>) {
        self.set_upstreams(project_id, port.into_iter().collect());
    }

    /// Points every domain of a project at a set of ports (one per
    /// instance), in one step. An empty set means "not running".
    pub fn set_upstreams(&self, project_id: &str, mut ports: Vec<u16>) {
        let mut seen = std::collections::HashSet::new();
        ports.retain(|p| seen.insert(*p));
        let mut routes = self.routes.write().unwrap();
        if ports.is_empty() {
            routes.upstreams.remove(project_id);
            return;
        }
        // Keep the cursor so a change doesn't send the next request to the
        // first instance every time.
        let next = routes
            .upstreams
            .get(project_id)
            .map_or(0, |p| p.next.load(Ordering::Relaxed));
        routes.upstreams.insert(
            project_id.to_string(),
            Pool {
                ports,
                next: AtomicUsize::new(next),
            },
        );
    }

    /// The first port of a project's set (the only one for single-instance
    /// apps).
    pub fn upstream(&self, project_id: &str) -> Option<u16> {
        self.routes
            .read()
            .unwrap()
            .upstreams
            .get(project_id)
            .and_then(|p| p.ports.first().copied())
    }

    /// All ports of a project's set.
    pub fn upstreams(&self, project_id: &str) -> Vec<u16> {
        self.routes
            .read()
            .unwrap()
            .upstreams
            .get(project_id)
            .map(|p| p.ports.clone())
            .unwrap_or_default()
    }

    /// Records that `port` refused a connection: it is skipped for
    /// [`DOWN_FOR`] while its app has other instances.
    pub fn mark_down(&self, port: u16) {
        let mut down = self.down.lock().unwrap();
        down.retain(|_, t| t.elapsed() < DOWN_FOR);
        down.insert(port, Instant::now());
    }

    /// Whether `port` is marked down right now.
    pub fn is_down(&self, port: u16) -> bool {
        self.down
            .lock()
            .unwrap()
            .get(&port)
            .is_some_and(|t| t.elapsed() < DOWN_FOR)
    }

    /// Whether `port` is (still) one of the target's upstreams.
    pub fn serves(&self, target: &Target, port: u16) -> bool {
        let routes = self.routes.read().unwrap();
        match target {
            Target::Project(id) => routes
                .upstreams
                .get(id)
                .is_some_and(|p| p.ports.contains(&port)),
            Target::Dashboard => routes.dashboard_port == Some(port),
        }
    }

    /// Picks an upstream port for the target (round robin, skipping ports
    /// marked down, and `avoid` when there is another choice).
    pub fn pick_for(&self, target: &Target, avoid: Option<u16>) -> Option<u16> {
        let routes = self.routes.read().unwrap();
        match target {
            Target::Project(id) => {
                let down = self.down.lock().unwrap();
                routes.upstreams.get(id).and_then(|p| pick(p, &down, avoid))
            }
            Target::Dashboard => routes.dashboard_port,
        }
    }

    pub fn set_dashboard_port(&self, port: Option<u16>) {
        self.routes.write().unwrap().dashboard_port = port;
    }

    /// All mapped domains, sorted by name.
    pub fn domains(&self) -> Vec<(String, DomainRoute)> {
        let mut all: Vec<_> = self
            .routes
            .read()
            .unwrap()
            .domains
            .iter()
            .map(|(d, r)| (d.clone(), r.clone()))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// The (first) upstream port of a route's target, if it has one right now.
    pub fn port_for(&self, target: &Target) -> Option<u16> {
        let routes = self.routes.read().unwrap();
        match target {
            Target::Project(id) => routes
                .upstreams
                .get(id)
                .and_then(|p| p.ports.first().copied()),
            Target::Dashboard => routes.dashboard_port,
        }
    }

    /// All upstream ports of a route's target.
    pub fn ports_for(&self, target: &Target) -> Vec<u16> {
        let routes = self.routes.read().unwrap();
        match target {
            Target::Project(id) => routes
                .upstreams
                .get(id)
                .map(|p| p.ports.clone())
                .unwrap_or_default(),
            Target::Dashboard => routes.dashboard_port.into_iter().collect(),
        }
    }

    /// Looks up the route for a Host header value. For an app with several
    /// instances, each call picks the next one.
    pub fn resolve(&self, host: &str) -> Resolution {
        let domain = host_only(host);
        let routes = self.routes.read().unwrap();
        let Some(route) = routes.domains.get(&domain) else {
            return Resolution::Unknown;
        };
        let port = match &route.target {
            Target::Project(id) => {
                let down = self.down.lock().unwrap();
                routes.upstreams.get(id).and_then(|p| pick(p, &down, None))
            }
            Target::Dashboard => routes.dashboard_port,
        };
        match port {
            Some(port) => Resolution::Upstream {
                domain,
                route: route.clone(),
                port,
            },
            None => Resolution::NoUpstream {
                domain,
                route: route.clone(),
            },
        }
    }
}

/// Counts proxied requests (and upgraded connections) per upstream port, so
/// a cutover can wait for the old process to finish what it is serving
/// before stopping it.
#[derive(Default, Clone)]
pub struct InFlight {
    counts: Arc<Mutex<HashMap<u16, Arc<AtomicUsize>>>>,
}

/// Decrements the count for its port when dropped.
pub struct InFlightGuard {
    count: Arc<AtomicUsize>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::SeqCst);
    }
}

impl InFlight {
    pub fn begin(&self, port: u16) -> InFlightGuard {
        let count = self.counts.lock().unwrap().entry(port).or_default().clone();
        count.fetch_add(1, Ordering::SeqCst);
        InFlightGuard { count }
    }

    pub fn count(&self, port: u16) -> usize {
        self.counts
            .lock()
            .unwrap()
            .get(&port)
            .map_or(0, |c| c.load(Ordering::SeqCst))
    }

    /// Waits until nothing is in flight to `port`, or `timeout` passes.
    /// Returns whether the port went idle.
    pub async fn wait_idle(&self, port: u16, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.count(port) == 0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_only_strips_ports_and_case() {
        assert_eq!(host_only("App.Example.com:8443"), "app.example.com");
        assert_eq!(host_only("example.com."), "example.com");
        assert_eq!(host_only("example.com"), "example.com");
        assert_eq!(host_only("[::1]:80"), "[::1]");
        assert_eq!(host_only("127.0.0.1:80"), "127.0.0.1");
    }

    #[test]
    fn resolves_domains_to_project_upstreams() {
        let table = RouteTable::new();
        assert_eq!(table.resolve("a.example.com"), Resolution::Unknown);

        table.set_domain("a.example.com", DomainRoute::project("p1", TlsMode::Off));
        table.set_domain(
            "www.a.example.com",
            DomainRoute::project("p1", TlsMode::Acme),
        );
        table.set_domain("b.example.com", DomainRoute::project("p2", TlsMode::Off));
        assert!(matches!(
            table.resolve("a.example.com"),
            Resolution::NoUpstream { .. }
        ));

        // One write flips every domain of the project.
        table.set_upstream("p1", Some(4001));
        for host in ["a.example.com", "WWW.A.example.com:80"] {
            match table.resolve(host) {
                Resolution::Upstream { port, .. } => assert_eq!(port, 4001),
                other => panic!("{host}: {other:?}"),
            }
        }
        table.set_upstream("p1", Some(4002));
        assert!(matches!(
            table.resolve("a.example.com"),
            Resolution::Upstream { port: 4002, .. }
        ));
        assert!(matches!(
            table.resolve("b.example.com"),
            Resolution::NoUpstream { .. }
        ));

        table.set_upstream("p1", None);
        assert!(matches!(
            table.resolve("a.example.com"),
            Resolution::NoUpstream { .. }
        ));
        assert!(table.remove_domain("a.example.com").is_some());
        assert_eq!(table.resolve("a.example.com"), Resolution::Unknown);
        assert_eq!(
            table
                .domains()
                .into_iter()
                .map(|(d, _)| d)
                .collect::<Vec<_>>(),
            vec!["b.example.com", "www.a.example.com"]
        );
    }

    fn port_of(table: &RouteTable, host: &str) -> Option<u16> {
        match table.resolve(host) {
            Resolution::Upstream { port, .. } => Some(port),
            _ => None,
        }
    }

    #[test]
    fn spreads_requests_over_instances_round_robin() {
        let table = RouteTable::new();
        table.set_domain("a.example.com", DomainRoute::project("p1", TlsMode::Off));
        table.set_upstreams("p1", vec![4001, 4002, 4003, 4002]);
        // Duplicates are dropped.
        assert_eq!(table.upstreams("p1"), vec![4001, 4002, 4003]);
        assert_eq!(table.upstream("p1"), Some(4001));
        let picks: Vec<_> = (0..6)
            .map(|_| port_of(&table, "a.example.com").unwrap())
            .collect();
        for port in [4001, 4002, 4003] {
            assert_eq!(picks.iter().filter(|p| **p == port).count(), 2, "{picks:?}");
        }
        let target = Target::Project("p1".into());
        assert!(table.serves(&target, 4003));
        assert!(!table.serves(&target, 4004));
        assert_eq!(table.ports_for(&target), vec![4001, 4002, 4003]);

        // Replacing the set is one step; removed ports get no more requests.
        table.set_upstreams("p1", vec![4002, 4004]);
        let picks: std::collections::HashSet<_> = (0..10)
            .map(|_| port_of(&table, "a.example.com").unwrap())
            .collect();
        assert_eq!(picks, [4002, 4004].into_iter().collect());
        assert!(!table.serves(&target, 4001));

        // The single-port API still works and replaces the set.
        table.set_upstream("p1", Some(4005));
        assert_eq!(table.upstreams("p1"), vec![4005]);
        table.set_upstreams("p1", vec![]);
        assert!(matches!(
            table.resolve("a.example.com"),
            Resolution::NoUpstream { .. }
        ));
    }

    #[test]
    fn skips_instances_that_refused_connections() {
        let table = RouteTable::new();
        let target = Target::Project("p1".into());
        table.set_upstreams("p1", vec![4001, 4002, 4003]);
        table.mark_down(4002);
        assert!(table.is_down(4002));
        for _ in 0..12 {
            assert_ne!(table.pick_for(&target, None), Some(4002));
        }
        // Avoiding the port that just failed picks another instance.
        for _ in 0..12 {
            assert_ne!(table.pick_for(&target, Some(4001)), Some(4001));
        }
        // Everything down: still try something rather than fail outright.
        table.mark_down(4001);
        table.mark_down(4003);
        assert!(table.pick_for(&target, None).is_some());
        assert_eq!(
            table.pick_for(&target, Some(4001)).map(|p| p != 4001),
            Some(true)
        );
        // A single instance is used even when marked down.
        table.set_upstreams("p2", vec![5001]);
        table.mark_down(5001);
        let only = Target::Project("p2".into());
        assert_eq!(table.pick_for(&only, Some(5001)), Some(5001));
        assert_eq!(table.pick_for(&Target::Project("none".into()), None), None);
    }

    #[test]
    fn dashboard_route_uses_its_own_port() {
        let table = RouteTable::new();
        table.set_domain(
            "aegis.example.com",
            DomainRoute {
                target: Target::Dashboard,
                tls: TlsMode::Acme,
                cert_path: None,
                key_path: None,
            },
        );
        assert!(matches!(
            table.resolve("aegis.example.com"),
            Resolution::NoUpstream { .. }
        ));
        table.set_dashboard_port(Some(8420));
        assert!(matches!(
            table.resolve("aegis.example.com"),
            Resolution::Upstream { port: 8420, .. }
        ));
    }

    #[test]
    fn tls_mode_round_trips() {
        for mode in [TlsMode::Acme, TlsMode::Files, TlsMode::Off] {
            assert_eq!(TlsMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(TlsMode::parse("ACME"), None);
    }

    #[tokio::test]
    async fn in_flight_counts_and_waits() {
        let inflight = InFlight::default();
        assert!(inflight.wait_idle(9, Duration::from_millis(10)).await);
        let guard = inflight.begin(9);
        let other = inflight.begin(9);
        assert_eq!(inflight.count(9), 2);
        assert!(!inflight.wait_idle(9, Duration::from_millis(50)).await);
        drop(other);
        let waiter = {
            let inflight = inflight.clone();
            tokio::spawn(async move { inflight.wait_idle(9, Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(guard);
        assert!(waiter.await.unwrap());
        assert_eq!(inflight.count(9), 0);
    }
}
