//! Dashboard state and the pure reducer that drives it.
//!
//! Nothing here does IO: [`App::update`] takes a [`Msg`] (a key press, data
//! from the daemon, a clock tick) and returns the [`Command`]s the IO worker
//! should run. That keeps every interaction testable without a terminal or a
//! daemon.

use aegis_api::aegis::{
    DeploymentInfo, EventResponse, LogLine, ProcessInfo, ProjectInfo, ReleaseInfo,
};
use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::{HashSet, VecDeque};

/// Log lines kept for the open app.
pub const MAX_LOG_LINES: usize = 2000;
/// Events kept in the event pane.
pub const MAX_EVENTS: usize = 1000;
/// Rows moved by PageUp / PageDown.
const PAGE: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Apps,
    Events,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailPane {
    Releases,
    Deployments,
    Logs,
}

impl DetailPane {
    fn next(self) -> Self {
        match self {
            Self::Releases => Self::Deployments,
            Self::Deployments => Self::Logs,
            Self::Logs => Self::Releases,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Releases => Self::Logs,
            Self::Deployments => Self::Releases,
            Self::Logs => Self::Deployments,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The tabbed overview (apps list or event stream).
    List,
    /// One app: releases, deployments and its live log.
    Detail { project_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Connected { version: String },
    Disconnected { error: String, retry_secs: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessCmd {
    Start,
    Stop,
    Restart,
}

impl ProcessCmd {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Restart => "Restart",
        }
    }
}

/// Work for the IO worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Reload projects (and the open app), or reconnect now when offline.
    Refresh,
    /// Load an app's releases and deployments and follow its log.
    OpenDetail {
        project: String,
    },
    CloseDetail,
    Control {
        project: String,
        name: String,
        action: ProcessCmd,
    },
    /// Deploy the project from its configured source (repository or directory).
    Deploy {
        project: String,
        name: String,
    },
    /// Roll back to `version`, or to the previous release when `None`.
    Rollback {
        project: String,
        name: String,
        version: Option<String>,
    },
}

/// Everything that can change the dashboard.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Key(KeyEvent),
    Resize,
    Tick(DateTime<Utc>),
    Connected {
        version: String,
    },
    Disconnected {
        error: String,
        retry_secs: u64,
    },
    Projects(Vec<ProjectInfo>),
    Detail {
        project_id: String,
        releases: Vec<ReleaseInfo>,
        deployments: Vec<DeploymentInfo>,
    },
    EventsHistory(Vec<EventResponse>),
    Event(EventResponse),
    /// The log stream for `project_id` (re)started; previous lines are stale.
    LogsReset {
        project_id: String,
    },
    Log {
        project_id: String,
        line: LogLine,
    },
    LogsEnded {
        project_id: String,
        reason: String,
    },
    ActionDone {
        ok: bool,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Ok,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusLine {
    pub kind: StatusKind,
    pub text: String,
}

/// Which filter `/` edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterTarget {
    Apps,
    Events,
    Logs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub prompt: String,
    pub command: Command,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter(FilterTarget),
    Confirm(Pending),
    Help,
}

/// A tail view: newest at the bottom, optionally pinned there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tail {
    pub follow: bool,
    /// Rows scrolled up from the newest one (0 = bottom).
    pub offset: usize,
}

impl Default for Tail {
    fn default() -> Self {
        Self {
            follow: true,
            offset: 0,
        }
    }
}

impl Tail {
    fn up(&mut self, n: usize, len: usize) {
        self.follow = false;
        self.offset = (self.offset + n).min(len.saturating_sub(1));
    }

    fn down(&mut self, n: usize) {
        self.offset = self.offset.saturating_sub(n);
    }

    fn top(&mut self, len: usize) {
        self.follow = false;
        self.offset = len.saturating_sub(1);
    }

    fn bottom(&mut self) {
        self.follow = true;
        self.offset = 0;
    }

    fn toggle_follow(&mut self) {
        if self.follow {
            self.follow = false;
        } else {
            self.bottom();
        }
    }

    /// Keeps a paused view still while new rows arrive below it.
    fn pushed(&mut self) {
        if !self.follow {
            self.offset += 1;
        }
    }

