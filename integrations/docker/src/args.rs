//! Command lines for the `docker` CLI. Pure functions, so what Aegis asks
//! Docker to do can be checked without a Docker daemon.
//!
//! App environment variables (including secrets) never appear here: they go
//! to the container through an `--env-file` (see [`env_file_contents`]).

use aegis_config::ResourceLimits;
use aegis_types::{ProcessId, ProjectId};
use std::collections::BTreeMap;
use std::path::Path;

/// Labels on everything Aegis creates, so it can find (and only touch) its own.
pub const LABEL_MANAGED: &str = "aegis.managed";
/// Which Aegis installation (data directory) created a container.
pub const LABEL_INSTANCE: &str = "aegis.instance";
pub const LABEL_PROJECT: &str = "aegis.project";
pub const LABEL_PROCESS: &str = "aegis.process";
pub const LABEL_RELEASE: &str = "aegis.release";

/// `aegis/<project id>`: the repository holding an app's release images.
pub fn image_repository(project_id: &ProjectId) -> String {
    format!("aegis/{}", project_id)
}

/// `aegis/<project id>:<release>`: the image a release runs. Rollbacks
/// reuse it, so a release always runs exactly what was built for it.
pub fn image_tag(project_id: &ProjectId, version: &str) -> String {
    format!(
        "{}:{}",
        image_repository(project_id),
        tag_for_version(version)
    )
}

/// A Docker tag for a release version. Release versions allow characters
/// tags don't ('+', a leading '-' or '.'); those are replaced and a short
/// hash keeps different versions apart.
pub fn tag_for_version(version: &str) -> String {
    let mut tag: String = version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !tag.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        tag.insert(0, 'v');
    }
    if tag != version {
        // FNV-1a: stable across runs and platforms.
        let hash = version.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
        });
        tag = format!("{}-{:08x}", tag, hash as u32);
    }
    tag.truncate(128);
    tag
}

/// One container per supervised process: `aegis-<process id>`.
pub fn container_name(process_id: &ProcessId) -> String {
    format!("aegis-{}", process_id)
}

/// The Docker volume behind an app's `[run] volumes` name. Scoped to the
/// app, so one app can never mount another's data.
pub fn volume_name(project_id: &ProjectId, name: &str) -> String {
    format!("aegis-{}-{}", project_id, name)
}

/// A named volume mounted into the container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub volume: String,
    pub target: String,
    pub read_only: bool,
}

/// Everything `docker create` needs, except the environment.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerPlan {
    pub name: String,
    pub image: String,
    pub labels: Vec<(String, String)>,
    /// (host port on 127.0.0.1, container port).
    pub publish: Option<(u16, u16)>,
    pub limits: ResourceLimits,
    pub init: bool,
    pub user: Option<String>,
    pub read_only: bool,
    pub tmpfs: Vec<String>,
    pub mounts: Vec<Mount>,
    /// Replaces the image's command when not empty.
    pub command: Vec<String>,
}

/// CPU cores as Docker's `--cpus` value: "0.5", "2".
pub fn format_cpus(cores: f64) -> String {
    let text = format!("{:.3}", cores);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() {
        "0".into()
    } else {
        text.into()
    }
}

/// Limits as `docker create` flags. Unset limits are left out (unlimited).
/// Memory gets no swap on top, like process apps.
pub fn limit_run_args(limits: &ResourceLimits) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(cores) = limits.cpu_cores {
        args.extend(["--cpus".to_string(), format_cpus(cores)]);
    }
    if let Some(bytes) = limits.memory_bytes {
        args.extend([
            "--memory".to_string(),
            bytes.to_string(),
            "--memory-swap".to_string(),
            bytes.to_string(),
        ]);
    }
    if let Some(pids) = limits.pids_max {
        args.extend(["--pids-limit".to_string(), pids.to_string()]);
    }
    args
}

