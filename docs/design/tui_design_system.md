# Aegis TUI Visual Design System & Component Guidelines

This document specifies the design language, color palette, typography, visual hierarchy, and keyboard navigation rules for the Aegis Terminal UI (`aegis-tui`).

---

## 1. Visual Aesthetics & Philosophy

The Aegis TUI is built using `ratatui` + `crossterm`. It adheres to modern terminal aesthetic standards:
- **Clean Grid Alignment**: Fixed padding, zero visual jitter, border block framing.
- **High-Contrast Dark Theme**: Deep black/charcoal backgrounds with vibrant HSL-tailored accent colors.
- **Informative Status Indicators**: Color-coded badges for project health and deployment states.

---

## 2. Color Palette (ANSI / 256-Color Mapping)

| Design Token | Hex / Color Code | Usage |
| :--- | :--- | :--- |
| **`Primary Accent`** | `#5F87FF` (Royal Blue) | Main title, active tab, focused border |
| **`Success`** | `#5FF7A6` (Emerald Green) | `Running`, `Healthy`, `DeploymentSucceeded` |
| **`Warning`** | `#FFD75F` (Amber Yellow) | `Queued`, `Building`, `RollbackRequired` |
| **`Error`** | `#FF5F87` (Crimson Red) | `Crashed`, `DeploymentFailed`, `Unhealthy` |
| **`Muted / Subtitle`** | `#808080` (Medium Gray) | Metadata labels, line numbers, borders |
| **`Background`** | `#121212` (Dark Charcoal) | Default background panel fill |

---

## 3. Keyboard Navigation Matrix

As implemented in `ui/tui` (the in-app `?` overlay is the source of truth):

| Key Combo | Action Scope | Function |
| :--- | :--- | :--- |
| `Tab` / `Shift+Tab` | Global | Switch tabs (`Apps`, `Events`); in an app, cycle panes (`Releases`, `Deployments`, `Logs`) |
| `1` / `2` | Global | Jump to the `Apps` / `Events` tab |
| `j` / `k` or `↓` / `↑` | List / Table | Move the selection, or scroll a log/event pane |
| `PgUp` / `PgDn`, `g` / `G` | Log / Events | Page; jump to top / bottom (bottom resumes following) |
| `Enter` | Apps | Open the app view |
| `l` | Apps | Open the app view on its live log |
| `/` | Global | Filter app names, event types (`a,b` = either) or log text |
| `f` | Log / Events | Follow or pause the tail |
| `r` | Global | Refresh now (reconnect now while offline) |
| `s` / `x` / `R` | App | Start / stop / restart, after confirmation |
| `d` | App | Deploy from the app's configured source, after confirmation |
| `b` | App | Roll back to the previous release, or to the release selected in `Releases` |
| `?` | Global | Help overlay |
| `Esc` | Global | Back, clear the filter, or close a dialog |
| `q` / `Ctrl+C` | Global | Quit |

`r` refreshes rather than restarts (restart is `R`), so a stray key never touches a running app; every action asks `y`/`n` first.

---

## 4. Component Layout Grid

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ Aegis 🛡️ v0.2.0-arch-complete [1] Projects  [2] Deployments  [3] Logs  [4] System │
├──────────────────────────────────────┬───────────────────────────────────────┤
│ Active Projects                      │ Project Detail: backend-api           │
│                                      │                                       │
│ ► backend-api     [Healthy] (PID 1001)│ Status:      Running (PID 1001)       │
│   frontend-web    [Healthy] (PID 1002)│ Active Release: v1.4.2 (a1b2c3d4)     │
│   payment-service [Stopped]           │ Strategy:    GracefulSwitch          │
│                                      │ Health Check: HTTP 200 (OK)           │
├──────────────────────────────────────┴───────────────────────────────────────┤
│ Live Event Stream                                                            │
│ [08:00:12] DeploymentSucceeded (backend-api -> v1.4.2)                       │
│ [08:00:15] ProcessStarted (PID 1001)                                         │
└──────────────────────────────────────────────────────────────────────────────┘
```
