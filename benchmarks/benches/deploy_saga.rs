//! Wall time of whole deployments and rollbacks through the real deploy
//! saga (copy, build, package, start, health check, cutover, stop the old
//! release), for a tiny app: `python3 -m http.server`.
//!
//! Not a Criterion benchmark: one deployment takes seconds, mostly spent
//! waiting on the app and on the health-check poll interval, so it is timed
//! a few times and summarised (min, median, p95, max). Results are printed
//! and written to `target/bench-results/deploy_saga.json`, which
//! `scripts/bench_report.py` folds into the CI report.
//!
//! `cargo bench -p aegis-benchmarks --bench deploy_saga`
//! Environment: `AEGIS_BENCH_DEPLOYS` (default 8) deployments per scenario.
//! Under `cargo test` (no `--bench` argument) it does nothing.

#[cfg(unix)]
fn main() {
    if !std::env::args().any(|a| a == "--bench") {
        println!("deploy_saga: skipped (run it with `cargo bench`)");
        return;
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(unix::run());
}

#[cfg(not(unix))]
fn main() {
    println!("deploy_saga: skipped (Unix only)");
}

#[cfg(unix)]
mod unix {
    use aegis_benchmarks::open_store;
    use aegis_control::{
        ControlPlane, ControlSettings, DeployRequest, ProcessAction, RegisterProject,
    };
    use aegis_ingress::{Ingress, IngressConfig};
    use aegis_process::ProcessSupervisor;
    use aegis_types::{DeploymentId, ProjectId};
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const SERVE: &str = "exec python3 -m http.server $PORT --bind 127.0.0.1";

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn write_app(src: &Path, name: &str, port: u16) {
        std::fs::write(src.join("index.html"), "<h1>bench</h1>\n").unwrap();
        std::fs::write(
            src.join("aegis.toml"),
            format!(
                r#"
[project]
name = "{name}"

[build]
install_command = ""
build_command = "echo $AEGIS_RELEASE_VERSION > version.txt"
start_command = "{SERVE}"

[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/"
health_check_timeout_secs = 30
drain_timeout_secs = 5
"#
            ),
        )
        .unwrap();
    }

    async fn deploy(control: &ControlPlane, project: ProjectId, version: &str) -> Duration {
        let start = Instant::now();
        let id = control
            .queue_deployment(DeployRequest {
                project_id: project,
                version: Some(version.to_string()),
                trigger: "bench".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let status = wait(control, id).await;
        assert_eq!(status, "Success", "deployment {version} ended {status}");
        start.elapsed()
    }

    async fn wait(control: &ControlPlane, id: DeploymentId) -> String {
        for _ in 0..12_000 {
            if let Some(d) = control.get_deployment(&id) {
                if !matches!(d.status.as_str(), "Queued" | "InProgress") {
                    return d.status;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("deployment {id} did not finish within a minute");
    }

    struct Stats {
        name: String,
        samples: Vec<Duration>,
    }

    impl Stats {
        fn sorted_ms(&self) -> Vec<f64> {
            let mut v: Vec<f64> = self
                .samples
                .iter()
                .map(|d| d.as_secs_f64() * 1000.0)
                .collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v
        }

        fn percentile(sorted: &[f64], p: f64) -> f64 {
            let rank = ((sorted.len() - 1) as f64 * p).round() as usize;
            sorted[rank]
        }

        fn json(&self) -> serde_json::Value {
            let v = self.sorted_ms();
            json!({
                "name": self.name,
                "unit": "ms",
                "n": v.len(),
                "min": v[0],
                "median": Self::percentile(&v, 0.5),
                "p95": Self::percentile(&v, 0.95),
                "max": v[v.len() - 1],
            })
        }
    }

    async fn new_control(data: &Path) -> ControlPlane {
        let mut settings = ControlSettings::new(data.to_path_buf());
        // Same as the daemon's defaults (500 ms health polls etc.), but no
        // cgroups, so runs with and without root compare.
        settings.resources = aegis_resources::Mode::Off;
        let store = open_store(&data.join("aegis.db")).await;
        let control = ControlPlane::new(store, ProcessSupervisor::new(), settings)
            .await
            .unwrap();
        control.start();
        control
    }

    async fn register(control: &ControlPlane, name: &str, src: &Path) -> ProjectId {
        control
            .register_project(RegisterProject {
                name: name.into(),
                source_dir: Some(src.to_path_buf()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
    }

    pub async fn run() {
        let n: usize = std::env::var("AEGIS_BENCH_DEPLOYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8)
            .max(2);
        let data = tempfile::TempDir::new().unwrap();
        let control = new_control(data.path()).await;
        let mut results = Vec::new();

        // Without a domain: the old release stops, then the new one starts
        // on the configured port.
        let src = tempfile::TempDir::new().unwrap();
        write_app(src.path(), "direct", free_port());
        let project = register(&control, "direct", src.path()).await;
        let first = deploy(&control, project, "d0").await;
        let mut direct = Stats {
            name: "deploy_saga/direct_port".into(),
            samples: Vec::new(),
        };
        for i in 1..=n {
            direct
                .samples
                .push(deploy(&control, project, &format!("d{i}")).await);
        }
        results.push(Stats {
            name: "deploy_saga/first_deploy".into(),
            samples: vec![first],
        });
        results.push(direct);
        control
            .process_action("direct", ProcessAction::Stop)
            .await
            .unwrap();

        // Behind the built-in proxy: side-by-side start, health check,
        // switch, drain, stop.
        let certs = data.path().join("certs");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = IngressConfig::new(certs);
        config.http_port = listener.local_addr().unwrap().port();
        config.https_port = free_port();
        let ingress = Ingress::new(config);
        tokio::spawn(ingress.clone().serve_http(listener, std::future::pending()));
        control.set_ingress(ingress);
        let src = tempfile::TempDir::new().unwrap();
        write_app(src.path(), "proxied", free_port());
        let project = register(&control, "proxied", src.path()).await;
        control
            .map_domain(project, "bench.example.com", "off", None, None)
            .await
            .unwrap();
        deploy(&control, project, "p0").await;
        let mut proxied = Stats {
            name: "deploy_saga/behind_proxy".into(),
            samples: Vec::new(),
        };
        for i in 1..=n {
            proxied
                .samples
                .push(deploy(&control, project, &format!("p{i}")).await);
        }
        results.push(proxied);

        let mut rollback = Stats {
            name: "deploy_saga/rollback_behind_proxy".into(),
            samples: Vec::new(),
        };
        for _ in 0..n {
            let start = Instant::now();
            control.rollback(project, None).await.unwrap();
            rollback.samples.push(start.elapsed());
        }
        results.push(rollback);
        control.shutdown().await;

        println!(
            "{:<38} {:>4} {:>9} {:>9} {:>9} {:>9}",
            "deploy saga (ms)", "n", "min", "median", "p95", "max"
        );
        let mut out = Vec::new();
        for s in &results {
            let j = s.json();
            println!(
                "{:<38} {:>4} {:>9.0} {:>9.0} {:>9.0} {:>9.0}",
                s.name, j["n"], j["min"], j["median"], j["p95"], j["max"]
            );
            out.push(j);
        }
        let dir = results_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deploy_saga.json");
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap()).unwrap();
        println!("wrote {}", path.display());
    }

    fn results_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("AEGIS_BENCH_RESULTS") {
            return PathBuf::from(dir);
        }
        let target = std::env::var("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"));
        target.join("bench-results")
    }
}
