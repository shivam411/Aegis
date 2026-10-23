//! Env files on the server and `.env` imports: precedence at build time and
//! at process start, the allowlist, roles, and that values never show up in
//! API responses, events, releases or logs.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tower::ServiceExt;

/// Every value below contains this, so leaks are easy to find.
const SENTINEL: &str = "s3ntinel";

struct User {
    cookie: String,
    csrf: String,
}

async fn send(api: &Api, req: Request<Body>) -> (StatusCode, String, Value) {
    let res = aegis_web::router(api.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    assert!(
        !text.contains(SENTINEL),
        "a value leaked into a response: {text}"
    );
    (
        status,
        cookie,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(api: &Api, username: &str, password: &str) -> User {
    let (status, cookie, body) = send(
        api,
        Request::post("/api/v1/auth/login")
            .header("host", "127.0.0.1:8420")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"username": username, "password": password}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    User {
        cookie: cookie.split(';').next().unwrap().to_string(),
        csrf: body["csrf_token"].as_str().unwrap().to_string(),
    }
}

impl User {
    async fn call(&self, api: &Api, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:8420")
            .header("cookie", &self.cookie)
            .header("x-csrf-token", &self.csrf);
        let body = if method == "GET" {
            Body::empty()
        } else {
            req = req.header("content-type", "application/json");
            Body::from(body.to_string())
        };
        let (status, _, value) = send(api, req.body(body).unwrap()).await;
        (status, value)
    }
}

#[cfg(unix)]
async fn api_with_env_dirs(dirs: Vec<PathBuf>) -> Api {
    api_configured(Default::default(), move |s| s.env_file_dirs = dirs).await
}

/// Files under `dir` (recursively) that contain the sentinel.
fn files_containing_sentinel(dir: &Path, skip: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if !skip
                .iter()
                .any(|s| path.file_name().is_some_and(|n| n == *s))
                && std::fs::read(&path)
                    .map(|b| String::from_utf8_lossy(&b).contains(SENTINEL))
                    .unwrap_or(false)
            {
                found.push(path);
            }
        }
    }
    found
}

async fn assert_events_clean(api: &Api) {
    let events = api
        .state
        .control
        .list_events(None, usize::MAX)
        .await
        .unwrap();
    for e in events {
        assert!(
            !e.payload_json.contains(SENTINEL),
            "{} leaked a value: {}",
            e.event_type,
            e.payload_json
        );
    }
}

#[cfg(unix)]
fn source(overview: &Value, name: &str) -> Value {
    overview["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} missing from {overview}"))
}