/// Limits as `docker update` flags for a running container. `docker update`
/// can't clear a CPU or memory limit, so "unlimited" becomes the host's
/// size (which is the same thing); the container is created without the
/// limit on its next start.
pub fn limit_update_args(
    limits: &ResourceLimits,
    host_cpus: usize,
    host_memory_bytes: u64,
) -> Vec<String> {
    let mut args = vec!["update".to_string()];
    let cpus = limits
        .cpu_cores
        .map(format_cpus)
        .unwrap_or_else(|| host_cpus.max(1).to_string());
    args.extend(["--cpus".to_string(), cpus]);
    match limits.memory_bytes {
        Some(bytes) => args.extend([
            "--memory".to_string(),
            bytes.to_string(),
            "--memory-swap".to_string(),
            bytes.to_string(),
        ]),
        None if host_memory_bytes > 0 => args.extend([
            "--memory".to_string(),
            host_memory_bytes.to_string(),
            "--memory-swap".to_string(),
            "-1".to_string(),
        ]),
        None => {}
    }
    let pids = limits
        .pids_max
        .map_or_else(|| "-1".to_string(), |n| n.to_string());
    args.extend(["--pids-limit".to_string(), pids]);
    args
}

/// `docker create` arguments. The port is only published on loopback: the
/// app is reached through Aegis's proxy (or a local reverse proxy), never
/// directly from the network.
pub fn create_args(plan: &ContainerPlan, env_file: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "create".into(),
        "--name".into(),
        plan.name.clone(),
        // Never fetch an image here: a release runs the image built (or
        // pulled) for it during the deployment.
        "--pull".into(),
        "never".into(),
        "--env-file".into(),
        env_file.to_string_lossy().into_owned(),
        // Aegis restarts containers itself (restart policy and backoff).
        "--restart".into(),
        "no".into(),
        // Output is streamed into Aegis's log files through the attached
        // `docker start`; Docker doesn't need to keep a second copy.
        "--log-driver".into(),
        "none".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
    ];
    for (key, value) in &plan.labels {
        args.extend(["--label".into(), format!("{}={}", key, value)]);
    }
    if let Some((host, container)) = plan.publish {
        args.extend([
            "--publish".into(),
            format!("127.0.0.1:{}:{}", host, container),
        ]);
    }
    if plan.init {
        args.push("--init".into());
    }
    if let Some(user) = &plan.user {
        args.extend(["--user".into(), user.clone()]);
    }
    if plan.read_only {
        args.push("--read-only".into());
    }
    for path in &plan.tmpfs {
        args.extend(["--tmpfs".into(), path.clone()]);
    }
    for mount in &plan.mounts {
        let mut spec = format!(
            "type=volume,source={},target={}",
            mount.volume, mount.target
        );
        if mount.read_only {
            spec.push_str(",readonly");
        }
        args.extend(["--mount".into(), spec]);
    }
    args.extend(limit_run_args(&plan.limits));
    args.push(plan.image.clone());
    args.extend(plan.command.iter().cloned());
    args
}

/// `docker build` arguments: `dockerfile` is relative to `context` (the
/// release directory).
pub fn build_args(
    tag: &str,
    context: &Path,
    dockerfile: &str,
    labels: &[(String, String)],
) -> Vec<String> {
    let mut args = vec![
        "build".to_string(),
        "--file".to_string(),
        context.join(dockerfile).to_string_lossy().into_owned(),
        "--tag".to_string(),
        tag.to_string(),
    ];
    for (key, value) in labels {
        args.extend(["--label".to_string(), format!("{}={}", key, value)]);
    }
    args.push(context.to_string_lossy().into_owned());
    args
}