    /// The `[start, end)` window of `len` rows that fits in `height`.
    pub fn window(&self, len: usize, height: usize) -> (usize, usize) {
        let offset = if self.follow {
            0
        } else {
            self.offset.min(len.saturating_sub(height))
        };
        let end = len - offset.min(len);
        (end.saturating_sub(height), end)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DetailState {
    pub focus: Option<DetailPane>,
    /// Newest first.
    pub releases: Vec<ReleaseInfo>,
    /// Newest first.
    pub deployments: Vec<DeploymentInfo>,
    pub loaded: bool,
    pub release_selected: usize,
    pub deployment_selected: usize,
    pub logs: VecDeque<LogLine>,
    pub log_filter: String,
    pub log_tail: Tail,
    /// Why the log is empty or stopped, when it is.
    pub log_note: Option<String>,
}

impl DetailState {
    pub fn focus(&self) -> DetailPane {
        self.focus.unwrap_or(DetailPane::Releases)
    }
}

pub struct App {
    pub addr: String,
    pub now: DateTime<Utc>,
    pub connection: Connection,
    pub tab: Tab,
    pub screen: Screen,
    pub mode: Mode,
    pub status: Option<StatusLine>,
    pub should_quit: bool,
    /// Sorted by name.
    pub projects: Vec<ProjectInfo>,
    pub apps_filter: String,
    /// Index into [`App::filtered_projects`].
    pub apps_selected: usize,
    /// Oldest first.
    pub events: VecDeque<EventResponse>,
    event_ids: HashSet<String>,
    pub events_filter: String,
    pub events_tail: Tail,
    pub detail: DetailState,
}

fn matches(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Event type filter: comma-separated alternatives, each a case-insensitive
/// substring (`deploy,crash` shows deployment and crash events).
pub fn event_matches(event_type: &str, filter: &str) -> bool {
    let terms: Vec<&str> = filter
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    terms.is_empty() || terms.iter().any(|t| matches(event_type, t))
}

pub fn log_matches(line: &LogLine, filter: &str) -> bool {
    matches(&line.line, filter)
}

/// How long a running process has been up.
pub fn uptime(process: &ProcessInfo, now: DateTime<Utc>) -> Option<chrono::Duration> {
    if process.status != "Running" {
        return None;
    }
    let started = DateTime::parse_from_rfc3339(&process.last_start).ok()?;
    Some(now.signed_duration_since(started.with_timezone(&Utc)))
}

impl App {
    pub fn new(addr: String, now: DateTime<Utc>) -> Self {
        Self {
            addr,
            now,
            connection: Connection::Connecting,
            tab: Tab::Apps,
            screen: Screen::List,
            mode: Mode::Normal,
            status: None,
            should_quit: false,
            projects: Vec::new(),
            apps_filter: String::new(),
            apps_selected: 0,
            events: VecDeque::new(),
            event_ids: HashSet::new(),
            events_filter: String::new(),
            events_tail: Tail::default(),
            detail: DetailState::default(),
        }
    }

    pub fn filtered_projects(&self) -> Vec<&ProjectInfo> {
        self.projects
            .iter()
            .filter(|p| matches(&p.name, &self.apps_filter))
            .collect()
    }

    pub fn filtered_events(&self) -> Vec<&EventResponse> {
        self.events
            .iter()
            .filter(|e| event_matches(&e.event_type, &self.events_filter))
            .collect()
    }

    pub fn filtered_logs(&self) -> Vec<&LogLine> {
        self.detail
            .logs
            .iter()
            .filter(|l| log_matches(l, &self.detail.log_filter))
            .collect()
    }

    pub fn selected_project(&self) -> Option<&ProjectInfo> {
        self.filtered_projects().get(self.apps_selected).copied()
    }

    pub fn detail_project(&self) -> Option<&ProjectInfo> {
        match &self.screen {
            Screen::Detail { project_id } => self.projects.iter().find(|p| &p.id == project_id),
            Screen::List => None,
        }
    }

    fn detail_id(&self) -> Option<&str> {
        match &self.screen {
            Screen::Detail { project_id } => Some(project_id),
            Screen::List => None,
        }
    }

    /// The app that action keys apply to: the open one, else the selected one.
    fn target_project(&self) -> Option<&ProjectInfo> {
        match self.screen {
            Screen::Detail { .. } => self.detail_project(),
            Screen::List if self.tab == Tab::Apps => self.selected_project(),
            Screen::List => None,
        }
    }

    fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.status = Some(StatusLine {
            kind,
            text: text.into(),
        });
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Command> {
        match msg {
            Msg::Key(key) => return self.on_key(key),
            Msg::Resize => {}
            Msg::Tick(now) => self.now = now,
            Msg::Connected { version } => {
                self.connection = Connection::Connected {
                    version: version.clone(),
                };
                self.set_status(StatusKind::Ok, format!("Connected to daemon v{}", version));
            }
            Msg::Disconnected { error, retry_secs } => {
                self.connection = Connection::Disconnected { error, retry_secs };
            }
            Msg::Projects(mut projects) => {
                let selected = self.selected_project().map(|p| p.id.clone());
                projects.sort_by_key(|p| p.name.to_lowercase());
                self.projects = projects;
                let filtered = self.filtered_projects();
                self.apps_selected = selected
                    .and_then(|id| filtered.iter().position(|p| p.id == id))
                    .unwrap_or(self.apps_selected)
                    .min(filtered.len().saturating_sub(1));
            }
            Msg::Detail {
                project_id,
                mut releases,
                mut deployments,
            } => {
                if self.detail_id() == Some(project_id.as_str()) {
                    // The daemon lists releases oldest first.
                    releases.reverse();
                    deployments.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                    deployments.truncate(50);
                    let d = &mut self.detail;
                    d.release_selected = d.release_selected.min(releases.len().saturating_sub(1));
                    d.deployment_selected = d
                        .deployment_selected
                        .min(deployments.len().saturating_sub(1));
                    d.releases = releases;
                    d.deployments = deployments;
                    d.loaded = true;
                }
            }
            Msg::EventsHistory(history) => {
                for event in history {
                    if self.event_ids.insert(event.id.clone()) {
                        self.events.push_back(event);
                    }
                }
                self.events
                    .make_contiguous()
                    .sort_by(|a, b| a.created_at.cmp(&b.created_at));
                self.trim_events();
            }
            Msg::Event(event) => {
                if self.event_ids.insert(event.id.clone()) {
                    if event_matches(&event.event_type, &self.events_filter) {
                        self.events_tail.pushed();
                    }
                    self.events.push_back(event);
                    self.trim_events();
                }
            }
            Msg::LogsReset { project_id } => {
                if self.detail_id() == Some(project_id.as_str()) {
                    self.detail.logs.clear();
                    self.detail.log_note = None;
                    self.detail.log_tail = Tail::default();
                }
            }
            Msg::Log { project_id, line } => {
                if self.detail_id() == Some(project_id.as_str()) {
                    if log_matches(&line, &self.detail.log_filter) {
                        self.detail.log_tail.pushed();
                    }
                    self.detail.log_note = None;
                    self.detail.logs.push_back(line);
                    while self.detail.logs.len() > MAX_LOG_LINES {
                        self.detail.logs.pop_front();
                    }
                }
            }
            Msg::LogsEnded { project_id, reason } => {
                if self.detail_id() == Some(project_id.as_str()) {
                    self.detail.log_note = Some(reason);
                }
            }
            Msg::ActionDone { ok, message } => {
                let kind = if ok {
                    StatusKind::Ok
                } else {
                    StatusKind::Error
                };
                self.set_status(kind, message);
            }
        }
        Vec::new()
    }

    fn trim_events(&mut self) {
        while self.events.len() > MAX_EVENTS {
            if let Some(old) = self.events.pop_front() {
                self.event_ids.remove(&old.id);
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Command> {
        // Windows also reports key releases; act on presses (and repeats) only.
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Vec::new();
        }
        match self.mode.clone() {
            Mode::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
                ) {
                    self.mode = Mode::Normal;
                }
                Vec::new()
            }
            Mode::Confirm(pending) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.mode = Mode::Normal;
                    self.set_status(StatusKind::Info, format!("{}...", pending.prompt));
                    vec![pending.command]
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                    self.mode = Mode::Normal;
                    self.set_status(StatusKind::Info, "Cancelled");
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Mode::Filter(target) => {
                self.on_filter_key(target, key);
                Vec::new()
            }
            Mode::Normal => self.on_normal_key(key),
        }
    }

    fn filter_mut(&mut self, target: FilterTarget) -> &mut String {
        match target {
            FilterTarget::Apps => &mut self.apps_filter,
            FilterTarget::Events => &mut self.events_filter,
            FilterTarget::Logs => &mut self.detail.log_filter,
        }
    }

    fn on_filter_key(&mut self, target: FilterTarget, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Esc => {
                self.filter_mut(target).clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.filter_mut(target).pop();
            }
            KeyCode::Char(c) => self.filter_mut(target).push(c),
            _ => return,
        }
        // The filtered set changed; start from the top / newest again.
        match target {
            FilterTarget::Apps => self.apps_selected = 0,
            FilterTarget::Events => self.events_tail = Tail::default(),
            FilterTarget::Logs => self.detail.log_tail = Tail::default(),
        }
    }

    fn filter_target(&self) -> FilterTarget {
        match (&self.screen, self.tab) {
            (Screen::Detail { .. }, _) => FilterTarget::Logs,
            (Screen::List, Tab::Apps) => FilterTarget::Apps,
            (Screen::List, Tab::Events) => FilterTarget::Events,
        }
    }

    fn open_detail(&mut self, focus: DetailPane) -> Vec<Command> {
        let Some(project) = self.selected_project() else {
            return Vec::new();
        };
        let project_id = project.id.clone();
        self.detail = DetailState {
            focus: Some(focus),
            ..DetailState::default()
        };
        self.screen = Screen::Detail {
            project_id: project_id.clone(),
        };
        vec![Command::OpenDetail {
            project: project_id,
        }]
    }

    fn close_detail(&mut self) -> Vec<Command> {
        self.screen = Screen::List;
        self.detail = DetailState::default();
        vec![Command::CloseDetail]
    }

    fn confirm(&mut self, prompt: String, command: Command) {
        self.mode = Mode::Confirm(Pending { prompt, command });
    }

    fn on_action_key(&mut self, code: KeyCode) -> bool {
        let Some(project) = self.target_project() else {
            return false;
        };
        let (id, name) = (project.id.clone(), project.name.clone());
        let running = project
            .process
            .as_ref()
            .is_some_and(|p| p.status == "Running");
        let control = |action: ProcessCmd| Command::Control {
            project: id.clone(),
            name: name.clone(),
            action,
        };
        match code {
            KeyCode::Char('s') => {
                if running {
                    self.set_status(StatusKind::Info, format!("{} is already running", name));
                } else {
                    self.confirm(format!("Start {}", name), control(ProcessCmd::Start));
                }
            }
            KeyCode::Char('x') => self.confirm(format!("Stop {}", name), control(ProcessCmd::Stop)),
            KeyCode::Char('R') => {
                self.confirm(format!("Restart {}", name), control(ProcessCmd::Restart))
            }
            KeyCode::Char('d') => self.confirm(
                format!("Deploy {} from its configured source", name),
                Command::Deploy {
                    project: id.clone(),
                    name: name.clone(),
                },
            ),
            KeyCode::Char('b') => {
                let selected = self.selected_rollback_version();
                let prompt = match &selected {
                    Some(v) => format!("Roll back {} to release {}", name, v),
                    None => format!("Roll back {} to the previous release", name),
                };
                self.confirm(
                    prompt,
                    Command::Rollback {
                        project: id.clone(),
                        name: name.clone(),
                        version: selected,
                    },
                );
            }
            _ => return false,
        }
        true
    }

    /// The release picked in the detail view, unless it is the live one.
    fn selected_rollback_version(&self) -> Option<String> {
        if !matches!(self.screen, Screen::Detail { .. })
            || self.detail.focus() != DetailPane::Releases
        {
            return None;
        }
        let release = self.detail.releases.get(self.detail.release_selected)?;
        let live = self.detail_project().map(|p| p.current_release.as_str());
        (live != Some(release.version.as_str())).then(|| release.version.clone())
    }

    fn on_normal_key(&mut self, key: KeyEvent) -> Vec<Command> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return Vec::new();
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                return Vec::new();
            }
            KeyCode::Char('r') => {
                self.set_status(StatusKind::Info, "Refreshing...");
                return vec![Command::Refresh];
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Filter(self.filter_target());
                return Vec::new();
            }
            KeyCode::Char('1') | KeyCode::Char('2') => {
                self.tab = if key.code == KeyCode::Char('1') {
                    Tab::Apps
                } else {
                    Tab::Events
                };
                if matches!(self.screen, Screen::Detail { .. }) {
                    return self.close_detail();
                }
                return Vec::new();
            }
            code @ (KeyCode::Char('s')
            | KeyCode::Char('x')
            | KeyCode::Char('R')
            | KeyCode::Char('d')
            | KeyCode::Char('b')) => {
                self.on_action_key(code);
                return Vec::new();
            }
            _ => {}
        }
        match self.screen {
            Screen::List => match self.tab {
                Tab::Apps => self.on_apps_key(key),
                Tab::Events => {
                    self.on_events_key(key);
                    Vec::new()
                }
            },
            Screen::Detail { .. } => self.on_detail_key(key),
        }
    }

