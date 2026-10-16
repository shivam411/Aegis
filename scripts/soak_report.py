#!/usr/bin/env python3
"""Turns the CSVs written by scripts/soak.sh into report.md and
summary.json, and decides whether the soak passed. Standard library only.

Usage: scripts/soak_report.py <out-dir>      (exit code 1 = soak failed)

Thresholds (environment variables, defaults in brackets):
  SOAK_MAX_FAILED_REQUESTS [5]      failed requests allowed outside app/daemon
  SOAK_MAX_FAILED_RATIO [0.0005]    restarts (whichever allows more)
  SOAK_RECOVERY_GRACE_MS [2000]     how long after an app or daemon restart
                                    failures still count as that restart's
  SOAK_RSS_MAX_GROWTH_MB [16]       leak: fitted RSS growth over a daemon
  SOAK_RSS_MAX_SLOPE_MB_PER_H [20]  segment beyond both of these, and still
                                    growing (last third above first third)
  SOAK_FD_MAX_GROWTH [20]           leak: fitted open-fd growth over a segment
  SOAK_MIN_SEGMENT_SECS [150]       shorter daemon segments aren't judged
  SOAK_MAX_APP_CRASHES [0]          ProcessCrashed events for the app
  SOAK_ALLOWED_ERRORS ['']          regex of ERROR log lines to ignore
"""

import csv
import json
import os
import re
import statistics
import sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")
DISRUPTIVE = {"restart", "daemon_restart"}


def env_float(name, default):
    try:
        return float(os.environ.get(name, default))
    except ValueError:
        return float(default)


