//! Limits enforced by the real kernel. These run only where the daemon
//! could use cgroups (Linux, as root or delegated); elsewhere they pass
//! after printing why they were skipped, unless `AEGIS_REQUIRE_CGROUPS=1`
//! (set in CI), where they fail instead. CI runs them twice: in a transient
//! systemd service with `Delegate=yes` as an unprivileged user (the
//! `install.sh --server` setup), and as root with `sudo`.

use aegis_process::{ProcessSpec, ProcessStatus, ProcessSupervisor, RestartPolicy, StopReason};
use aegis_resources::{BackendKind, Mode, ResourceLimits, ResourceManager};
use aegis_types::ProjectId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn manager() -> Option<Arc<ResourceManager>> {
    let rm = Arc::new(ResourceManager::new(&Mode::Auto));
    let caps = rm.capabilities();
    if !(caps.cpu && caps.memory) {
        let reason = caps.reason.unwrap_or_default();
        if std::env::var("AEGIS_REQUIRE_CGROUPS").is_ok_and(|v| v == "1") {
            panic!("AEGIS_REQUIRE_CGROUPS=1 but cgroup limits are not usable: {reason}");
        }
        eprintln!(
            "SKIPPED: cgroup limits are not usable here ({reason}); set AEGIS_REQUIRE_CGROUPS=1 to fail instead"
        );
        return None;
    }
    eprintln!(
        "cgroups: {} at {} (cpu {}, memory {}, pids {})",
        caps.backend.as_str(),
        caps.location.as_ref().unwrap().display(),
        caps.cpu,
        caps.memory,
        caps.pids
    );
    Some(rm)
}

/// Checks a file in a process's cgroup, read back from the kernel (cgroup
/// v2 only; v1 uses other file names).
fn assert_cgroup_file(rm: &ResourceManager, id: &aegis_types::ProcessId, file: &str, want: &str) {
    let caps = rm.capabilities();
    if caps.backend != BackendKind::CgroupV2 {
        return;
    }
    let dir = caps.location.unwrap().join(id.to_string());
    let got = std::fs::read_to_string(dir.join(file))
        .unwrap_or_else(|e| panic!("reading {}/{file}: {e}", dir.display()));
    assert_eq!(got.trim(), want, "{}/{file}", dir.display());
    eprintln!("read back {}/{file}: {want}", dir.display());
}

fn spec(project: ProjectId, command: &str) -> ProcessSpec {
    ProcessSpec {
        project_id: project,
        command: command.into(),
        cwd: std::env::temp_dir(),
        env: HashMap::new(),
        restart_policy: RestartPolicy::Always,
        log_file: None,
        launcher: None,
    }
}

/// CPU cores used over `window`.
async fn cpu_used(rm: &ResourceManager, id: &aegis_types::ProcessId, window: Duration) -> f64 {
    let a = rm.stats(id).unwrap().cpu_usage_usec;
    let t = Instant::now();
    tokio::time::sleep(window).await;
    let b = rm.stats(id).unwrap().cpu_usage_usec;
    (b - a) as f64 / t.elapsed().as_micros() as f64
}

#[tokio::test]
async fn cpu_limit_caps_a_busy_loop_and_changes_live() {
    let Some(rm) = manager() else { return };
    let supervisor = ProcessSupervisor::new();
    supervisor.set_spawn_hook(rm.clone());
    let project = ProjectId::new();
    rm.load_limits(
        project,
        ResourceLimits {
            cpu_cores: Some(0.2),
            ..Default::default()
        },
    );

    // A child of the shell: forked processes are limited too.
    let id = supervisor
        .spawn(spec(project, "sh -c 'while :; do :; done'"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stats = rm.stats(&id).unwrap();
    assert!(
        stats.procs.len() >= 2,
        "shell and its child are in the cgroup: {stats:?}"
    );

    assert_cgroup_file(&rm, &id, "cpu.max", "20000 100000");

    let limited = cpu_used(&rm, &id, Duration::from_secs(2)).await;
    eprintln!(
        "cpu.max 20000 100000: the busy loop used {limited:.2} cores ({} processes in the cgroup)",
        stats.procs.len()
    );
    assert!(limited < 0.3, "capped at 0.2 cores, used {limited:.2}");
    assert!(limited > 0.1, "the loop is running: {limited:.2}");
    assert!(rm.stats(&id).unwrap().nr_throttled > 0);

    // Raise the limit without restarting.
    let pid = supervisor.get(&id).unwrap().pid;
    rm.set_limits(
        project,
        ResourceLimits {
            cpu_cores: Some(0.6),
            ..Default::default()
        },
    )
    .unwrap();
    assert_cgroup_file(&rm, &id, "cpu.max", "60000 100000");
    let raised = cpu_used(&rm, &id, Duration::from_secs(2)).await;
    eprintln!("cpu.max 60000 100000 (changed live): {raised:.2} cores");
    assert!(
        raised > 0.4 && raised < 0.7,
        "now capped at 0.6: {raised:.2}"
    );
    assert_eq!(supervisor.get(&id).unwrap().pid, pid, "no restart");

    supervisor
        .stop(&id, Duration::ZERO, StopReason::Requested)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(rm.stats(&id).is_none(), "cgroup removed with the process");
}

#[tokio::test]
async fn exceeding_the_memory_limit_is_oom_killed_and_restarted() {
    let Some(rm) = manager() else { return };
    let supervisor = ProcessSupervisor::new();
    supervisor.set_spawn_hook(rm.clone());
    let mut events = supervisor.subscribe();
    let project = ProjectId::new();
    rm.load_limits(
        project,
        ResourceLimits {
            memory_bytes: Some(48 * 1024 * 1024),
            ..Default::default()
        },
    );

    // Allocates (and touches) 16 MiB every 100 ms.
    let hog = "exec python3 -c \"import time\nb=[]\nwhile True:\n  b.append(bytearray(16*1024*1024)); time.sleep(0.1)\"";
    let id = supervisor.spawn(spec(project, hog)).await.unwrap();
    assert_cgroup_file(&rm, &id, "memory.max", &(48u64 << 20).to_string());

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut crashed = false;
    while Instant::now() < deadline && !crashed {
        if let Ok(Ok(aegis_process::ProcessEvent::Exited { will_restart, .. })) =
            tokio::time::timeout(Duration::from_secs(1), events.recv()).await
        {
            assert!(will_restart);
            crashed = true;
        }
    }
    assert!(crashed, "the process was not killed");
    let stats = rm.stats(&id).expect("same cgroup across the restart");
    assert!(stats.oom_kills >= 1, "{stats:?}");
    eprintln!(
        "memory.max {}: killed by the kernel (oom_kill {}), restarting",
        48 << 20,
        stats.oom_kills
    );
    // The supervisor brought it back.
    for _ in 0..50 {
        if supervisor.get(&id).unwrap().status == ProcessStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let restarts = supervisor.get(&id).unwrap().restart_count;
    assert!(restarts >= 1);
    eprintln!("restarted {restarts} time(s), in the same cgroup");
    supervisor
        .stop(&id, Duration::ZERO, StopReason::Requested)
        .await
        .unwrap();
}
