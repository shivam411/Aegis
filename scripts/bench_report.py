#!/usr/bin/env python3
"""Collects benchmark results into one JSON file and a markdown report, and
compares them with a baseline (the latest run on main).

Inputs:
  * Criterion output: <criterion-dir>/**/new/{benchmark,estimates}.json
  * Timing-harness output: JSON lists like target/bench-results/deploy_saga.json
    ([{"name", "unit": "ms", "median", "min", "p95", "max", "n"}, ...])

Usage:
  scripts/bench_report.py --criterion-dir target/criterion \
      --extra target/bench-results/deploy_saga.json \
      --baseline baseline/bench-results.json \
      --out-json bench-results.json --out-md bench-report.md \
      [--threshold 0.30] [--fail-on-regression] [--github-annotations]

A benchmark regressed when its median is more than --threshold slower than
the baseline's. Regressions are reported (and annotated as warnings in
GitHub Actions); the exit code is 1 only with --fail-on-regression.
Standard library only.
"""

import argparse
import datetime
import glob
import json
import os
import sys

NS = {"ns": 1.0, "us": 1e3, "ms": 1e6, "s": 1e9}


def load_criterion(root):
    out = {}
    for bench_file in glob.glob(os.path.join(root, "**", "new", "benchmark.json"), recursive=True):
        new_dir = os.path.dirname(bench_file)
        est_file = os.path.join(new_dir, "estimates.json")
        if not os.path.exists(est_file):
            continue
        with open(bench_file) as f:
            bench = json.load(f)
        with open(est_file) as f:
            est = json.load(f)
        name = bench.get("full_id") or bench.get("title")
        median = est["median"]
        entry = {
            "kind": "criterion",
            "unit": "ns",
            "value": median["point_estimate"],
            "low": median["confidence_interval"]["lower_bound"],
            "high": median["confidence_interval"]["upper_bound"],
        }
        throughput = bench.get("throughput") or {}
        if "Elements" in throughput:
            entry["elements"] = throughput["Elements"]
        out[name] = entry
    return out


def load_extra(paths):
    out = {}
    for path in paths:
        for item in glob.glob(path):
            with open(item) as f:
                rows = json.load(f)
            for row in rows:
                scale = NS[row.get("unit", "ms")]
                out[row["name"]] = {
                    "kind": "timing",
                    "unit": "ns",
                    "value": row["median"] * scale,
                    "low": row["min"] * scale,
                    "high": row["p95"] * scale,
                    "max": row["max"] * scale,
                    "n": row["n"],
                }
    return out


def fmt_time(ns):
    if ns is None:
        return "-"
    for unit, scale in (("s", 1e9), ("ms", 1e6), ("µs", 1e3)):
        if ns >= scale:
            return f"{ns / scale:.3g} {unit}" if ns / scale < 100 else f"{ns / scale:.0f} {unit}"
    return f"{ns:.3g} ns"


def fmt_rate(entry):
    elements = entry.get("elements")
    if not elements or not entry.get("value"):
        return ""
    per_sec = elements / (entry["value"] / 1e9)
    if per_sec >= 1e6:
        return f"{per_sec / 1e6:.2f} M/s"
    if per_sec >= 1e3:
        return f"{per_sec / 1e3:.1f} k/s"
    return f"{per_sec:.0f} /s"


def compare(current, baseline, threshold):
    rows = []
    for name in sorted(set(current) | set(baseline)):
        cur = current.get(name)
        base = baseline.get(name)
        status, change = "new", None
        if cur is None:
            status = "missing"
        elif base is not None and base.get("value"):
            change = cur["value"] / base["value"] - 1.0
            if change > threshold:
                status = "REGRESSION"
            elif change < -threshold:
                status = "faster"
            else:
                status = "ok"
        rows.append((name, cur, base, change, status))
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--criterion-dir", default="target/criterion")
    ap.add_argument("--extra", action="append", default=[])
    ap.add_argument("--baseline")
    ap.add_argument("--out-json", required=True)
    ap.add_argument("--out-md", required=True)
    ap.add_argument("--threshold", type=float, default=0.30)
    ap.add_argument("--fail-on-regression", action="store_true")
    ap.add_argument("--github-annotations", action="store_true")
    args = ap.parse_args()

    current = load_criterion(args.criterion_dir)
    current.update(load_extra(args.extra))
    if not current:
        print("no benchmark results found", file=sys.stderr)
        return 2

    meta = {
        "commit": os.environ.get("GITHUB_SHA", ""),
        "ref": os.environ.get("GITHUB_REF", ""),
        "run_id": os.environ.get("GITHUB_RUN_ID", ""),
        "created_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "runner": os.environ.get("RUNNER_OS", "") + " " + os.environ.get("RUNNER_ARCH", ""),
    }
    with open(args.out_json, "w") as f:
        json.dump({"schema": 1, "meta": meta, "benchmarks": current}, f, indent=2, sort_keys=True)

    baseline, base_meta = {}, None
    if args.baseline and os.path.exists(args.baseline):
        with open(args.baseline) as f:
            data = json.load(f)
        baseline = data.get("benchmarks", {})
        base_meta = data.get("meta", {})

    rows = compare(current, baseline, args.threshold)
    regressions = [r for r in rows if r[4] == "REGRESSION"]
    pct = int(args.threshold * 100)

    md = ["## Benchmarks", ""]
    if base_meta:
        commit = (base_meta.get("commit") or "")[:10]
        md.append(
            f"Compared with main at `{commit}` (run {base_meta.get('run_id', '?')}, "
            f"{base_meta.get('created_at', '?')}). Medians; a change beyond ±{pct}% is flagged. "
            "Shared CI runners are noisy: treat single flags as hints, look for repeats."
        )
    else:
        md.append("No main baseline found yet (the first run on main creates one). Absolute numbers only.")
    md.append("")
    if regressions:
        md.append(f"**{len(regressions)} benchmark(s) more than {pct}% slower than main:** "
                  + ", ".join(f"`{r[0]}` ({r[3] * 100:+.0f}%)" for r in regressions))
        md.append("")
    md.append("| Benchmark | main | this run | change | rate | status |")
    md.append("| :--- | ---: | ---: | ---: | ---: | :--- |")
    for name, cur, base, change, status in rows:
        md.append(
            "| `{}` | {} | {} | {} | {} | {} |".format(
                name,
                fmt_time(base["value"]) if base else "-",
                fmt_time(cur["value"]) if cur else "-",
                f"{change * 100:+.1f}%" if change is not None else "",
                fmt_rate(cur) if cur else "",
                f"**{status}**" if status == "REGRESSION" else status,
            )
        )
    timing = [(n, c) for n, c in current.items() if c["kind"] == "timing"]
    if timing:
        md += ["", "Deploy saga timings (whole deployments of a tiny app, real processes):", ""]
        md.append("| Scenario | n | min | median | p95 | max |")
        md.append("| :--- | ---: | ---: | ---: | ---: | ---: |")
        for name, c in sorted(timing):
            md.append(f"| `{name}` | {c['n']} | {fmt_time(c['low'])} | {fmt_time(c['value'])} | "
                      f"{fmt_time(c['high'])} | {fmt_time(c['max'])} |")
    md.append("")
    with open(args.out_md, "w", encoding="utf-8") as f:
        f.write("\n".join(md))

    if args.github_annotations:
        for name, _, _, change, _ in regressions:
            print(f"::warning title=Benchmark regression::{name} is {change * 100:+.0f}% "
                  f"slower than main (threshold {pct}%)")
    print("\n".join(md))
    if regressions and args.fail_on_regression:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