def read_csv(path):
    if not os.path.exists(path):
        return []
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def num(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def pct(sorted_values, p):
    if not sorted_values:
        return None
    k = min(len(sorted_values) - 1, max(0, int(round((len(sorted_values) - 1) * p))))
    return sorted_values[k]


def slope_per_sec(points):
    """Least-squares slope of (t, y) points."""
    n = len(points)
    if n < 2:
        return 0.0
    mt = sum(t for t, _ in points) / n
    my = sum(y for _, y in points) / n
    den = sum((t - mt) ** 2 for t, _ in points)
    if den == 0:
        return 0.0
    return sum((t - mt) * (y - my) for t, y in points) / den


def segment_trend(points):
    """Fitted growth over the segment (after a 20% warm-up) and whether the
    last third sits above the first third."""
    if len(points) < 6:
        return None
    skip = max(1, len(points) // 5)
    pts = points[skip:]
    s = slope_per_sec(pts)
    span = pts[-1][0] - pts[0][0]
    third = max(1, len(pts) // 3)
    first = statistics.median(y for _, y in pts[:third])
    last = statistics.median(y for _, y in pts[-third:])
    return {
        "slope_per_hour": s * 3600,
        "fitted_growth": s * span,
        "first_third_median": first,
        "last_third_median": last,
        "rising": last > first,
        "start": pts[0][1],
        "end": pts[-1][1],
        "span_secs": span,
    }


def fmt_ms(v):
    if v is None:
        return "-"
    return f"{v:.2f} ms" if v < 10 else f"{v:,.0f} ms"


def fd_kind(target):
    for prefix in ("socket:", "pipe:", "anon_inode:"):
        if target.startswith(prefix):
            return prefix[:-1]
    if target.startswith("/dev/"):
        return "device"
    return "file" if target else "unknown"


def read_fd_snapshots(out):
    """{label: [target, ...]} from fds-<label>.txt, in the order taken."""
    snaps = []
    for name in os.listdir(out):
        if name.startswith("fds-") and name.endswith(".txt"):
            path = os.path.join(out, name)
            with open(path, errors="replace") as f:
                targets = [line.rstrip("\n").split(" ", 1)[1] if " " in line else ""
                           for line in f if line.strip()]
            label = name[4:-4]
            if label == "start":
                key = (0, 0)
            elif label == "end":
                key = (2, 0)
            else:
                digits = "".join(c for c in label if c.isdigit())
                key = (1, int(digits) if digits else 0)
            snaps.append((key, label, targets))
    return [(label, targets) for _, label, targets in sorted(snaps)]


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "soak-report"
    run = {}
    if os.path.exists(os.path.join(out, "run.json")):
        with open(os.path.join(out, "run.json")) as f:
            run = json.load(f)
    samples = read_csv(os.path.join(out, "samples.csv"))
    load = read_csv(os.path.join(out, "load.csv"))
    failures = read_csv(os.path.join(out, "failures.csv"))
    ops = read_csv(os.path.join(out, "ops.csv"))

    max_failed = env_float("SOAK_MAX_FAILED_REQUESTS", 5)
    max_ratio = env_float("SOAK_MAX_FAILED_RATIO", 0.0005)
    grace = env_float("SOAK_RECOVERY_GRACE_MS", 2000)
    rss_max_mb = env_float("SOAK_RSS_MAX_GROWTH_MB", 16)
    rss_max_slope = env_float("SOAK_RSS_MAX_SLOPE_MB_PER_H", 20)
    fd_max = env_float("SOAK_FD_MAX_GROWTH", 20)
    min_segment = env_float("SOAK_MIN_SEGMENT_SECS", 150)
    max_crashes = env_float("SOAK_MAX_APP_CRASHES", 0)
    allowed_errors = os.environ.get("SOAK_ALLOWED_ERRORS", "")

    problems = []
    notes = []

    # --- operations ---------------------------------------------------------
    windows = []  # (start_ms, end_ms, op)
    by_op = {}
    for o in ops:
        s, e = num(o["start_ms"]), num(o["end_ms"])
        by_op.setdefault(o["op"], []).append(o)
        if o["result"] == "fail":
            problems.append(f"operation `{o['op']}` failed: {o['detail']}")
        if s is not None and e is not None and o["op"] not in ("daemon_start", "daemon_stop"):
            end = e + (grace if o["op"] in DISRUPTIVE else 0)
            windows.append((s, end, o["op"]))
        if o["op"] == "daemon_crash":
            problems.append("the daemon exited on its own")

    def window_of(ts_ms):
        hit = None
        for s, e, op in windows:
            if s <= ts_ms <= e:
                if op in DISRUPTIVE:
                    return op
                hit = op
        return hit or "steady"

    # --- requests -----------------------------------------------------------
    total_ok = sum(int(r["ok"]) for r in load)
    total_failed = sum(int(r["failed"]) for r in load)
    total = total_ok + total_failed
    secs = len(load)
    zero_ok_secs = sum(1 for r in load if int(r["ok"]) == 0)
    p50s = sorted(num(r["p50_ms"]) for r in load if int(r["ok"]) > 0)
    p99s = sorted(num(r["p99_ms"]) for r in load if int(r["ok"]) > 0)
    max_lat = max((num(r["max_ms"]) or 0 for r in load), default=0)

    fail_by_window = {}
    fail_kinds = {}
    for f in failures:
        w = window_of(num(f["epoch_ms"]) or 0)
        fail_by_window[w] = fail_by_window.get(w, 0) + 1
        kind = (f.get("error") or "").split(":")[0]
        fail_kinds[kind] = fail_kinds.get(kind, 0) + 1
    disruptive_failed = sum(v for k, v in fail_by_window.items() if k in DISRUPTIVE)
    other_failed = sum(v for k, v in fail_by_window.items() if k not in DISRUPTIVE)

    # Requests sent outside disruptive windows (approximation by second).
    def second_disruptive(sec):
        return any(op in DISRUPTIVE and s <= sec * 1000 + 999 and e >= sec * 1000
                   for s, e, op in windows)

    other_total = sum(int(r["ok"]) + int(r["failed"]) for r in load
                      if not second_disruptive(int(r["epoch_s"])))
    allowed = max(max_failed, max_ratio * other_total)
    if total == 0:
        problems.append("the load generator recorded no requests")
    if other_failed > allowed:
        problems.append(
            f"{other_failed} requests failed during deploys, rollbacks, limit changes or steady "
            f"state (allowed {allowed:.0f})")

    # --- daemon samples -----------------------------------------------------
    segments = {}
    dead_samples = 0
    for s in samples:
        alive = s.get("daemon_alive")
        if alive == "0":
            dead_samples += 1
            continue
        if alive != "1":
            continue
        t = num(s["epoch_s"])
        segments.setdefault(s["daemon_pid"], []).append((t, s))
    if dead_samples:
        problems.append(f"the daemon was not running in {dead_samples} sample(s) outside planned restarts")
    if run.get("daemon_alive_at_end") == 0:
        problems.append("the daemon was not running at the end")

    seg_rows = []
    for pid, rows in segments.items():
        span = rows[-1][0] - rows[0][0] if rows else 0
        rss = [(t, num(s["rss_kb"]) / 1024) for t, s in rows if num(s["rss_kb"]) is not None]
        fds = [(t, num(s["fds"])) for t, s in rows if num(s["fds"]) is not None]
        thr = [(t, num(s["threads"])) for t, s in rows if num(s["threads"]) is not None]
        rt, ft, tt = segment_trend(rss), segment_trend(fds), segment_trend(thr)
        judged = span >= min_segment and rt is not None
        verdict = "not judged (short)" if not judged else "ok"
        if judged:
            if (rt["fitted_growth"] > rss_max_mb and rt["slope_per_hour"] > rss_max_slope
                    and rt["rising"]):
                verdict = "RSS GROWTH"
                problems.append(
                    f"daemon pid {pid}: RSS grew {rt['fitted_growth']:.1f} MiB over "
                    f"{span / 60:.0f} min ({rt['slope_per_hour']:.1f} MiB/h), still rising")
            if ft and ft["fitted_growth"] > fd_max and ft["rising"]:
                verdict = "FD GROWTH" if verdict == "ok" else verdict + ", FD GROWTH"
                problems.append(
                    f"daemon pid {pid}: open fds grew by {ft['fitted_growth']:.0f} over "
                    f"{span / 60:.0f} min, still rising")
            if tt and tt["fitted_growth"] > 20 and tt["rising"]:
                notes.append(f"daemon pid {pid}: thread count grew by {tt['fitted_growth']:.0f}")
        seg_rows.append((pid, span, rss, fds, thr, rt, ft, tt, verdict))

    zombies = max((num(s["zombies"]) or 0 for s in samples if s.get("daemon_alive") == "1"),
                  default=0)
    if zombies > 0:
        notes.append(f"up to {zombies:.0f} zombie child process(es) of the daemon seen at once")

    # --- app crashes ----------------------------------------------------------
    crashes = 0
    event_counts = {}
    events_path = os.path.join(out, "events.json")
    if os.path.exists(events_path):
        try:
            with open(events_path) as f:
                data = json.load(f)
            event_counts = data.get("counts", {})
            crashes = int(event_counts.get("ProcessCrashed", 0))
            for ev in data.get("events", []):
                if ev.get("event_type") != "ProcessCrashed":
                    notes.append(f"{ev.get('event_type')} at {ev.get('created_at')}: "
                                 f"{json.dumps(ev.get('payload'))[:200]}")
        except (ValueError, AttributeError, TypeError):
            notes.append("events.json could not be read")
    else:
        notes.append("no events.json (the daemon's database could not be read)")
    if crashes > max_crashes:
        problems.append(f"the app crashed {crashes} time(s) (ProcessCrashed events)")

    # --- daemon log -----------------------------------------------------------
    errors, warns = [], 0
    log_path = os.path.join(out, "daemon.log")
    if os.path.exists(log_path):
        with open(log_path, errors="replace") as f:
            for line in f:
                line = ANSI.sub("", line.rstrip("\n"))
                if re.search(r"\sERROR\s", line):
                    if not (allowed_errors and re.search(allowed_errors, line)):
                        errors.append(line)
                elif re.search(r"\sWARN\s", line):
                    warns += 1
    if errors:
        problems.append(f"{len(errors)} ERROR line(s) in the daemon log")

    # --- report ---------------------------------------------------------------
    passed = not problems
    duration = run.get("duration_min", "?")
    md = [f"## Soak test ({duration} min): {'PASS' if passed else 'FAIL'}", ""]
    if problems:
        md += [f"- {p}" for p in problems] + [""]
    md.append(
        f"Commit `{str(run.get('commit', ''))[:10]}`, {run.get('cpus', '?')} CPUs, kernel "
        f"{run.get('kernel', '?')}. Load: {run.get('load_rate', '?')} req/s over "
        f"{run.get('load_workers', '?')} keep-alive connections to `{run.get('domain', '?')}` "
        f"through the built-in proxy. An operation every {run.get('op_interval_secs', '?')} s, "
        f"daemon restart every {run.get('daemon_restart_every_secs', '?')} s. "
        f"Resource-limit backend: `{run.get('limits_backend', '?')}`.")
    md += ["", "### Requests", ""]
    md.append("| Requests | Failed | Success | Avg rate | p50 (median of 1 s p50s) | p99 (worst 1 s p99) | Max | Seconds with no successful request |")
    md.append("| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    md.append(
        f"| {total:,} | {total_failed:,} | {(100.0 * total_ok / total if total else 0):.3f}% | "
        f"{(total / secs if secs else 0):.1f}/s | {fmt_ms(pct(p50s, 0.5))} | "
        f"{fmt_ms(p99s[-1] if p99s else None)} | {fmt_ms(max_lat)} | {zero_ok_secs} |")
    md += ["", f"Failed requests by what was happening (restart windows include {grace / 1000:.0f} s of recovery): "
           + (", ".join(f"{k}: {v}" for k, v in sorted(fail_by_window.items())) or "none")
           + f". Allowed outside app/daemon restarts: {allowed:.0f}; seen: {other_failed}."
           + (f" Failures during app/daemon restarts are expected (no zero-downtime restart yet): {disruptive_failed}." if disruptive_failed else ""),
           ""]
    if fail_kinds:
        md.append("Failure kinds: " + ", ".join(f"`{k}` × {v}" for k, v in sorted(fail_kinds.items())))
        md.append("")

    md += ["### Operations", ""]
    md.append("| Operation | Count | OK | Failed | Skipped | Median | p95 | Max |")
    md.append("| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for op, rows in sorted(by_op.items()):
        durs = sorted(num(r["end_ms"]) - num(r["start_ms"]) for r in rows
                      if num(r["end_ms"]) is not None and r["result"] == "ok")
        md.append(
            f"| `{op}` | {len(rows)} | {sum(r['result'] == 'ok' for r in rows)} | "
            f"{sum(r['result'] == 'fail' for r in rows)} | {sum(r['result'] == 'skipped' for r in rows)} | "
            f"{fmt_ms(pct(durs, 0.5))} | {fmt_ms(pct(durs, 0.95))} | {fmt_ms(durs[-1] if durs else None)} |")
    md += ["", "`deploy`, `rollback` are timed from the API call until the release is live; "
           "`daemon_start` from launch until `/healthz` answers.", ""]

    md += ["### Daemon resources per process lifetime", ""]
    md.append("| Daemon pid | Length | RSS start → end | RSS fitted growth | RSS slope | FDs start → end | FD fitted growth | Threads start → end | Verdict |")
    md.append("| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |")
    for pid, span, rss, fds, thr, rt, ft, tt, verdict in seg_rows:
        def se(series, f="{:.1f}"):
            return f"{f.format(series[0][1])} → {f.format(series[-1][1])}" if series else "-"
        md.append(
            f"| {pid} | {span / 60:.1f} min | {se(rss)} MiB | "
            f"{(rt['fitted_growth'] if rt else 0):+.1f} MiB | {(rt['slope_per_hour'] if rt else 0):+.1f} MiB/h | "
            f"{se(fds, '{:.0f}')} | {(ft['fitted_growth'] if ft else 0):+.0f} | {se(thr, '{:.0f}')} | {verdict} |")
    md += ["", f"Leak heuristic: a daemon lifetime of at least {min_segment / 60:.1f} min fails when, after "
           f"a 20% warm-up, the least-squares RSS trend grows more than {rss_max_mb:.0f} MiB at more than "
           f"{rss_max_slope:.0f} MiB/h and the last third is above the first third; or open fds grow by "
           f"more than {fd_max:.0f} the same way.", ""]

    if samples:
        first, last = samples[0], samples[-1]
        span_h = max((num(last["epoch_s"]) - num(first["epoch_s"])) / 3600, 1e-9)

        def growth(col):
            a, b = num(first[col]) or 0, num(last[col]) or 0
            return f"{a / 1048576:.2f} → {b / 1048576:.2f} MiB ({(b - a) / 1048576 / span_h:+.2f} MiB/h)"

        md += ["### Disk", "",
               f"- Database: {growth('db_bytes')}; WAL at the end: {(num(last['wal_bytes']) or 0) / 1048576:.2f} MiB",
               f"- App logs: {growth('app_logs_bytes')}",
               f"- Whole data directory (releases, logs, database): {growth('data_bytes')}",
               f"- Daemon log (stdout): {growth('daemon_log_bytes')}", ""]
        restarts = [num(s["app_restarts"]) for s in samples if num(s["app_restarts"]) is not None]
        md.append(f"App: supervisor restart count of the live process peaked at "
                  f"{max(restarts) if restarts else 0:.0f}; ProcessCrashed events: {crashes}.")
        if event_counts:
            md.append(f"Events recorded: {sum(event_counts.values()):,} ("
                      + ", ".join(f"{k} {v}" for k, v in sorted(event_counts.items(), key=lambda kv: -kv[1])[:8])
                      + ", ...).")
        md.append("")

    snaps = read_fd_snapshots(out)
    if snaps:
        kinds = ["socket", "pipe", "anon_inode", "file", "device"]
        md += ["### Daemon fds by kind", "",
               "| Snapshot | Total | " + " | ".join(kinds) + " |",
               "| :--- | ---: | " + " | ".join("---:" for _ in kinds) + " |"]
        for label, targets in snaps:
            counts = {}
            for t in targets:
                counts[fd_kind(t)] = counts.get(fd_kind(t), 0) + 1
            md.append(f"| {label} | {len(targets)} | " + " | ".join(str(counts.get(k, 0)) for k in kinds) + " |")
        first_files = {t for t in snaps[0][1] if fd_kind(t) == "file"}
        new_files = sorted({t for _, ts in snaps[1:] for t in ts if fd_kind(t) == "file"} - first_files)
        if new_files:
            md += ["", "Files open in later snapshots but not in the first: "
                   + ", ".join(f"`{t}`" for t in new_files[:10])
                   + (" ..." if len(new_files) > 10 else "")]
        md += ["", "Snapshots are taken after the first deploy, before each daemon restart and at the end "
               "(full lists in `fds-*.txt`).", ""]

    md += ["### Daemon log", "", f"{len(errors)} ERROR line(s) (not allow-listed), {warns} WARN line(s)."]
    if errors:
        md += ["", "```"] + errors[:15] + ["```"]
    if notes:
        md += ["", "### Notes", ""] + [f"- {n}" for n in notes]

    # A coarse timeline (at most ~24 rows) of the samples.
    if samples:
        step = max(1, len(samples) // 24)
        t0 = num(samples[0]["epoch_s"])
        md += ["", "<details><summary>Timeline (every {} s)</summary>".format(
            step * int(run.get("sample_secs", 5))), "",
               "| t | daemon pid | RSS MiB | fds | threads | DB MiB | app pid | app RSS MiB |",
               "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"]
        for s in samples[::step]:
            rss = num(s["rss_kb"])
            app_rss = num(s["app_rss_kb"])
            md.append(
                f"| {(num(s['epoch_s']) - t0) / 60:.1f} min | {s['daemon_pid'] if s['daemon_alive'] == '1' else '(restarting)'} | "
                f"{(rss / 1024) if rss else 0:.1f} | {s['fds'] or '-'} | {s['threads'] or '-'} | "
                f"{(num(s['db_bytes']) or 0) / 1048576:.2f} | {s['app_pid'] or '-'} | "
                f"{(app_rss / 1024) if app_rss else 0:.1f} |")
        md += ["", "</details>"]
    md.append("")

    with open(os.path.join(out, "report.md"), "w", encoding="utf-8") as f:
        f.write("\n".join(md))
    summary = {
        "passed": passed,
        "problems": problems,
        "notes": notes,
        "requests": {"total": total, "failed": total_failed, "failed_outside_restarts": other_failed,
                     "failed_during_restarts": disruptive_failed, "allowed": allowed,
                     "zero_success_seconds": zero_ok_secs,
                     "p50_ms": pct(p50s, 0.5), "worst_p99_ms": p99s[-1] if p99s else None},
        "app_crashes": crashes,
        "event_counts": event_counts,
        "error_lines": len(errors),
        "segments": [
            {"pid": pid, "span_secs": span,
             "rss_fitted_growth_mb": rt["fitted_growth"] if rt else None,
             "rss_slope_mb_per_h": rt["slope_per_hour"] if rt else None,
             "fd_fitted_growth": ft["fitted_growth"] if ft else None, "verdict": verdict}
            for pid, span, rss, fds, thr, rt, ft, tt, verdict in seg_rows],
        "run": run,
    }
    with open(os.path.join(out, "summary.json"), "w", encoding="utf-8") as f:
        json.dump(summary, f, indent=2)
    print("\n".join(md))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