#[cfg(unix)]
async fn wait_for_file(path: &Path, want: &str) -> String {
    let mut last = String::new();
    for _ in 0..100 {
        last = std::fs::read_to_string(path).unwrap_or_default();
        if last == want {
            return last;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    last
}

#[cfg(unix)]
#[tokio::test]
async fn env_file_variables_and_secrets_layer_at_build_and_start() {
    let envs = TempDir::new().unwrap();
    let env_path = envs.path().join("vault.env");
    std::fs::write(
        &env_path,
        format!(
            "# from the server\nFROM_FILE=file-{SENTINEL}-1\nLAYERED=file-{SENTINEL}\n\
             export SECRETY='file-{SENTINEL}'\nPORT=1\n"
        ),
    )
    .unwrap();
    let api = api_with_env_dirs(vec![envs.path().to_path_buf()]).await;
    let app = TempDir::new().unwrap();
    let seen = app.path().join("seen.txt");
    let port = free_port();
    std::fs::write(
        app.path().join("aegis.toml"),
        format!(
            "[build]\ninstall_command = \"\"\n\
             build_command = \"printf '%s|%s' \\\"$FROM_FILE\\\" \\\"$SECRETY\\\" > build.txt\"\n\
             start_command = \"printf '%s|%s|%s|%s' \\\"$FROM_FILE\\\" \\\"$LAYERED\\\" \\\"$SECRETY\\\" \\\"$PORT\\\" > {}; exec sleep 60\"\n\
             [deploy]\nport = {port}\nhealth_check_url = \"\"\nhealth_check_timeout_secs = 5\ndrain_timeout_secs = 1\n\
             [env]\nLAYERED = \"toml\"\n",
            seen.display()
        ),
    )
    .unwrap();
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "vault", "source_dir": app.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;

    // Env file + a Variable over [env] + a Secret.
    let (status, body) = api
        .call(
            "PUT",
            "/api/v1/projects/vault/settings",
            json!({"env": {"LAYERED": "variable"}, "env_file": env_path}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["settings"]["env_file"],
        env_path.to_string_lossy().as_ref()
    );
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/projects/vault/secrets/SECRETY",
            json!({"value": format!("secret-{SENTINEL}")}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, queued) = api
        .call("POST", "/api/v1/projects/vault/deploy", json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let d = wait_deployment(&api, queued["deployment_id"].as_str().unwrap()).await;
    assert_eq!(d["status"], "Success", "{d}");
    // At process start: Secret > Variable > [env] > env file; PORT is Aegis's.
    let want = format!("file-{SENTINEL}-1|variable|secret-{SENTINEL}|{port}");
    assert_eq!(wait_for_file(&seen, &want).await, want);
    // At build time: the env file's values, but no secrets.
    let release = api
        .state
        .control
        .resolve_project("vault")
        .unwrap()
        .current_release
        .unwrap();
    let release_dir = api
        .state
        .control
        .data_dir()
        .join("projects")
        .join(
            api.state
                .control
                .resolve_project("vault")
                .unwrap()
                .id
                .to_string(),
        )
        .join("releases")
        .join(&release);
    assert_eq!(
        std::fs::read_to_string(release_dir.join("build.txt")).unwrap(),
        format!("file-{SENTINEL}-1|file-{SENTINEL}")
    );

    // The file is read again at every start: edit it, restart.
    std::fs::write(&env_path, format!("FROM_FILE=file-{SENTINEL}-2\n")).unwrap();
    let (status, _) = api
        .call("POST", "/api/v1/processes/vault/restart", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let want = format!("file-{SENTINEL}-2|variable|secret-{SENTINEL}|{port}");
    assert_eq!(wait_for_file(&seen, &want).await, want);

    // Names and sources, never values (send() checks every response).
    std::fs::write(
        &env_path,
        format!(
            "FROM_FILE=file-{SENTINEL}-2\nLAYERED=x-{SENTINEL}\nSECRETY=y-{SENTINEL}\nPORT=1\n"
        ),
    )
    .unwrap();
    let (status, overview) = api.get("/api/v1/projects/vault/env").await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert!(!overview.to_string().contains(SENTINEL));
    assert_eq!(overview["env_file"]["readable"], true);
    assert_eq!(
        overview["env_file"]["names"],
        json!(["FROM_FILE", "LAYERED", "PORT", "SECRETY"])
    );
    let from_file = source(&overview, "FROM_FILE");
    assert_eq!(from_file["source"], "env_file");
    assert_eq!(from_file["build"], true);
    let layered = source(&overview, "LAYERED");
    assert_eq!(layered["source"], "variable");
    assert_eq!(layered["overrides"], json!(["aegis_toml", "env_file"]));
    let secret = source(&overview, "SECRETY");
    assert_eq!(secret["source"], "secret");
    assert_eq!(secret["overrides"], json!(["env_file"]));
    let port_row = source(&overview, "PORT");
    assert_eq!(port_row["source"], "aegis");
    assert_eq!(port_row["overrides"], json!(["env_file"]));
    assert_eq!(port_row["build"], false);
    // Viewers see the same names.
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/users",
            json!({"username": "vic", "password": "a-long-enough-password", "role": "viewer"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let viewer = login(&api, "vic", "a-long-enough-password").await;
    let (status, seen_by_viewer) = viewer
        .call(&api, "GET", "/api/v1/projects/vault/env", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen_by_viewer["variables"], overview["variables"]);

    // A missing file can be saved (fix it on the server afterwards), but
    // starting then fails with a clear message, and the overview says why.
    let missing = envs.path().join("missing.env");
    let (status, body) = api
        .call(
            "PUT",
            "/api/v1/projects/vault/settings",
            json!({"env": {"LAYERED": "variable"}, "env_file": missing}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = api
        .call("POST", "/api/v1/processes/vault/restart", json!({}))
        .await;
    assert!(status.is_client_error(), "{status} {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not exist"),
        "{body}"
    );
    let (_, overview) = api.get("/api/v1/projects/vault/env").await;
    assert_eq!(overview["env_file"]["readable"], false);
    assert!(overview["env_file"]["error"]
        .as_str()
        .unwrap()
        .contains("does not exist"));

    // Outside the allowed directories, or with "..": refused.
    let outside = app.path().join("outside.env");
    std::fs::write(&outside, "A=1").unwrap();
    for path in [
        outside.to_string_lossy().to_string(),
        format!("{}/../x.env", envs.path().display()),
        "relative.env".to_string(),
    ] {
        let (status, body) = api
            .call(
                "PUT",
                "/api/v1/projects/vault/settings",
                json!({"env_file": path}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
    }

    // Values never reach events, releases or logs.
    assert_events_clean(&api).await;
    let leaked = files_containing_sentinel(api.state.control.data_dir(), &["build.txt"]);
    assert!(leaked.is_empty(), "values written to {leaked:?}");
    api.state.control.shutdown().await;
}

#[tokio::test]
async fn env_files_are_off_without_allowed_directories() {
    let api = api().await;
    let app = TempDir::new().unwrap();
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "plain", "source_dir": app.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let path = app.path().join(".env");
    std::fs::write(&path, "A=1").unwrap();
    let (status, body) = api
        .call(
            "PUT",
            "/api/v1/projects/plain/settings",
            json!({"env_file": path}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("env_file_dirs"),
        "{body}"
    );
    let (status, overview) = api.get("/api/v1/projects/plain/env").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overview["env_file_dirs"], json!([]));
    assert_eq!(overview["env_file"], Value::Null);
    api.state.control.shutdown().await;
}

#[tokio::test]
async fn importing_a_dotenv_file() {
    let api = api().await;
    let app = TempDir::new().unwrap();
    let (status, _) = api
        .call(
            "POST",
            "/api/v1/projects",
            json!({"name": "imgman", "source_dir": app.path()}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let admin = login(&api, "admin", ADMIN_PASSWORD).await;
    // Existing configuration the import meets.
    let (status, _) = admin
        .call(
            &api,
            "PUT",
            "/api/v1/projects/imgman/secrets/NEXTAUTH_SECRET",
            json!({"value": format!("old-{SENTINEL}")}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = api
        .call(
            "PUT",
            "/api/v1/projects/imgman/settings",
            json!({"env": {"MONGODB_URI": "mongodb://localhost/plain"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let content = format!(
        "# img-man\nNEXTAUTH_SECRET=new-{SENTINEL}\nNEXT_PUBLIC_SITE=https://img.example.com\n\
         MONGODB_URI=mongodb+srv://app:{SENTINEL}@cluster0.example.net/img\n\
         NODE_ENV=production\n\
         GCP_PRIVATE_KEY=\"-----BEGIN PRIVATE KEY-----\\n{SENTINEL}\\n-----END PRIVATE KEY-----\\n\"\n\
         PORT=3000\n"
    );

    // Signed-in admins only: not API tokens, not viewers.
    for uri in [
        "/api/v1/projects/imgman/env/import/preview",
        "/api/v1/projects/imgman/env/import",
    ] {
        let (status, _) = api
            .call("POST", uri, json!({"content": content, "choices": {}}))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let (status, _) = admin
        .call(
            &api,
            "POST",
            "/api/v1/users",
            json!({"username": "vic", "password": "a-long-enough-password", "role": "viewer"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let viewer = login(&api, "vic", "a-long-enough-password").await;
    let (status, _) = viewer
        .call(
            &api,
            "POST",
            "/api/v1/projects/imgman/env/import/preview",
            json!({ "content": content }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The preview: names and suggestions, never values.
    let (status, preview) = admin
        .call(
            &api,
            "POST",
            "/api/v1/projects/imgman/env/import/preview",
            json!({ "content": content }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let row = |name: &str| {
        preview["variables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(row("NEXTAUTH_SECRET")["suggested"], "secret");
    assert_eq!(row("NEXTAUTH_SECRET")["exists_as"], "secret");
    assert_eq!(row("NEXT_PUBLIC_SITE")["suggested"], "variable");
    // Already a Variable, but its value here has credentials: kept as it is
    // (a Variable), and the conflict is marked.
    assert_eq!(row("MONGODB_URI")["exists_as"], "variable");
    assert_eq!(row("NODE_ENV")["suggested"], "variable");
    assert_eq!(row("GCP_PRIVATE_KEY")["suggested"], "secret");
    assert_eq!(row("GCP_PRIVATE_KEY")["multiline"], true);
    assert_eq!(row("PORT")["suggested"], "skip");

    // Syntax errors name the line, never the value.
    let (status, body) = admin
        .call(
            &api,
            "POST",
            "/api/v1/projects/imgman/env/import/preview",
            json!({"content": format!("A=1\nB=\"{SENTINEL}\n")}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("line 2"));

    // A multi-line value can't be a Variable; nothing is applied then.
    let (status, body) = admin
        .call(
            &api,
            "POST",
            "/api/v1/projects/imgman/env/import",
            json!({"content": content, "choices": {"NODE_ENV": "variable", "GCP_PRIVATE_KEY": "variable"}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (_, project) = api.get("/api/v1/projects/imgman").await;
    assert!(project["settings"]["env"].get("NODE_ENV").is_none());

    let (status, result) = admin
        .call(
            &api,
            "POST",
            "/api/v1/projects/imgman/env/import",
            json!({"content": content, "choices": {
                "NEXTAUTH_SECRET": "secret",
                "NEXT_PUBLIC_SITE": "variable",
                "MONGODB_URI": "secret",
                "NODE_ENV": "variable",
                "GCP_PRIVATE_KEY": "secret",
                "PORT": "skip",
            }}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["variables"], json!(["NEXT_PUBLIC_SITE", "NODE_ENV"]));
    assert_eq!(
        result["secrets"],
        json!(["GCP_PRIVATE_KEY", "MONGODB_URI", "NEXTAUTH_SECRET"])
    );
    assert_eq!(result["skipped"], json!(["PORT"]));
    let (_, project) = api.get("/api/v1/projects/imgman").await;
    assert_eq!(
        project["settings"]["env"],
        json!({"NEXT_PUBLIC_SITE": "https://img.example.com", "NODE_ENV": "production"}),
        "MONGODB_URI moved from Variables to Secrets"
    );
    let (_, secrets) = admin
        .call(&api, "GET", "/api/v1/projects/imgman/secrets", json!({}))
        .await;
    let names: Vec<&str> = secrets
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["GCP_PRIVATE_KEY", "MONGODB_URI", "NEXTAUTH_SECRET"]);

    // Bad choices.
    for choices in [
        json!({"MISSING": "secret"}),
        json!({"NODE_ENV": "maybe"}),
        json!({"PORT": "secret"}),
    ] {
        let (status, _) = admin
            .call(
                &api,
                "POST",
                "/api/v1/projects/imgman/env/import",
                json!({"content": content, "choices": choices}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{choices}");
    }

    assert_events_clean(&api).await;
    let leaked = files_containing_sentinel(api.state.control.data_dir(), &[]);
    assert!(leaked.is_empty(), "values written to {leaked:?}");
    api.state.control.shutdown().await;
}
