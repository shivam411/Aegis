//! Colours from docs/design/tui_design_system.md.
//!
//! The background is left to the terminal so light and dark themes both work.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(0x5F, 0x87, 0xFF);
pub const SUCCESS: Color = Color::Rgb(0x5F, 0xF7, 0xA6);
pub const WARNING: Color = Color::Rgb(0xFF, 0xD7, 0x5F);
pub const ERROR: Color = Color::Rgb(0xFF, 0x5F, 0x87);
pub const MUTED: Color = Color::Rgb(0x80, 0x80, 0x80);

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn border(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        muted()
    }
}

pub fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// Colour for a process, release or deployment status.
pub fn status_color(status: &str) -> Color {
    match status {
        "Running" | "Success" | "Active" | "Healthy" => SUCCESS,
        "Queued" | "InProgress" | "Building" | "Backoff" | "Built" | "Starting" => WARNING,
        "Failed" | "Crashed" | "Unhealthy" | "RolledBack" | "Exited" => ERROR,
        _ => MUTED,
    }
}

/// Colour for an event type, judged by its name.
pub fn event_color(event_type: &str) -> Color {
    let t = event_type.to_lowercase();
    if ["fail", "crash", "unhealthy", "error", "oom", "denied"]
        .iter()
        .any(|w| t.contains(w))
    {
        ERROR
    } else if [
        "succeeded",
        "started",
        "completed",
        "healthy",
        "live",
        "activated",
    ]
    .iter()
    .any(|w| t.contains(w))
    {
        SUCCESS
    } else if ["queued", "rollback", "backoff", "stopp", "restart"]
        .iter()
        .any(|w| t.contains(w))
    {
        WARNING
    } else {
        Color::Reset
    }
}
