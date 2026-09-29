//! Resource limits and metrics over HTTP. Limit enforcement is checked
//! where the host allows cgroups (Linux as root, or delegated); elsewhere
//! the tests check that limits are refused with a reason.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use serde_json::{json, Value};
use std::time::Duration;
use tempfile::TempDir;
use tower::ServiceExt;

/// A web app with a /hog endpoint that allocates (and touches) 200 MiB.
fn write_hog_app(dir: &std::path::Path, port: u16) {
    std::fs::write(
        dir.join("app.py"),
        r#"
import http.server, os
hog = []
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/hog":
            for _ in range(50):
                hog.append(bytearray(4 * 1024 * 1024))
        self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", int(os.environ["PORT"])), H).serve_forever()
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("aegis.toml"),
        format!(
            r#"
[build]
install_command = ""
build_command = ""
start_command = "exec python3 app.py"
[deploy]
port = {port}
health_check_url = "http://127.0.0.1:{port}/"
health_check_timeout_secs = 10
drain_timeout_secs = 1
"#
        ),
    )
    .unwrap();
}

async fn deploy(api: &Api, name: &str, src: &std::path::Path) {
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": name, "source_dir": src}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, accepted) = api
        .call(
            "POST",
            &format!("/api/v1/projects/{name}/deploy"),
            json!({}),
        )
        .await;
    let d = wait_deployment(api, accepted["deployment_id"].as_str().unwrap()).await;
    assert_eq!(d["status"], "Success", "{d}");
}