/// The env file for `docker create --env-file`: `NAME=value` lines. Docker
/// reads values verbatim but can't represent line breaks, so those are
/// refused rather than silently cut.
pub fn env_file_contents(env: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    for (name, value) in env {
        let name_ok = !name.is_empty()
            && !name.starts_with('#')
            && !name
                .chars()
                .any(|c| c == '=' || c.is_whitespace() || c.is_control());
        if !name_ok {
            return Err(format!(
                "'{}' can't be passed to a container as an environment variable name",
                name
            ));
        }
        if value.contains(['\n', '\r', '\0']) {
            return Err(format!(
                "{} contains a line break, which Docker can't pass to a container; store it on one line (for example base64-encoded)",
                name
            ));
        }
        out.push_str(name);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> ContainerPlan {
        ContainerPlan {
            name: "aegis-p1".into(),
            image: "aegis/proj:r1".into(),
            labels: vec![(LABEL_MANAGED.into(), "true".into())],
            publish: Some((41000, 8080)),
            limits: ResourceLimits::default(),
            init: true,
            user: None,
            read_only: false,
            tmpfs: vec![],
            mounts: vec![],
            command: vec![],
        }
    }

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn create_args_publish_on_loopback_and_never_carry_env_values() {
        let env_file = Path::new("/var/lib/aegis/run/docker/aegis-p1.env");
        let args = create_args(&plan(), env_file);
        assert_eq!(args[0], "create");
        assert!(has_pair(&args, "--publish", "127.0.0.1:41000:8080"));
        assert!(has_pair(&args, "--env-file", &env_file.to_string_lossy()));
        assert!(has_pair(&args, "--pull", "never"));
        assert!(has_pair(&args, "--restart", "no"));
        assert!(has_pair(&args, "--security-opt", "no-new-privileges"));
        assert!(args.contains(&"--init".to_string()));
        assert_eq!(args.last().unwrap(), "aegis/proj:r1");
        for forbidden in [
            "--privileged",
            "--network",
            "--net",
            "--cap-add",
            "--device",
            "-v",
            "--volume",
            "-e",
            "--env",
            "--pid",
        ] {
            assert!(!args.iter().any(|a| a == forbidden), "{forbidden}");
        }
        // No port: nothing is published.
        let mut p = plan();
        p.publish = None;
        assert!(!create_args(&p, env_file).iter().any(|a| a == "--publish"));
    }

    #[test]
    fn secrets_only_reach_the_env_file() {
        let env = BTreeMap::from([
            ("API_TOKEN".to_string(), "s3cr3t-value".to_string()),
            ("PORT".to_string(), "8080".to_string()),
            ("GREETING".to_string(), "hello = world  ".to_string()),
        ]);
        let file = env_file_contents(&env).unwrap();
        assert_eq!(
            file,
            "API_TOKEN=s3cr3t-value\nGREETING=hello = world  \nPORT=8080\n"
        );
        let args = create_args(&plan(), Path::new("/run/x.env"));
        let joined = args.join(" ");
        assert!(!joined.contains("s3cr3t-value"));
        assert!(!joined.contains("API_TOKEN"));
        let update = limit_update_args(&ResourceLimits::default(), 4, 1 << 30).join(" ");
        assert!(!update.contains("s3cr3t"));

        let multi = BTreeMap::from([("CERT".to_string(), "a\nb".to_string())]);
        assert!(env_file_contents(&multi)
            .unwrap_err()
            .contains("line break"));
        let bad_name = BTreeMap::from([("A B".to_string(), "x".to_string())]);
        assert!(env_file_contents(&bad_name).is_err());
    }

    #[test]
    fn options_map_to_safe_flags() {
        let mut p = plan();
        p.init = false;
        p.user = Some("1000:1000".into());
        p.read_only = true;
        p.tmpfs = vec!["/tmp".into()];
        p.mounts = vec![
            Mount {
                volume: "aegis-proj-data".into(),
                target: "/data".into(),
                read_only: false,
            },
            Mount {
                volume: "aegis-proj-cfg".into(),
                target: "/cfg".into(),
                read_only: true,
            },
        ];
        p.command = vec!["node".into(), "server.js".into()];
        let args = create_args(&p, Path::new("/run/x.env"));
        assert!(!args.contains(&"--init".to_string()));
        assert!(has_pair(&args, "--user", "1000:1000"));
        assert!(args.contains(&"--read-only".to_string()));
        assert!(has_pair(&args, "--tmpfs", "/tmp"));
        assert!(has_pair(
            &args,
            "--mount",
            "type=volume,source=aegis-proj-data,target=/data"
        ));
        assert!(has_pair(
            &args,
            "--mount",
            "type=volume,source=aegis-proj-cfg,target=/cfg,readonly"
        ));
        // The command follows the image.
        let image_at = args.iter().position(|a| a == "aegis/proj:r1").unwrap();
        assert_eq!(&args[image_at + 1..], ["node", "server.js"]);
    }

    #[test]
    fn resource_limits_map_to_docker_flags() {
        let limits = ResourceLimits {
            cpu_cores: Some(0.5),
            memory_bytes: Some(64 * 1024 * 1024),
            pids_max: Some(100),
        };
        let run = limit_run_args(&limits);
        assert_eq!(
            run,
            [
                "--cpus",
                "0.5",
                "--memory",
                "67108864",
                "--memory-swap",
                "67108864",
                "--pids-limit",
                "100"
            ]
        );
        assert!(limit_run_args(&ResourceLimits::default()).is_empty());
        let mut p = plan();
        p.limits = limits.clone();
        let args = create_args(&p, Path::new("/x.env"));
        assert!(has_pair(&args, "--cpus", "0.5"));
        assert!(has_pair(&args, "--memory", "67108864"));
        assert!(has_pair(&args, "--pids-limit", "100"));

        let update = limit_update_args(&limits, 8, 16 << 30);
        assert_eq!(update[0], "update");
        assert!(has_pair(&update, "--cpus", "0.5"));
        assert!(has_pair(&update, "--memory", "67108864"));
        assert!(has_pair(&update, "--memory-swap", "67108864"));
        assert!(has_pair(&update, "--pids-limit", "100"));
        // Removing limits from a running container.
        let cleared = limit_update_args(&ResourceLimits::default(), 8, 16 << 30);
        assert!(has_pair(&cleared, "--cpus", "8"));
        assert!(has_pair(&cleared, "--memory", &(16u64 << 30).to_string()));
        assert!(has_pair(&cleared, "--memory-swap", "-1"));
        assert!(has_pair(&cleared, "--pids-limit", "-1"));

        assert_eq!(format_cpus(2.0), "2");
        assert_eq!(format_cpus(0.25), "0.25");
        assert_eq!(format_cpus(1.0 / 3.0), "0.333");
    }

    #[test]
    fn names_and_tags() {
        let project: ProjectId = "0190a1b2-0000-7000-8000-000000000001".parse().unwrap();
        assert_eq!(
            image_tag(&project, "20260101-120000"),
            "aegis/0190a1b2-0000-7000-8000-000000000001:20260101-120000"
        );
        assert_eq!(tag_for_version("r1"), "r1");
        let plus = tag_for_version("1.0+build");
        assert!(plus.starts_with("1.0_build-"), "{plus}");
        assert_ne!(plus, tag_for_version("1.0_build"));
        assert!(tag_for_version("-x").starts_with("v-x-"));
        assert!(tag_for_version(&"a".repeat(128)).len() <= 128);
        assert_eq!(
            volume_name(&project, "data"),
            "aegis-0190a1b2-0000-7000-8000-000000000001-data"
        );
        let args = build_args(
            "aegis/p:r1",
            Path::new("/srv/rel"),
            "docker/app.Dockerfile",
            &[(LABEL_RELEASE.to_string(), "r1".to_string())],
        );
        assert_eq!(args[0], "build");
        assert!(has_pair(&args, "--tag", "aegis/p:r1"));
        assert!(has_pair(&args, "--label", "aegis.release=r1"));
        assert!(args[2].ends_with("app.Dockerfile"));
        assert_eq!(
            Path::new(args.last().unwrap()),
            Path::new("/srv/rel"),
            "the release directory is the build context"
        );
    }
}
