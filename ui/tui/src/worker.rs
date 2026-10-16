//! IO: talks to the daemon over its local gRPC API (the same one the CLI
//! uses) and turns everything it hears into [`Msg`]s for the reducer.
//!
//! The worker owns the connection. When the daemon goes away it reports
//! [`Msg::Disconnected`] and reconnects with backoff; the UI keeps running.

// tonic::Status is large by design; the daemon allows this lint too.
#![allow(clippy::result_large_err)]

use crate::app::{Command, Msg, ProcessCmd};
use aegis_api::aegis::aegis_daemon_client::AegisDaemonClient;
use aegis_api::aegis::{
    ControlProcessRequest, DeployRequest, ListDeploymentsRequest, ListEventsRequest,
    ListProjectsRequest, ListReleasesRequest, LogLine, ProcessAction, ProjectInfo, RollbackRequest,
    StatusRequest, StreamEventsRequest, StreamLogsRequest,
};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::Instant;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Status, Streaming};

type Client = AegisDaemonClient<Channel>;

/// How often projects (and the open app) are re-read.
const REFRESH_EVERY: Duration = Duration::from_secs(2);
/// Delay between an event arriving and the refresh it triggers, so a burst
/// of events causes one refresh.
const EVENT_DEBOUNCE: Duration = Duration::from_millis(300);
const LOG_HISTORY: u32 = 500;
const EVENT_HISTORY: u32 = 200;

/// Wait before reconnect attempt `attempt` (0-based): 1, 2, 4, 8, then 10 s.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.min(4)).min(10))
}

/// Errors meaning the connection is gone rather than "the daemon said no".
fn is_transport(status: &Status) -> bool {
    matches!(
        status.code(),
        Code::Unavailable | Code::Unknown | Code::Cancelled | Code::DeadlineExceeded
    )
}

/// An error and its causes on one line ("transport error: tcp connect error: ...").
fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(e) = source {
        let s = e.to_string();
        if !text.contains(&s) {
            text.push_str(": ");
            text.push_str(&s);
        }
        source = e.source();
    }
    text
}

fn describe_status(status: &Status) -> String {
    if status.message().is_empty() {
        format!("{:?}", status.code())
    } else {
        status.message().to_string()
    }
}

enum End {
    Quit,
    Lost(String),
}

/// The log stream's identity: a new process or a restart (new start time)
/// means the stream must be reopened.
type LogKey = Option<(String, String)>;

struct Worker {
    addr: String,
    tx: UnboundedSender<Msg>,
    /// The app whose detail view is open.
    detail: Option<String>,
    log_key: LogKey,
    /// Whether `log_key` reflects the open app yet.
    log_synced: bool,
}

pub async fn run(addr: String, tx: UnboundedSender<Msg>, mut commands: UnboundedReceiver<Command>) {
    let mut worker = Worker {
        addr,
        tx,
        detail: None,
        log_key: None,
        log_synced: false,
    };
    let mut attempt = 0;
    loop {
        match worker.connect().await {
            Ok((client, version)) => {
                attempt = 0;
                worker.send(Msg::Connected { version });
                match worker.session(client, &mut commands).await {
                    End::Quit => return,
                    // Try again straight away; backoff starts if that fails.
                    End::Lost(error) => worker.send(Msg::Disconnected {
                        error,
                        retry_secs: 0,
                    }),
                }
            }
            Err(error) => {
                let wait = backoff(attempt);
                attempt = attempt.saturating_add(1);
                if worker.wait_offline(&error, wait, &mut commands).await {
                    return;
                }
            }
        }
    }
}