async fn wait_for(api: &Api, uri: &str, pred: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..100 {
        let (_, v) = api.get(uri).await;
        if pred(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{uri} never matched");
}

#[tokio::test]
async fn usage_history_and_live_stream() {
    let api = api().await;
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_hog_app(src.path(), port);
    deploy(&api, "web", src.path()).await;

    // Measured within a sample interval or two.
    let r = wait_for(&api, "/api/v1/projects/web/resources", |v| {
        !v["usage"].is_null()
    })
    .await;
    let usage = &r["usage"];
    assert!(
        usage["memory_bytes"].as_u64().unwrap() > 1_000_000,
        "{usage}"
    );
    assert!(usage["procs"].as_u64().unwrap() >= 1);
    assert!(usage["threads"].as_u64().unwrap() >= 1);
    assert!(usage["fds"].as_u64().unwrap() >= 3);
    assert_eq!(
        r["limits"],
        json!({"cpu_cores": null, "memory_bytes": null, "pids_max": null})
    );
    assert!(r["capabilities"]["host_cpu_cores"].as_u64().unwrap() >= 1);
    // The project list carries the latest usage too (for overview cards).
    let (_, project) = api.get("/api/v1/projects/web").await;
    assert!(project["usage"]["memory_bytes"].as_u64().unwrap() > 0);

    let m = wait_for(&api, "/api/v1/projects/web/metrics?range=5m", |v| {
        v["points"].as_array().is_some_and(|p| p.len() >= 2)
    })
    .await;
    assert_eq!(m["range_secs"], 300);
    let p = &m["points"][0];
    assert!(p["memory_bytes"].as_u64().unwrap() > 0 && p["at"].as_i64().unwrap() > 0);
    let (_, host) = api.get("/api/v1/host/metrics?range=1h&points=10").await;
    let points = host["points"].as_array().unwrap();
    assert!(!points.is_empty() && points.len() <= 11);
    assert!(points[0]["memory_limit_bytes"].as_u64().unwrap() > 0);
    for bad in ["1y", "0m", "abc"] {
        let (status, _) = api.get(&format!("/api/v1/host/metrics?range={bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _) = api.get("/api/v1/projects/nope/metrics").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Live samples over SSE, for this app only.
    let res = aegis_web::router(api.state.clone())
        .oneshot(
            Request::get("/api/v1/metrics/stream?project=web")
                .header("host", "127.0.0.1:8420")
                .header("authorization", api.bearer())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let project_id = project["id"].as_str().unwrap().to_string();
    let mut body = res.into_body();
    let text = read_sse_until(&mut body, |t| t.matches("event: sample").count() >= 2).await;
    assert!(text.contains(&project_id));
    assert!(!text.contains("\"series\":\"host\""));
    api.state.control.shutdown().await;
}

#[tokio::test]
async fn limits_are_validated_applied_live_and_audited() {
    let api = api().await;
    let (_, caps) = api.get("/api/v1/resources").await;
    let cores = caps["host_cpu_cores"].as_u64().unwrap() as f64;
    let src = TempDir::new().unwrap();
    let port = free_port();
    write_hog_app(src.path(), port);
    deploy(&api, "web", src.path()).await;
    let put = |body: Value| api.call("PUT", "/api/v1/projects/web/resources", body);

    for bad in [
        json!({"cpu_cores": 0}),
        json!({"cpu_cores": cores + 1.0}),
        json!({"colour": "red"}),
    ] {
        let (status, body) = put(bad.clone()).await;
        // Out of range is 400; on hosts without limits a CPU value is 409.
        assert!(
            status == StatusCode::BAD_REQUEST
                || (status == StatusCode::CONFLICT && caps["cpu"] == false),
            "{bad}: {status} {body}"
        );
    }
    // Clearing limits always works.
    let (status, _) = put(json!({})).await;
    assert_eq!(status, StatusCode::OK);

    if !(caps["cpu"] == true && caps["memory"] == true) {
        eprintln!(
            "limits not enforced here ({}); checking refusal only",
            caps["reason"]
        );
        let (status, body) = put(json!({"memory_bytes": 268435456})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "failed_precondition");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not available"));
        api.state.control.shutdown().await;
        return;
    }
    assert_ne!(caps["backend"], "none");

    // Below current use needs confirmation.
    wait_for(&api, "/api/v1/projects/web/resources", |v| {
        !v["usage"].is_null()
    })
    .await;
    let (status, body) = put(json!({"memory_bytes": 8 * 1024 * 1024})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "confirmation_required");
    // So does promising more memory than the host has.
    let total = caps["host_memory_bytes"].as_u64().unwrap();
    let (status, body) = put(json!({"memory_bytes": total + (1 << 30)})).await;
    assert_eq!(
        body["error"]["code"], "confirmation_required",
        "{status} {body}"
    );

    // Applied to the running app without a restart.
    let (_, before) = api.get("/api/v1/projects/web").await;
    let (status, r) =
        put(json!({"cpu_cores": 0.5, "memory_bytes": 96 * 1024 * 1024, "pids_max": 64})).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["limits"]["cpu_cores"], 0.5);
    assert_eq!(r["limits"]["memory_bytes"], 96 * 1024 * 1024);
    wait_for(&api, "/api/v1/projects/web/resources", |v| {
        v["usage"]["memory_limit_bytes"] == 96 * 1024 * 1024 && v["usage"]["cpu_limit_cores"] == 0.5
    })
    .await;
    let (_, after) = api.get("/api/v1/projects/web").await;
    assert_eq!(
        after["process"]["pid"], before["process"]["pid"],
        "no restart"
    );
    assert_eq!(after["resources"]["pids_max"], 64);
    let (_, caps) = api.get("/api/v1/resources").await;
    assert_eq!(caps["committed_memory_bytes"], 96 * 1024 * 1024);

    // Exceeding the memory limit: killed by the kernel, restarted by the
    // supervisor, and reported.
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Ok(mut s) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            let _ = s.write_all(b"GET /hog HTTP/1.0\r\n\r\n").await;
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf).await;
        }
    })
    .await;
    let events = wait_for(&api, "/api/v1/events?project=web&limit=200", |v| {
        v.as_array()
            .unwrap()
            .iter()
            .any(|e| e["event_type"] == "ResourcePressureDetected")
    })
    .await;
    let events = events.as_array().unwrap();
    let alert = events
        .iter()
        .find(|e| e["event_type"] == "ResourcePressureDetected")
        .unwrap();
    assert_eq!(alert["payload"]["kind"], "oom_kill");
    assert!(alert["payload"]["message"]
        .as_str()
        .unwrap()
        .contains("96 MiB"));
    assert!(events.iter().any(|e| e["event_type"] == "ProcessCrashed"));
    let changed = events
        .iter()
        .find(|e| {
            e["event_type"] == "ResourceLimitsChanged" && e["payload"]["limits"]["cpu_cores"] == 0.5
        })
        .unwrap();
    assert_eq!(changed["payload"]["applied_to_processes"], 1);
    assert_eq!(changed["payload"]["actor"], "token:tests");
    let p = wait_for(&api, "/api/v1/projects/web", |v| {
        v["process"]["status"] == "Running" && v["process"]["restart_count"].as_u64() >= Some(1)
    })
    .await;
    assert_eq!(
        p["resources"]["memory_bytes"],
        96 * 1024 * 1024,
        "limits survive the restart"
    );
    wait_for(&api, "/api/v1/projects/web/resources", |v| {
        v["usage"]["memory_limit_bytes"] == 96 * 1024 * 1024
    })
    .await;
    api.state.control.shutdown().await;
}

#[tokio::test]
async fn without_cgroups_metrics_come_from_proc_and_limits_are_refused() {
    let api = api_configured(aegis_web::WebSettings::default(), |s| {
        s.resources = aegis_resources::Mode::Off;
    })
    .await;
    let (_, caps) = api.get("/api/v1/resources").await;
    assert_eq!(caps["backend"], "none");
    assert_eq!(caps["cpu"], false);
    assert!(caps["reason"].as_str().unwrap().contains("turned off"));

    let src = TempDir::new().unwrap();
    let port = free_port();
    write_hog_app(src.path(), port);
    deploy(&api, "web", src.path()).await;
    let r = wait_for(&api, "/api/v1/projects/web/resources", |v| {
        !v["usage"].is_null()
    })
    .await;
    assert!(
        r["usage"]["memory_bytes"].as_u64().unwrap() > 1_000_000,
        "{r}"
    );
    assert!(r["usage"]["memory_limit_bytes"].is_null());

    let (status, body) = api
        .call(
            "PUT",
            "/api/v1/projects/web/resources",
            json!({"cpu_cores": 0.5}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "failed_precondition");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("turned off"));
    api.state.control.shutdown().await;
}
