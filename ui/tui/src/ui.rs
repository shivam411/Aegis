//! Rendering: draws an [`App`] into a frame. No state changes happen here.

use crate::app::{
    uptime, App, Connection, DetailPane, FilterTarget, Mode, Screen, StatusKind, Tab, Tail,
};
use crate::theme::{self, ACCENT, ERROR, MUTED, SUCCESS, WARNING};
use aegis_api::aegis::{EventResponse, LogLine, ProjectInfo};
use chrono::{DateTime, Local};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;

pub fn render(frame: &mut Frame, app: &App) {
    let banner = !matches!(app.connection, Connection::Connected { .. });
    let [header, banner_area, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(banner)),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    render_header(frame, header, app);
    if banner {
        render_banner(frame, banner_area, app);
    }
    match &app.screen {
        Screen::List => match app.tab {
            Tab::Apps => render_apps(frame, body, app),
            Tab::Events => {
                let events = app.filtered_events();
                let title = filter_title("Events", &app.events_filter, events.len());
                render_events(frame, body, &events, &app.events_tail, title, true);
            }
        },
        Screen::Detail { .. } => render_detail(frame, body, app),
    }
    render_footer(frame, footer, app);

    match &app.mode {
        Mode::Help => render_help(frame),
        Mode::Confirm(pending) => render_confirm(frame, &pending.prompt),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

pub fn fmt_duration(d: chrono::Duration) -> String {
    let s = d.num_seconds().max(0);
    let (days, hours, mins, secs) = (s / 86_400, s / 3600 % 24, s / 60 % 60, s % 60);
    if days > 0 {
        format!("{}d {}h", days, hours)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// Local wall-clock time of an RFC 3339 timestamp, with a fallback for other formats.
fn fmt_time(ts: &str, format: &str) -> String {
    DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&Local).format(format).to_string())
        .unwrap_or_else(|_| ts.chars().take(19).collect())
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn filter_title(name: &str, filter: &str, count: usize) -> String {
    if filter.is_empty() {
        format!(" {} ({}) ", name, count)
    } else {
        format!(" {} ({}) - filter: {} ", name, count, filter)
    }
}

fn block(title: impl Into<String>, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border(focused))
        .title(Span::styled(title.into(), theme::title()))
}

fn status_span(status: &str) -> Span<'static> {
    Span::styled(
        status.to_string(),
        Style::default()
            .fg(theme::status_color(status))
            .add_modifier(Modifier::BOLD),
    )
}

fn app_status(p: &ProjectInfo) -> &str {
    p.process
        .as_ref()
        .map(|pr| pr.status.as_str())
        .unwrap_or("Not deployed")
}

fn cpu_text(p: &ProjectInfo) -> String {
    p.usage
        .as_ref()
        .map(|u| format!("{:.0}%", u.cpu_cores * 100.0))
        .unwrap_or_else(|| "-".into())
}

fn mem_text(p: &ProjectInfo) -> String {
    match &p.usage {
        Some(u) => match u.memory_limit_bytes {
            Some(limit) => format!("{} / {}", fmt_bytes(u.memory_bytes), fmt_bytes(limit)),
            None => fmt_bytes(u.memory_bytes),
        },
        None => "-".into(),
    }
}

fn uptime_text(p: &ProjectInfo, app: &App) -> String {
    p.process
        .as_ref()
        .and_then(|pr| uptime(pr, app.now))
        .map(fmt_duration)
        .unwrap_or_else(|| "-".into())
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

// ---------------------------------------------------------------------------
// Chrome
// ---------------------------------------------------------------------------

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let tab = |n: &str, label: &str, active: bool| {
        let style = if active {
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::muted()
        };
        Span::styled(format!(" [{}] {} ", n, label), style)
    };
    let on_list = app.screen == Screen::List;
    let mut spans = vec![
        Span::styled(" Aegis ", theme::title()),
        tab("1", "Apps", app.tab == Tab::Apps || !on_list),
        Span::raw(" "),
        tab("2", "Events", app.tab == Tab::Events && on_list),
    ];
    if let Some(p) = app.detail_project() {
        spans.push(Span::styled(format!("  > {}", p.name), theme::title()));
    }
    let left = Paragraph::new(Line::from(spans));

    let (dot, text) = match &app.connection {
        Connection::Connected { version } => {
            (SUCCESS, format!("daemon v{} @ {}", version, app.addr))
        }
        Connection::Connecting => (WARNING, format!("connecting to {}", app.addr)),
        Connection::Disconnected { .. } => (ERROR, format!("offline: {}", app.addr)),
    };
    let right = Paragraph::new(Line::from(vec![
        Span::styled("* ", Style::default().fg(dot)),
        Span::styled(text, theme::muted()),
        Span::raw(" "),
    ]))
    .alignment(Alignment::Right);

    let [l, r] = Layout::horizontal([Constraint::Min(30), Constraint::Max(60)]).areas(area);
    frame.render_widget(left, l);
    frame.render_widget(right, r);
}

fn render_banner(frame: &mut Frame, area: Rect, app: &App) {
    let (bg, text) = match &app.connection {
        Connection::Disconnected { error, retry_secs } => (
            ERROR,
            format!(
                " DISCONNECTED - cannot reach the daemon at {} ({}). Retrying in {}s; press r to retry now. ",
                app.addr, error, retry_secs
            ),
        ),
        _ => (WARNING, format!(" Connecting to the daemon at {}... ", app.addr)),
    };
    frame.render_widget(
        Paragraph::new(text).style(
            Style::default()
                .fg(Color::Black)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

fn hints(app: &App) -> &'static str {
    match (&app.mode, &app.screen, app.tab) {
        (Mode::Filter(_), _, _) => "type to filter  Enter keep  Esc clear",
        (Mode::Confirm(_), _, _) => "y/Enter confirm  n/Esc cancel",
        (Mode::Help, _, _) => "Esc/? close help",
        (_, Screen::Detail { .. }, _) => {
            "Tab pane  j/k move  f follow  PgUp/PgDn scroll  / filter logs  s/x/R start/stop/restart  d deploy  b rollback  Esc back  ? help"
        }
        (_, Screen::List, Tab::Apps) => {
            "j/k move  Enter open  l logs  / filter  s/x/R start/stop/restart  d deploy  b rollback  Tab events  r refresh  ? help  q quit"
        }
        (_, Screen::List, Tab::Events) => {
            "j/k scroll  f follow  / filter types (a,b)  g/G top/bottom  Tab apps  ? help  q quit"
        }
    }
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let [status_area, hint_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let status = if let Mode::Filter(target) = app.mode {
        let value = match target {
            FilterTarget::Apps => &app.apps_filter,
            FilterTarget::Events => &app.events_filter,
            FilterTarget::Logs => &app.detail.log_filter,
        };
        Line::from(vec![
            Span::styled(" / ", theme::title()),
            Span::raw(value.clone()),
            Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
        ])
    } else if let Some(s) = &app.status {
        let color = match s.kind {
            StatusKind::Info => Color::Reset,
            StatusKind::Ok => SUCCESS,
            StatusKind::Error => ERROR,
        };
        Line::from(Span::styled(
            format!(" {}", s.text),
            Style::default().fg(color),
        ))
    } else {
        Line::from("")
    };
    frame.render_widget(Paragraph::new(status), status_area);
    frame.render_widget(
        Paragraph::new(Span::styled(format!(" {}", hints(app)), theme::muted())),
        hint_area,
    );
}

// ---------------------------------------------------------------------------
// Apps
// ---------------------------------------------------------------------------

fn render_apps(frame: &mut Frame, area: Rect, app: &App) {
    // Show a strip of recent events under the table when there is room.
    let (table_area, events_area) = if area.height >= 20 {
        let [t, e] = Layout::vertical([Constraint::Min(8), Constraint::Length(9)]).areas(area);
        (t, Some(e))
    } else {
        (area, None)
    };

    let projects = app.filtered_projects();
    let title = filter_title("Apps", &app.apps_filter, projects.len());
    if projects.is_empty() {
        let text = if app.projects.is_empty() {
            "No apps yet. Register one with 'aegis init' and deploy it with 'aegis deploy'."
        } else {
            "No apps match the filter (Esc clears it)."
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(theme::muted())
                .block(block(title, true)),
            table_area,
        );
    } else {
        let header = Row::new(
            [
                "NAME", "STATUS", "RELEASE", "UPTIME", "RESTARTS", "CPU", "MEMORY", "PID",
            ]
            .map(|h| Cell::from(Span::styled(h, theme::muted()))),
        );
        let rows = projects.iter().map(|p| {
            let pid = p
                .process
                .as_ref()
                .and_then(|pr| pr.pid)
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".into());
            let restarts = p
                .process
                .as_ref()
                .map(|pr| pr.restart_count.to_string())
                .unwrap_or_else(|| "-".into());
            let mut release = if p.current_release.is_empty() {
                "-".to_string()
            } else {
                p.current_release.clone()
            };
            if p.instances.len() > 1 {
                release = format!("{} ({}x)", release, p.instances.len());
            }
            Row::new(vec![
                Cell::from(Span::styled(
                    p.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Cell::from(status_span(app_status(p))),
                Cell::from(release),
                Cell::from(uptime_text(p, app)),
                Cell::from(restarts),
                Cell::from(cpu_text(p)),
                Cell::from(mem_text(p)),
                Cell::from(pid),
            ])
        });
        let widths = [
            Constraint::Min(14),
            Constraint::Length(13),
            Constraint::Length(20),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Length(20),
            Constraint::Length(8),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .block(block(title, true))
            .row_highlight_style(theme::selected())
            .highlight_symbol("> ");
        let mut state = TableState::default().with_selected(Some(app.apps_selected));
        frame.render_stateful_widget(table, table_area, &mut state);
    }

    if let Some(events_area) = events_area {
        let events: Vec<&EventResponse> = app.events.iter().collect();
        render_events(
            frame,
            events_area,
            &events,
            &Tail::default(),
            " Live events (2 for all) ".into(),
            false,
        );
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// One-line summary of an event payload: the top-level scalar fields as
/// `key=value`, falling back to the raw text.
fn summarize_payload(payload: &str) -> String {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(payload) else {
        return payload.to_string();
    };
    let pairs: Vec<String> = fields
        .iter()
        .filter_map(|(k, v)| match v {
            serde_json::Value::String(s) if !s.is_empty() => Some(format!("{}={}", k, s)),
            serde_json::Value::Number(n) => Some(format!("{}={}", k, n)),
            serde_json::Value::Bool(b) => Some(format!("{}={}", k, b)),
            _ => None,
        })
        .collect();
    if pairs.is_empty() {
        payload.to_string()
    } else {
        pairs.join(" ")
    }
}

fn render_events(
    frame: &mut Frame,
    area: Rect,
    events: &[&EventResponse],
    tail: &Tail,
    title: String,
    focused: bool,
) {
    let height = area.height.saturating_sub(2) as usize;
    let (start, end) = tail.window(events.len(), height);
    let lines: Vec<Line> = events[start..end]
        .iter()
        .map(|e| {
            Line::from(vec![
                Span::styled(fmt_time(&e.created_at, "%H:%M:%S "), theme::muted()),
                Span::styled(
                    format!("{:<24} ", e.event_type),
                    Style::default()
                        .fg(theme::event_color(&e.event_type))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(summarize_payload(&e.payload_json)),
            ])
        })
        .collect();
    let mode = if tail.follow {
        "following"
    } else {
        "paused - f to follow"
    };
    let title = format!("{}[{}] ", title, mode);
    let body = if lines.is_empty() {
        Paragraph::new(Span::styled("Waiting for events...", theme::muted()))
    } else {
        Paragraph::new(lines)
    };
    frame.render_widget(body.block(block(title, focused)), area);
}

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let info_height = if app
        .detail_project()
        .is_some_and(|p| p.instances.len() > 1 || !p.standby_version.is_empty())
    {
        6
    } else {
        5
    };
    let [info_area, rest] =
        Layout::vertical([Constraint::Length(info_height), Constraint::Min(6)]).areas(area);
    render_info(frame, info_area, app);

    let focus = app.detail.focus();
    let (lists, logs_area) = if rest.width >= 110 {
        let [l, r] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .areas(rest);
        (l, r)
    } else {
        let [l, r] =
            Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(rest);
        (l, r)
    };
    let direction = if rest.width >= 110 {
        Direction::Vertical
    } else {
        Direction::Horizontal
    };
    let [rel_area, dep_area] = Layout::default()
        .direction(direction)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .areas(lists);

    render_releases(frame, rel_area, app, focus == DetailPane::Releases);
    render_deployments(frame, dep_area, app, focus == DetailPane::Deployments);
    render_logs(frame, logs_area, app, focus == DetailPane::Logs);
}

fn render_info(frame: &mut Frame, area: Rect, app: &App) {
    let Some(p) = app.detail_project() else {
        frame.render_widget(
            Paragraph::new("This app no longer exists. Press Esc to go back.")
                .block(block(" App ", false)),
            area,
        );
        return;
    };
    let label = |s: &str| Span::styled(format!("{}: ", s), theme::muted());
    let process = p.process.as_ref();
    let mut status = vec![label("Status"), status_span(app_status(p))];
    if let Some(pid) = process.and_then(|pr| pr.pid) {
        status.push(Span::styled(format!(" (pid {})", pid), theme::muted()));
    }
    status.extend([
        Span::raw("   "),
        label("Release"),
        Span::raw(if p.current_release.is_empty() {
            "-".to_string()
        } else {
            p.current_release.clone()
        }),
        Span::raw("   "),
        label("Uptime"),
        Span::raw(uptime_text(p, app)),
        Span::raw("   "),
        label("Restarts"),
        Span::raw(
            process
                .map(|pr| pr.restart_count.to_string())
                .unwrap_or_else(|| "-".into()),
        ),
    ]);
    let source = if !p.source_dir.is_empty() {
        p.source_dir.clone()
    } else if !p.repository_url.is_empty() {
        format!("{} ({})", p.repository_url, p.branch)
    } else {
        "-".into()
    };
    let resources = vec![
        label("CPU"),
        Span::raw(cpu_text(p)),
        Span::raw("   "),
        label("Memory"),
        Span::raw(mem_text(p)),
        Span::raw("   "),
        label("Runtime"),
        Span::raw(if p.runtime.is_empty() {
            "-".to_string()
        } else {
            p.runtime.clone()
        }),
        Span::raw("   "),
        label("Source"),
        Span::raw(source),
    ];
    let mut process_line = vec![label("Command")];
    match process {
        Some(pr) => {
            process_line.push(Span::raw(if pr.command.is_empty() {
                "-".to_string()
            } else {
                pr.command.clone()
            }));
            if let Some(code) = pr.last_exit_code {
                process_line.push(Span::raw("   "));
                process_line.push(label("Last exit"));
                process_line.push(Span::raw(code.to_string()));
            }
        }
        None => process_line.push(Span::styled(
            "not deployed yet (d to deploy)",
            theme::muted(),
        )),
    }
    let mut lines = vec![
        Line::from(status),
        Line::from(resources),
        Line::from(process_line),
    ];
    if p.instances.len() > 1 || !p.standby_version.is_empty() {
        lines.push(Line::from(instances_line(p)));
    }
    frame.render_widget(
        Paragraph::new(lines).block(block(format!(" {} ", p.name), false)),
        area,
    );
}

/// "Instances: #1 Running :41001  #2 Backoff ..." plus a BlueGreen standby.
fn instances_line(p: &ProjectInfo) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled("Instances: ", theme::muted())];
    for (i, instance) in p.instances.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::raw(format!(
            "#{} ",
            instance.instance.unwrap_or(i as u32 + 1)
        )));
        spans.push(status_span(&instance.status));
        if let Some(port) = instance.port {
            spans.push(Span::styled(format!(" :{}", port), theme::muted()));
        }
    }
    if !p.standby_version.is_empty() {
        spans.push(Span::styled(
            format!(
                "   standby {} (instant rollback until {})",
                p.standby_version, p.standby_until
            ),
            theme::muted(),
        ));
    }
    spans
}

fn render_releases(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let d = &app.detail;
    let live = app
        .detail_project()
        .map(|p| p.current_release.clone())
        .unwrap_or_default();
    let title = format!(" Releases ({}) ", d.releases.len());
    if d.releases.is_empty() {
        let text = if d.loaded {
            "No releases yet."
        } else {
            "Loading..."
        };
        frame.render_widget(
            Paragraph::new(Span::styled(text, theme::muted())).block(block(title, focused)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = d
        .releases
        .iter()
        .map(|r| {
            let mut spans = vec![
                Span::styled(
                    format!("{:<18} ", r.version),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                status_span(&r.status),
            ];
            if r.version == live {
                spans.push(Span::styled(" (live)", Style::default().fg(SUCCESS)));
            }
            if !r.commit_sha.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", short_id(&r.commit_sha)),
                    theme::muted(),
                ));
            }
            if !r.commit_message.is_empty() {
                spans.push(Span::raw(format!(
                    "  {}",
                    truncate(r.commit_message.lines().next().unwrap_or(""), 40)
                )));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block(title, focused))
        .highlight_style(if focused {
            theme::selected()
        } else {
            Style::default()
        })
        .highlight_symbol("> ");
    let mut state = ListState::default().with_selected(Some(d.release_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_deployments(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let d = &app.detail;
    let title = format!(" Deployments ({}) ", d.deployments.len());
    if d.deployments.is_empty() {
        let text = if d.loaded {
            "No deployments yet."
        } else {
            "Loading..."
        };
        frame.render_widget(
            Paragraph::new(Span::styled(text, theme::muted())).block(block(title, focused)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = d
        .deployments
        .iter()
        .map(|dep| {
            let mut spans = vec![
                Span::styled(fmt_time(&dep.created_at, "%m-%d %H:%M "), theme::muted()),
                status_span(&dep.status),
                Span::raw(format!(
                    " {} ",
                    if dep.version.is_empty() {
                        "-"
                    } else {
                        &dep.version
                    }
                )),
            ];
            if !dep.stage.is_empty() && dep.status == "InProgress" {
                spans.push(Span::styled(
                    format!("[{}] ", dep.stage),
                    Style::default().fg(WARNING),
                ));
            }
            if !dep.trigger.is_empty() {
                spans.push(Span::styled(
                    format!("via {} ", dep.trigger),
                    theme::muted(),
                ));
            }
            if !dep.error.is_empty() {
                spans.push(Span::styled(
                    truncate(&dep.error, 60),
                    Style::default().fg(ERROR),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(block(title, focused))
        .highlight_style(if focused {
            theme::selected()
        } else {
            Style::default()
        })
        .highlight_symbol("> ");
    let mut state = ListState::default().with_selected(Some(d.deployment_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn log_line(l: &LogLine) -> Line<'static> {
    let style = match l.stream.as_str() {
        "err" => Style::default().fg(ERROR),
        "sys" => Style::default().fg(WARNING),
        _ => Style::default(),
    };
    Line::from(vec![
        Span::styled(
            fmt_time(&l.timestamp, "%H:%M:%S "),
            Style::default().fg(MUTED),
        ),
        Span::styled(l.line.clone(), style),
    ])
}

fn render_logs(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let d = &app.detail;
    let lines = app.filtered_logs();
    let height = area.height.saturating_sub(2) as usize;
    let (start, end) = d.log_tail.window(lines.len(), height);
    let mode = if d.log_tail.follow {
        "following".to_string()
    } else {
        format!("paused, {} newer - f to follow", lines.len() - end)
    };
    let title = format!(
        "{}[{}] ",
        filter_title("Logs", &d.log_filter, lines.len()),
        mode
    );
    let body = if lines.is_empty() {
        let note = d
            .log_note
            .clone()
            .unwrap_or_else(|| "Waiting for output...".into());
        Paragraph::new(Span::styled(note, theme::muted())).wrap(Wrap { trim: true })
    } else {
        let mut rendered: Vec<Line> = lines[start..end].iter().map(|l| log_line(l)).collect();
        if let Some(note) = &d.log_note {
            if end == lines.len() && rendered.len() >= height && !rendered.is_empty() {
                rendered.remove(0);
            }
            if end == lines.len() {
                rendered.push(Line::from(Span::styled(
                    format!("-- {} --", note),
                    Style::default().fg(WARNING),
                )));
            }
        }
        Paragraph::new(rendered)
    };
    frame.render_widget(body.block(block(title, focused)), area);
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

pub const HELP: &[(&str, &str)] = &[
    (
        "Tab / Shift+Tab",
        "Switch tab (Apps/Events) or pane in app detail",
    ),
    ("1 / 2", "Apps / Events tab"),
    ("Up/Down, j/k", "Move selection or scroll"),
    ("PgUp/PgDn, g/G", "Page, jump to top / bottom"),
    ("Enter", "Open the selected app"),
    ("l", "Open the selected app's logs"),
    ("Esc", "Back / clear filter / close dialog"),
    ("/", "Filter: app names, event types (a,b), log text"),
    ("f", "Follow / pause logs and events"),
    ("r", "Refresh now (or reconnect)"),
    ("s / x / R", "Start / stop / restart the app"),
    ("d", "Deploy the app from its configured source"),
    ("b", "Roll back (to the selected release in detail)"),
    ("?", "Toggle this help"),
    ("q, Ctrl+C", "Quit"),
];

fn render_help(frame: &mut Frame) {
    let area = centered(frame.area(), 72, HELP.len() as u16 + 4);
    let rows = HELP.iter().map(|(k, v)| {
        Row::new(vec![
            Cell::from(Span::styled(*k, theme::title())),
            Cell::from(*v),
        ])
    });
    let table = Table::new(rows, [Constraint::Length(18), Constraint::Min(20)])
        .block(block(" Keys - ? or Esc to close ", true));
    frame.render_widget(Clear, area);
    frame.render_widget(table, area);
}

fn render_confirm(frame: &mut Frame, prompt: &str) {
    let width = (prompt.chars().count() as u16 + 8).clamp(40, 90);
    let area = centered(frame.area(), width, 5);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("{}?", prompt),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled("[y] confirm   [n] cancel", theme::muted())),
        ])
        .alignment(Alignment::Center)
        .block(block(" Confirm ", true).border_style(Style::default().fg(WARNING))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{app_with_projects, ch, event, key, log};
    use crate::app::Msg;
    use aegis_api::aegis::{ReleaseInfo, ResourceUsage};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use ratatui::Terminal;

    fn draw(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn apps_screen_shows_the_table() {
        let mut app = app_with_projects();
        app.update(Msg::Connected {
            version: "0.6.0".into(),
        });
        let mut projects = app.projects.clone();
        projects[0].usage = Some(ResourceUsage {
            cpu_cores: 0.25,
            memory_bytes: 120 * 1024 * 1024,
            ..Default::default()
        });
        app.update(Msg::Projects(projects));
        app.update(Msg::Event(event(
            "1",
            "DeploymentSucceeded",
            "2026-10-16T10:00:00Z",
        )));
        let screen = draw(&app, 140, 30);
        for expected in [
            "Aegis",
            "[1] Apps",
            "daemon v0.6.0",
            "Apps (3)",
            "NAME",
            "STATUS",
            "UPTIME",
            "api",
            "Running",
            "1h 30m",
            "25%",
            "120.0 MiB",
            "4242",
            "web",
            "Failed",
            "worker",
            "Stopped",
            "Live events",
            "DeploymentSucceeded",
        ] {
            assert!(
                screen.contains(expected),
                "missing {:?} in:\n{}",
                expected,
                screen
            );
        }
        assert!(!screen.contains("DISCONNECTED"));
    }

    #[test]
    fn disconnected_banner_is_shown() {
        let mut app = app_with_projects();
        app.update(Msg::Disconnected {
            error: "connection refused".into(),
            retry_secs: 8,
        });
        let screen = draw(&app, 160, 24);
        assert!(screen.contains("DISCONNECTED"), "{}", screen);
        assert!(screen.contains("connection refused"));
        assert!(screen.contains("Retrying in 8s"));
    }

    #[test]
    fn empty_state_explains_what_to_do() {
        let app = App::new("http://127.0.0.1:50051".into(), crate::app::tests::now());
        let screen = draw(&app, 120, 24);
        assert!(screen.contains("No apps yet"), "{}", screen);
        assert!(screen.contains("Connecting to the daemon"));
    }

    #[test]
    fn detail_screen_shows_releases_deployments_and_logs() {
        let mut app = app_with_projects();
        app.update(key(KeyCode::Enter));
        app.update(Msg::Detail {
            project_id: "id-api".into(),
            releases: vec![
                ReleaseInfo {
                    version: "v1".into(),
                    status: "Inactive".into(),
                    ..Default::default()
                },
                ReleaseInfo {
                    version: "v2".into(),
                    status: "Active".into(),
                    commit_message: "Fix login".into(),
                    ..Default::default()
                },
            ],
            deployments: vec![aegis_api::aegis::DeploymentInfo {
                status: "Success".into(),
                version: "v2".into(),
                trigger: "cli".into(),
                created_at: "2026-10-16T10:29:00Z".into(),
                ..Default::default()
            }],
        });
        for i in 0..5 {
            app.update(Msg::Log {
                project_id: "id-api".into(),
                line: log(&format!("listening on :{}", 3000 + i)),
            });
        }
        let screen = draw(&app, 140, 30);
        for expected in [
            "> api",
            "Releases (2)",
            "(live)",
            "Fix login",
            "Deployments (1)",
            "Success",
            "via cli",
            "Logs (5)",
            "following",
            "listening on :3004",
            "Restarts",
        ] {
            assert!(
                screen.contains(expected),
                "missing {:?} in:\n{}",
                expected,
                screen
            );
        }

        // Pausing changes the title; the narrow layout still shows every pane.
        app.update(ch('f'));
        let screen = draw(&app, 90, 40);
        assert!(screen.contains("paused"), "{}", screen);
        assert!(screen.contains("Releases (2)"));
        assert!(screen.contains("Deployments (1)"));
    }

    #[test]
    fn detail_screen_lists_instances() {
        let mut app = app_with_projects();
        let mut projects = app.projects.clone();
        let instance = |n: u32, status: &str| aegis_api::aegis::ProcessInfo {
            id: format!("proc-{}", n),
            status: status.into(),
            instance: Some(n),
            port: Some(41000 + n),
            ..Default::default()
        };
        projects[0].instances = vec![
            instance(1, "Running"),
            instance(2, "Running"),
            instance(3, "Backoff"),
        ];
        projects[0].standby_version = "v1".into();
        let name = projects[0].name.clone();
        app.update(Msg::Projects(projects));
        let screen = draw(&app, 140, 30);
        assert!(screen.contains("(3x)"), "{}", screen);
        let index = app
            .filtered_projects()
            .iter()
            .position(|p| p.name == name)
            .unwrap();
        for _ in 0..index {
            app.update(key(KeyCode::Down));
        }
        app.update(key(KeyCode::Enter));
        let screen = draw(&app, 140, 30);
        for expected in ["Instances:", "#1", "#3", ":41002", "Backoff", "standby v1"] {
            assert!(
                screen.contains(expected),
                "missing {:?} in:\n{}",
                expected,
                screen
            );
        }
    }

    #[test]
    fn help_and_confirm_overlays() {
        let mut app = app_with_projects();
        app.update(ch('?'));
        let screen = draw(&app, 120, 30);
        assert!(screen.contains("Keys"), "{}", screen);
        assert!(screen.contains("Roll back"));
        app.update(key(KeyCode::Esc));
        app.update(ch('R'));
        let screen = draw(&app, 120, 30);
        assert!(screen.contains("Restart api?"), "{}", screen);
        assert!(screen.contains("[y] confirm"));
    }

    #[test]
    fn events_tab_shows_the_filter() {
        let mut app = app_with_projects();
        app.update(ch('2'));
        app.update(Msg::Event(event(
            "1",
            "ProcessCrashed",
            "2026-10-16T10:00:00Z",
        )));
        app.update(Msg::Event(event(
            "2",
            "DeploymentQueued",
            "2026-10-16T10:00:01Z",
        )));
        app.update(ch('/'));
        app.update(ch('c'));
        app.update(ch('r'));
        let screen = draw(&app, 120, 20);
        assert!(screen.contains("filter: cr"), "{}", screen);
        assert!(screen.contains("ProcessCrashed"));
        assert!(!screen.contains("DeploymentQueued"));
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let mut app = app_with_projects();
        for (w, h) in [(1, 1), (10, 3), (30, 8)] {
            draw(&app, w, h);
        }
        app.update(key(KeyCode::Enter));
        app.update(ch('?'));
        for (w, h) in [(1, 1), (10, 3), (30, 8)] {
            draw(&app, w, h);
        }
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_duration(chrono::Duration::seconds(5)), "5s");
        assert_eq!(fmt_duration(chrono::Duration::seconds(125)), "2m 5s");
        assert_eq!(fmt_duration(chrono::Duration::seconds(7300)), "2h 1m");
        assert_eq!(fmt_duration(chrono::Duration::seconds(90_000)), "1d 1h");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KiB");
        assert_eq!(
            summarize_payload(r#"{"project_id":"abc","version":"v2"}"#),
            "project_id=abc version=v2"
        );
        assert_eq!(summarize_payload(r#"{"a":{"b":1}}"#), r#"{"a":{"b":1}}"#);
        assert_eq!(summarize_payload("not json"), "not json");
    }
}