async fn next_log(logs: &mut Option<Streaming<LogLine>>) -> Result<Option<LogLine>, Status> {
    match logs {
        Some(stream) => stream.message().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

impl Worker {
    fn send(&self, msg: Msg) {
        // The UI has quit when this fails; the worker stops on its next recv.
        let _ = self.tx.send(msg);
    }

    async fn connect(&self) -> Result<(Client, String), String> {
        let endpoint = Endpoint::from_shared(self.addr.clone())
            .map_err(|e| format!("invalid address: {}", describe(&e)))?
            .connect_timeout(Duration::from_secs(3))
            .http2_keep_alive_interval(Duration::from_secs(15))
            .keep_alive_timeout(Duration::from_secs(10));
        let channel = endpoint.connect().await.map_err(|e| describe(&e))?;
        let mut client = AegisDaemonClient::new(channel);
        let status = client
            .get_status(StatusRequest {})
            .await
            .map_err(|s| describe_status(&s))?
            .into_inner();
        Ok((client, status.version))
    }

    /// Waits before the next reconnect, counting down in the banner. Returns
    /// true when the UI has quit.
    async fn wait_offline(
        &mut self,
        error: &str,
        wait: Duration,
        commands: &mut UnboundedReceiver<Command>,
    ) -> bool {
        let deadline = Instant::now() + wait;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            self.send(Msg::Disconnected {
                error: error.to_string(),
                retry_secs: remaining.as_secs_f64().ceil() as u64,
            });
            tokio::select! {
                _ = tokio::time::sleep(remaining.min(Duration::from_secs(1))) => {}
                cmd = commands.recv() => match cmd {
                    None => return true,
                    // "r" while offline: retry now.
                    Some(Command::Refresh) => return false,
                    Some(Command::OpenDetail { project }) => self.set_detail(Some(project)),
                    Some(Command::CloseDetail) => self.set_detail(None),
                    Some(_) => self.send(Msg::ActionDone {
                        ok: false,
                        message: "Not connected to the daemon; the action was not sent.".into(),
                    }),
                },
            }
        }
    }

    fn set_detail(&mut self, project: Option<String>) {
        self.detail = project;
        self.log_key = None;
        self.log_synced = false;
    }

    async fn session(
        &mut self,
        mut client: Client,
        commands: &mut UnboundedReceiver<Command>,
    ) -> End {
        // Subscribe before reading history so nothing falls in between; the
        // reducer drops duplicates.
        let mut events = match client.stream_events(StreamEventsRequest {}).await {
            Ok(r) => r.into_inner(),
            Err(s) => return End::Lost(describe_status(&s)),
        };
        match client
            .list_events(ListEventsRequest {
                limit: EVENT_HISTORY,
                ..Default::default()
            })
            .await
        {
            Ok(r) => self.send(Msg::EventsHistory(r.into_inner().events)),
            Err(s) if is_transport(&s) => return End::Lost(describe_status(&s)),
            Err(_) => {}
        }

        let mut logs: Option<Streaming<LogLine>> = None;
        self.log_synced = false;
        if let Err(e) = self.refresh(&mut client, &mut logs).await {
            return End::Lost(e);
        }
        let mut tick = tokio::time::interval(REFRESH_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut refresh_soon: Option<Instant> = None;

        loop {
            let result = tokio::select! {
                cmd = commands.recv() => match cmd {
                    None => return End::Quit,
                    Some(cmd) => self.handle(cmd, &mut client, &mut logs).await,
                },
                msg = events.message() => match msg {
                    Ok(Some(event)) => {
                        self.send(Msg::Event(event));
                        refresh_soon.get_or_insert(Instant::now() + EVENT_DEBOUNCE);
                        Ok(())
                    }
                    Ok(None) => Err("the daemon closed the event stream".to_string()),
                    Err(s) => Err(describe_status(&s)),
                },
                line = next_log(&mut logs) => {
                    let project_id = self.detail.clone().unwrap_or_default();
                    match line {
                        Ok(Some(line)) => self.send(Msg::Log { project_id, line }),
                        Ok(None) => {
                            logs = None;
                            self.send(Msg::LogsEnded {
                                project_id,
                                reason: "log stream ended (the process stopped or was replaced)".into(),
                            });
                        }
                        Err(s) => {
                            // A dead connection also ends the event stream,
                            // which triggers the reconnect.
                            logs = None;
                            self.send(Msg::LogsEnded {
                                project_id,
                                reason: format!("log stream failed: {}", describe_status(&s)),
                            });
                        }
                    }
                    Ok(())
                },
                _ = sleep_until(refresh_soon) => {
                    refresh_soon = None;
                    self.refresh(&mut client, &mut logs).await
                },
                _ = tick.tick() => self.refresh(&mut client, &mut logs).await,
            };
            if let Err(e) = result {
                return End::Lost(e);
            }
        }
    }

    /// Re-reads projects and, when an app is open, its releases, deployments
    /// and (if its process changed) its log stream. Errors mean the
    /// connection is gone.
    async fn refresh(
        &mut self,
        client: &mut Client,
        logs: &mut Option<Streaming<LogLine>>,
    ) -> Result<(), String> {
        let projects = client
            .list_projects(ListProjectsRequest {})
            .await
            .map_err(|s| describe_status(&s))?
            .into_inner()
            .projects;
        let Some(project_id) = self.detail.clone() else {
            self.send(Msg::Projects(projects));
            return Ok(());
        };
        let key = log_key(projects.iter().find(|p| p.id == project_id));
        self.send(Msg::Projects(projects));
        self.load_detail(client, &project_id).await?;
        if !self.log_synced || key != self.log_key {
            self.log_synced = true;
            self.log_key = key.clone();
            *logs = self.open_logs(client, &project_id, key.is_some()).await?;
        }
        Ok(())
    }

    async fn load_detail(&self, client: &mut Client, project_id: &str) -> Result<(), String> {
        let releases = client
            .list_releases(ListReleasesRequest {
                project: project_id.to_string(),
            })
            .await;
        let deployments = client
            .list_deployments(ListDeploymentsRequest {
                project: project_id.to_string(),
            })
            .await;
        match (releases, deployments) {
            (Ok(r), Ok(d)) => {
                self.send(Msg::Detail {
                    project_id: project_id.to_string(),
                    releases: r.into_inner().releases,
                    deployments: d.into_inner().deployments,
                });
                Ok(())
            }
            (Err(s), _) | (_, Err(s)) if is_transport(&s) => Err(describe_status(&s)),
            // E.g. the project was removed; the detail view says so.
            _ => Ok(()),
        }
    }

    async fn open_logs(
        &self,
        client: &mut Client,
        project_id: &str,
        has_process: bool,
    ) -> Result<Option<Streaming<LogLine>>, String> {
        let project_id = project_id.to_string();
        self.send(Msg::LogsReset {
            project_id: project_id.clone(),
        });
        if !has_process {
            self.send(Msg::LogsEnded {
                project_id,
                reason: "No process yet. Deploy the app (d) to see its logs.".into(),
            });
            return Ok(None);
        }
        match client
            .stream_logs(StreamLogsRequest {
                target: project_id.clone(),
                lines: LOG_HISTORY,
            })
            .await
        {
            Ok(r) => Ok(Some(r.into_inner())),
            Err(s) if is_transport(&s) => Err(describe_status(&s)),
            Err(s) => {
                self.send(Msg::LogsEnded {
                    project_id,
                    reason: format!("logs unavailable: {}", describe_status(&s)),
                });
                Ok(None)
            }
        }
    }

    async fn handle(
        &mut self,
        cmd: Command,
        client: &mut Client,
        logs: &mut Option<Streaming<LogLine>>,
    ) -> Result<(), String> {
        match cmd {
            Command::Refresh => {
                self.refresh(client, logs).await?;
                self.send(Msg::ActionDone {
                    ok: true,
                    message: "Refreshed".into(),
                });
            }
            Command::OpenDetail { project } => {
                self.set_detail(Some(project));
                *logs = None;
                self.refresh(client, logs).await?;
            }
            Command::CloseDetail => {
                self.set_detail(None);
                *logs = None;
            }
            action => {
                // Actions can take a while (a rollback returns once the old
                // release is live), so they run beside the refresh loop.
                let client = client.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let (ok, message) = perform(client, action).await;
                    let _ = tx.send(Msg::ActionDone { ok, message });
                });
            }
        }
        Ok(())
    }
}

