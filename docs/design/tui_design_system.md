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

| Key Combo | Action Scope | Function |
| :--- | :--- | :--- |
| `Tab` / `Shift+Tab` | Global | Cycle through main tabs (`Projects`, `Deployments`, `Logs`, `System`) |
| `j` / `k` or `↓` / `↑` | List / Table | Move cursor item focus up / down |
| `Enter` | Selected Resource | Drill down into Resource Graph details (`aegis inspect`) |
| `r` | Resource Action | Trigger manual restart signal for focused process |
| `d` | Resource Action | Trigger new manual deployment trigger for focused project |
| `l` | Resource Action | Switch instantly to live log tail stream for focused process |
| `q` / `Esc` | Global | Exit TUI application or close modal overlay |

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