    fn on_apps_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let len = self.filtered_projects().len();
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => self.tab = Tab::Events,
            KeyCode::Up | KeyCode::Char('k') => {
                self.apps_selected = self.apps_selected.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.apps_selected = (self.apps_selected + 1).min(len.saturating_sub(1))
            }
            KeyCode::Home | KeyCode::Char('g') => self.apps_selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.apps_selected = len.saturating_sub(1),
            KeyCode::Enter => return self.open_detail(DetailPane::Releases),
            KeyCode::Char('l') => return self.open_detail(DetailPane::Logs),
            KeyCode::Esc => {
                self.apps_filter.clear();
            }
            _ => {}
        }
        Vec::new()
    }

    fn on_events_key(&mut self, key: KeyEvent) {
        let len = self.filtered_events().len();
        let tail = &mut self.events_tail;
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => self.tab = Tab::Apps,
            KeyCode::Up | KeyCode::Char('k') => tail.up(1, len),
            KeyCode::Down | KeyCode::Char('j') => tail.down(1),
            KeyCode::PageUp => tail.up(PAGE, len),
            KeyCode::PageDown => tail.down(PAGE),
            KeyCode::Home | KeyCode::Char('g') => tail.top(len),
            KeyCode::End | KeyCode::Char('G') => tail.bottom(),
            KeyCode::Char('f') => tail.toggle_follow(),
            KeyCode::Esc => {
                self.events_filter.clear();
                self.events_tail = Tail::default();
            }
            _ => {}
        }
    }

    fn on_detail_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let log_len = self.filtered_logs().len();
        let d = &mut self.detail;
        let focus = d.focus();
        match key.code {
            KeyCode::Esc => return self.close_detail(),
            KeyCode::Tab => d.focus = Some(focus.next()),
            KeyCode::BackTab => d.focus = Some(focus.prev()),
            KeyCode::Char('f') => d.log_tail.toggle_follow(),
            KeyCode::PageUp => d.log_tail.up(PAGE, log_len),
            KeyCode::PageDown => d.log_tail.down(PAGE),
            KeyCode::Up | KeyCode::Char('k') => match focus {
                DetailPane::Releases => d.release_selected = d.release_selected.saturating_sub(1),
                DetailPane::Deployments => {
                    d.deployment_selected = d.deployment_selected.saturating_sub(1)
                }
                DetailPane::Logs => d.log_tail.up(1, log_len),
            },
            KeyCode::Down | KeyCode::Char('j') => match focus {
                DetailPane::Releases => {
                    d.release_selected =
                        (d.release_selected + 1).min(d.releases.len().saturating_sub(1))
                }
                DetailPane::Deployments => {
                    d.deployment_selected =
                        (d.deployment_selected + 1).min(d.deployments.len().saturating_sub(1))
                }
                DetailPane::Logs => d.log_tail.down(1),
            },
            KeyCode::Home | KeyCode::Char('g') => match focus {
                DetailPane::Releases => d.release_selected = 0,
                DetailPane::Deployments => d.deployment_selected = 0,
                DetailPane::Logs => d.log_tail.top(log_len),
            },
            KeyCode::End | KeyCode::Char('G') => match focus {
                DetailPane::Releases => d.release_selected = d.releases.len().saturating_sub(1),
                DetailPane::Deployments => {
                    d.deployment_selected = d.deployments.len().saturating_sub(1)
                }
                DetailPane::Logs => d.log_tail.bottom(),
            },
            _ => {}
        }
        Vec::new()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    pub fn ch(c: char) -> Msg {
        key(KeyCode::Char(c))
    }

    pub fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    pub fn project(name: &str, status: Option<&str>) -> ProjectInfo {
        ProjectInfo {
            id: format!("id-{}", name),
            name: name.to_string(),
            current_release: "v2".to_string(),
            process: status.map(|s| ProcessInfo {
                id: format!("proc-{}", name),
                status: s.to_string(),
                restart_count: 3,
                release_version: "v2".to_string(),
                last_start: "2026-10-16T10:30:00Z".to_string(),
                pid: Some(4242),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    pub fn event(id: &str, ty: &str, at: &str) -> EventResponse {
        EventResponse {
            id: id.to_string(),
            event_type: ty.to_string(),
            payload_json: "{}".to_string(),
            created_at: at.to_string(),
        }
    }

    pub fn log(text: &str) -> LogLine {
        LogLine {
            timestamp: "2026-10-16T11:59:00Z".to_string(),
            stream: "out".to_string(),
            line: text.to_string(),
        }
    }

    pub fn app_with_projects() -> App {
        let mut app = App::new("http://127.0.0.1:50051".into(), now());
        app.update(Msg::Projects(vec![
            project("worker", Some("Stopped")),
            project("api", Some("Running")),
            project("web", Some("Failed")),
        ]));
        app
    }

    fn release(version: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: version.to_string(),
            status: "Built".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn projects_are_sorted_and_selection_follows_the_app() {
        let mut app = app_with_projects();
        let names: Vec<_> = app.projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["api", "web", "worker"]);
        app.update(ch('j'));
        assert_eq!(app.selected_project().unwrap().name, "web");
        // A new app sorting before the selection must not move the cursor off it.
        let mut list = app.projects.clone();
        list.push(project("aardvark", None));
        app.update(Msg::Projects(list));
        assert_eq!(app.selected_project().unwrap().name, "web");
    }

    #[test]
    fn navigation_is_clamped() {
        let mut app = app_with_projects();
        app.update(key(KeyCode::Up));
        assert_eq!(app.apps_selected, 0);
        for _ in 0..10 {
            app.update(key(KeyCode::Down));
        }
        assert_eq!(app.apps_selected, 2);
        app.update(ch('g'));
        assert_eq!(app.apps_selected, 0);
    }

    #[test]
    fn key_releases_are_ignored() {
        let mut app = app_with_projects();
        let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        app.update(Msg::Key(release));
        assert!(!app.should_quit);
        app.update(ch('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn app_filter_is_live_and_escape_clears_it() {
        let mut app = app_with_projects();
        app.update(ch('/'));
        assert_eq!(app.mode, Mode::Filter(FilterTarget::Apps));
        app.update(ch('W'));
        let names: Vec<_> = app
            .filtered_projects()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(names, ["web", "worker"]);
        app.update(ch('o'));
        assert_eq!(app.filtered_projects().len(), 1);
        // Typing 'q' into the filter must not quit.
        app.update(ch('q'));
        assert!(!app.should_quit);
        app.update(key(KeyCode::Backspace));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            app.apps_filter, "Wo",
            "the filter matches case-insensitively"
        );
        app.update(key(KeyCode::Esc));
        assert_eq!(app.filtered_projects().len(), 3);
    }

    #[test]
    fn event_type_filter_supports_alternatives() {
        assert!(event_matches("DeploymentSucceeded", ""));
        assert!(event_matches("DeploymentSucceeded", "deploy"));
        assert!(!event_matches("ProcessStarted", "deploy"));
        assert!(event_matches("ProcessCrashed", "deploy, crash"));
        assert!(event_matches("ProcessCrashed", " , "));
    }

    #[test]
    fn events_are_deduplicated_sorted_and_capped() {
        let mut app = App::new(String::new(), now());
        app.update(Msg::Event(event(
            "b",
            "ProcessStarted",
            "2026-10-16T10:00:02Z",
        )));
        app.update(Msg::EventsHistory(vec![
            event("a", "DeploymentQueued", "2026-10-16T10:00:01Z"),
            event("b", "ProcessStarted", "2026-10-16T10:00:02Z"),
        ]));
        let ids: Vec<_> = app.events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        app.update(Msg::Event(event("b", "ProcessStarted", "x")));
        assert_eq!(app.events.len(), 2);
        for i in 0..MAX_EVENTS + 5 {
            app.update(Msg::Event(event(&format!("n{}", i), "Tick", "z")));
        }
        assert_eq!(app.events.len(), MAX_EVENTS);
        assert_eq!(app.event_ids.len(), MAX_EVENTS);
    }

    #[test]
    fn events_tab_filter_and_pause() {
        let mut app = App::new(String::new(), now());
        app.update(ch('2'));
        assert_eq!(app.tab, Tab::Events);
        for (i, ty) in ["DeploymentQueued", "ProcessStarted", "DeploymentSucceeded"]
            .iter()
            .enumerate()
        {
            app.update(Msg::Event(event(&i.to_string(), ty, "t")));
        }
        app.update(ch('/'));
        for c in "deploy".chars() {
            app.update(ch(c));
        }
        app.update(key(KeyCode::Enter));
        assert_eq!(app.filtered_events().len(), 2);

        app.update(ch('f'));
        assert!(!app.events_tail.follow);
        // While paused, a matching event keeps the view still; others don't move it.
        app.update(Msg::Event(event("3", "DeploymentFailed", "t")));
        assert_eq!(app.events_tail.offset, 1);
        app.update(Msg::Event(event("4", "ProcessStopped", "t")));
        assert_eq!(app.events_tail.offset, 1);
        app.update(ch('G'));
        assert!(app.events_tail.follow);
        assert_eq!(app.events_tail.offset, 0);
    }

    #[test]
    fn tail_window() {
        let mut tail = Tail::default();
        assert_eq!(tail.window(100, 10), (90, 100));
        assert_eq!(tail.window(5, 10), (0, 5));
        assert_eq!(tail.window(0, 10), (0, 0));
        tail.up(3, 100);
        assert_eq!(tail.window(100, 10), (87, 97));
        tail.top(100);
        assert_eq!(tail.window(100, 10), (0, 10));
    }

    #[test]
    fn enter_opens_detail_and_escape_closes_it() {
        let mut app = app_with_projects();
        let cmds = app.update(key(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::OpenDetail {
                project: "id-api".into()
            }]
        );
        assert_eq!(
            app.screen,
            Screen::Detail {
                project_id: "id-api".into()
            }
        );
        // Tab cycles panes inside the detail view.
        app.update(key(KeyCode::Tab));
        assert_eq!(app.detail.focus(), DetailPane::Deployments);
        app.update(key(KeyCode::Tab));
        assert_eq!(app.detail.focus(), DetailPane::Logs);
        let cmds = app.update(key(KeyCode::Esc));
        assert_eq!(cmds, vec![Command::CloseDetail]);
        assert_eq!(app.screen, Screen::List);
    }

    #[test]
    fn detail_data_for_another_app_is_ignored() {
        let mut app = app_with_projects();
        app.update(key(KeyCode::Enter));
        app.update(Msg::Detail {
            project_id: "id-web".into(),
            releases: vec![release("v1")],
            deployments: vec![],
        });
        assert!(!app.detail.loaded);
        app.update(Msg::Detail {
            project_id: "id-api".into(),
            releases: vec![release("v1"), release("v2")],
            deployments: vec![
                DeploymentInfo {
                    created_at: "2026-01-01".into(),
                    ..Default::default()
                },
                DeploymentInfo {
                    created_at: "2026-02-01".into(),
                    ..Default::default()
                },
            ],
        });
        assert!(app.detail.loaded);
        assert_eq!(app.detail.releases[0].version, "v2", "newest first");
        assert_eq!(app.detail.deployments[0].created_at, "2026-02-01");
    }

    #[test]
    fn logs_follow_pause_scroll_and_reset() {
        let mut app = app_with_projects();
        app.update(ch('l'));
        assert_eq!(app.detail.focus(), DetailPane::Logs);
        let id = "id-api".to_string();
        for i in 0..20 {
            app.update(Msg::Log {
                project_id: id.clone(),
                line: log(&format!("line {}", i)),
            });
        }
        assert!(app.detail.log_tail.follow);
        app.update(key(KeyCode::Up));
        assert!(!app.detail.log_tail.follow);
        assert_eq!(app.detail.log_tail.offset, 1);
        app.update(Msg::Log {
            project_id: id.clone(),
            line: log("line 20"),
        });
        assert_eq!(app.detail.log_tail.offset, 2, "paused view stays put");
        app.update(ch('f'));
        assert!(app.detail.log_tail.follow);
        assert_eq!(app.detail.log_tail.offset, 0);
        // Lines for another app are dropped.
        app.update(Msg::Log {
            project_id: "id-web".into(),
            line: log("other"),
        });
        assert_eq!(app.detail.logs.len(), 21);
        app.update(Msg::LogsReset { project_id: id });
        assert!(app.detail.logs.is_empty());
    }

    #[test]
    fn log_buffer_is_capped() {
        let mut app = app_with_projects();
        app.update(key(KeyCode::Enter));
        for i in 0..MAX_LOG_LINES + 10 {
            app.update(Msg::Log {
                project_id: "id-api".into(),
                line: log(&i.to_string()),
            });
        }
        assert_eq!(app.detail.logs.len(), MAX_LOG_LINES);
        assert_eq!(app.detail.logs[0].line, "10");
    }

    #[test]
    fn log_filter_in_detail() {
        let mut app = app_with_projects();
        app.update(key(KeyCode::Enter));
        for text in ["GET /health 200", "ERROR db timeout", "GET / 200"] {
            app.update(Msg::Log {
                project_id: "id-api".into(),
                line: log(text),
            });
        }
        app.update(ch('/'));
        assert_eq!(app.mode, Mode::Filter(FilterTarget::Logs));
        for c in "error".chars() {
            app.update(ch(c));
        }
        let lines: Vec<_> = app.filtered_logs().iter().map(|l| l.line.clone()).collect();
        assert_eq!(lines, ["ERROR db timeout"]);
    }

    #[test]
    fn actions_require_confirmation() {
        let mut app = app_with_projects();
        assert!(app.update(ch('R')).is_empty());
        assert!(matches!(app.mode, Mode::Confirm(_)));
        // 'n' cancels without sending anything.
        assert!(app.update(ch('n')).is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.status.as_ref().unwrap().text, "Cancelled");

        app.update(ch('R'));
        let cmds = app.update(ch('y'));
        assert_eq!(
            cmds,
            vec![Command::Control {
                project: "id-api".into(),
                name: "api".into(),
                action: ProcessCmd::Restart
            }]
        );

        app.update(ch('d'));
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            vec![Command::Deploy {
                project: "id-api".into(),
                name: "api".into()
            }]
        );

        app.update(ch('x'));
        assert_eq!(
            app.update(ch('y')),
            vec![Command::Control {
                project: "id-api".into(),
                name: "api".into(),
                action: ProcessCmd::Stop
            }]
        );
    }

    #[test]
    fn start_on_a_running_app_is_a_no_op() {
        let mut app = app_with_projects();
        app.update(ch('s'));
        assert_eq!(app.mode, Mode::Normal);
        app.update(ch('j')); // web (Failed)
        app.update(ch('s'));
        assert!(matches!(app.mode, Mode::Confirm(_)));
    }

    #[test]
    fn rollback_targets_previous_or_selected_release() {
        let mut app = app_with_projects();
        app.update(ch('b'));
        assert_eq!(
            app.update(ch('y')),
            vec![Command::Rollback {
                project: "id-api".into(),
                name: "api".into(),
                version: None
            }]
        );

        app.update(key(KeyCode::Enter));
        app.update(Msg::Detail {
            project_id: "id-api".into(),
            releases: vec![release("v1"), release("v2")],
            deployments: vec![],
        });
        // The live release (v2) is selected: rolling back means "previous".
        app.update(ch('b'));
        assert_eq!(
            app.update(ch('y')),
            vec![Command::Rollback {
                project: "id-api".into(),
                name: "api".into(),
                version: None
            }]
        );
        app.update(ch('j'));
        app.update(ch('b'));
        match &app.mode {
            Mode::Confirm(p) => assert!(p.prompt.contains("release v1"), "{}", p.prompt),
            other => panic!("expected a confirmation, got {:?}", other),
        }
        assert_eq!(
            app.update(ch('y')),
            vec![Command::Rollback {
                project: "id-api".into(),
                name: "api".into(),
                version: Some("v1".into())
            }]
        );
    }

    #[test]
    fn help_overlay_swallows_keys() {
        let mut app = app_with_projects();
        app.update(ch('?'));
        assert_eq!(app.mode, Mode::Help);
        app.update(ch('j'));
        assert_eq!(app.apps_selected, 0);
        app.update(ch('q'));
        assert_eq!(app.mode, Mode::Normal);
        assert!(!app.should_quit);
    }

    #[test]
    fn connection_and_results_update_status() {
        let mut app = App::new(String::new(), now());
        assert_eq!(app.connection, Connection::Connecting);
        app.update(Msg::Disconnected {
            error: "refused".into(),
            retry_secs: 4,
        });
        assert!(matches!(app.connection, Connection::Disconnected { .. }));
        app.update(Msg::Connected {
            version: "0.6.0".into(),
        });
        assert_eq!(
            app.connection,
            Connection::Connected {
                version: "0.6.0".into()
            }
        );
        app.update(Msg::ActionDone {
            ok: false,
            message: "boom".into(),
        });
        assert_eq!(app.status.as_ref().unwrap().kind, StatusKind::Error);
        assert_eq!(app.update(ch('r')), vec![Command::Refresh]);
    }

    #[test]
    fn uptime_only_for_running_processes() {
        let p = project("api", Some("Running"));
        let up = uptime(p.process.as_ref().unwrap(), now()).unwrap();
        assert_eq!(up.num_minutes(), 90);
        let p = project("api", Some("Stopped"));
        assert!(uptime(p.process.as_ref().unwrap(), now()).is_none());
    }

    #[test]
    fn number_keys_switch_tabs_and_leave_detail() {
        let mut app = app_with_projects();
        app.update(key(KeyCode::Enter));
        assert_eq!(app.update(ch('2')), vec![Command::CloseDetail]);
        assert_eq!(app.screen, Screen::List);
        assert_eq!(app.tab, Tab::Events);
        app.update(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Apps);
    }
}