fn log_key(project: Option<&ProjectInfo>) -> LogKey {
    let process = project?.process.as_ref()?;
    Some((process.id.clone(), process.last_start.clone()))
}

/// Runs a user action and describes the outcome for the status line.
async fn perform(mut client: Client, action: Command) -> (bool, String) {
    match action {
        Command::Control {
            project,
            name,
            action,
        } => {
            let pb_action = match action {
                ProcessCmd::Start => ProcessAction::Start,
                ProcessCmd::Stop => ProcessAction::Stop,
                ProcessCmd::Restart => ProcessAction::Restart,
            };
            match client
                .control_process(ControlProcessRequest {
                    target: project,
                    action: pb_action as i32,
                })
                .await
            {
                Ok(r) => (
                    true,
                    format!("{} {}: now {}", action.verb(), name, r.into_inner().status),
                ),
                Err(s) => (
                    false,
                    format!("{} {} failed: {}", action.verb(), name, describe_status(&s)),
                ),
            }
        }
        Command::Deploy { project, name } => {
            match client
                .deploy(DeployRequest {
                    project,
                    ..Default::default()
                })
                .await
            {
                Ok(r) => {
                    let id = r.into_inner().deployment_id;
                    (
                        true,
                        format!(
                            "Deployment {} queued for {}; progress shows under Deployments and Events",
                            id.get(..8).unwrap_or(&id),
                            name
                        ),
                    )
                }
                Err(s) => (
                    false,
                    format!("Deploy {} failed: {}", name, describe_status(&s)),
                ),
            }
        }
        Command::Rollback {
            project,
            name,
            version,
        } => {
            match client
                .rollback(RollbackRequest {
                    project,
                    version: version.unwrap_or_default(),
                })
                .await
            {
                Ok(r) => (
                    true,
                    format!("{} rolled back to {}", name, r.into_inner().version),
                ),
                Err(s) => (
                    false,
                    format!("Rollback of {} failed: {}", name, describe_status(&s)),
                ),
            }
        }
        Command::Refresh | Command::OpenDetail { .. } | Command::CloseDetail => {
            (true, String::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_api::aegis::ProcessInfo;

    #[test]
    fn backoff_grows_and_caps() {
        let secs: Vec<u64> = (0..7).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 10, 10, 10]);
        assert_eq!(backoff(u32::MAX).as_secs(), 10);
    }

    #[test]
    fn log_key_changes_on_restart_and_new_process() {
        let mut p = ProjectInfo::default();
        assert_eq!(log_key(Some(&p)), None);
        p.process = Some(ProcessInfo {
            id: "p1".into(),
            last_start: "t1".into(),
            ..Default::default()
        });
        let first = log_key(Some(&p));
        p.process.as_mut().unwrap().last_start = "t2".into();
        assert_ne!(log_key(Some(&p)), first);
    }

    async fn expect<T>(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
        mut pick: impl FnMut(Msg) -> Option<T>,
    ) -> T {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let msg = rx.recv().await.expect("worker running");
                if let Some(v) = pick(msg) {
                    return v;
                }
            }
        })
        .await
        .expect("expected message")
    }

    /// Drives the worker against a real daemon. Opt-in, because it needs one:
    /// `AEGIS_TUI_LIVE_ADDR=http://127.0.0.1:50051 cargo test -p aegis-tui -- --ignored`
    /// It registers a throwaway project, so use a scratch daemon.
    #[tokio::test]
    #[ignore]
    async fn live_daemon_smoke() {
        let addr = std::env::var("AEGIS_TUI_LIVE_ADDR").expect("set AEGIS_TUI_LIVE_ADDR");
        let mut client = AegisDaemonClient::connect(addr.clone()).await.unwrap();
        let name = format!("tui-smoke-{}", std::process::id());
        let project = client
            .register_project(aegis_api::aegis::RegisterProjectRequest {
                name: name.clone(),
                source_dir: std::env::temp_dir().to_string_lossy().to_string(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(run(addr, tx, cmd_rx));
        expect(&mut rx, |m| {
            matches!(m, Msg::Connected { .. }).then_some(())
        })
        .await;
        expect(&mut rx, |m| match m {
            Msg::EventsHistory(events) => {
                assert!(
                    events.iter().any(|e| e.payload_json.contains(&name)),
                    "history includes the registration"
                );
                Some(())
            }
            _ => None,
        })
        .await;
        expect(&mut rx, |m| match m {
            Msg::Projects(p) => p.iter().any(|p| p.id == project.id).then_some(()),
            _ => None,
        })
        .await;

        cmd_tx
            .send(Command::OpenDetail {
                project: project.id.clone(),
            })
            .unwrap();
        expect(&mut rx, |m| match m {
            Msg::Detail { project_id, .. } => (project_id == project.id).then_some(()),
            _ => None,
        })
        .await;
        let reason = expect(&mut rx, |m| match m {
            Msg::LogsEnded { reason, .. } => Some(reason),
            _ => None,
        })
        .await;
        assert!(reason.contains("No process yet"), "{}", reason);

        // A new event reaches the stream.
        client
            .emit_event(aegis_api::aegis::EmitEventRequest {
                event_type: "TuiSmokePing".into(),
                payload_json: "{}".into(),
            })
            .await
            .unwrap();
        expect(&mut rx, |m| match m {
            Msg::Event(e) => (e.event_type == "TuiSmokePing").then_some(()),
            _ => None,
        })
        .await;

        // Stopping a never-deployed app fails with the daemon's message.
        cmd_tx
            .send(Command::Control {
                project: project.id.clone(),
                name: name.clone(),
                action: ProcessCmd::Stop,
            })
            .unwrap();
        let (ok, message) = expect(&mut rx, |m| match m {
            Msg::ActionDone { ok, message } => Some((ok, message)),
            _ => None,
        })
        .await;
        assert!(!ok && message.contains("failed"), "{}", message);

        drop(cmd_tx);
        handle.await.unwrap();
    }

    /// Manual reconnect check: start this, then stop and restart the daemon
    /// within a minute. `AEGIS_TUI_LIVE_ADDR=... cargo test -p aegis-tui live_reconnect -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn live_reconnect() {
        let addr = std::env::var("AEGIS_TUI_LIVE_ADDR").expect("set AEGIS_TUI_LIVE_ADDR");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(run(addr, tx, cmd_rx));
        async fn wait(
            rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
            want_connected: bool,
        ) -> Result<(), tokio::time::error::Elapsed> {
            tokio::time::timeout(Duration::from_secs(60), async {
                loop {
                    match rx.recv().await.expect("worker running") {
                        Msg::Connected { .. } if want_connected => return,
                        Msg::Disconnected { .. } if !want_connected => return,
                        _ => {}
                    }
                }
            })
            .await
        }
        wait(&mut rx, true).await.expect("connected");
        wait(&mut rx, false)
            .await
            .expect("noticed the daemon going away");
        wait(&mut rx, true).await.expect("reconnected");
        drop(cmd_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn unreachable_daemon_reports_disconnected_and_keeps_running() {
        // Bind and drop a listener to get a port nothing listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(run(format!("http://127.0.0.1:{}", port), tx, cmd_rx));
        let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("a message")
            .expect("channel open");
        assert!(
            matches!(msg, Msg::Disconnected { retry_secs: 1, .. }),
            "{:?}",
            msg
        );
        // Actions while offline fail politely instead of hanging.
        cmd_tx
            .send(Command::Deploy {
                project: "x".into(),
                name: "x".into(),
            })
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(Msg::ActionDone { ok, .. }) = rx.recv().await {
                    return ok;
                }
            }
        })
        .await
        .unwrap();
        assert!(!reply);
        // Closing the command channel stops the worker.
        drop(cmd_tx);
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("worker stops")
            .unwrap();
    }
}
