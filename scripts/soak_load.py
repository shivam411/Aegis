#!/usr/bin/env python3
"""Steady HTTP load for scripts/soak.sh. Standard library only.

Each worker thread keeps one HTTP/1.1 connection alive and sends GET
requests at a fixed pace (--rate requests per second in total). Every
second it appends one line to <out-dir>/load.csv:

    epoch_s,ok,failed,p50_ms,p99_ms,max_ms

and every failed request to <out-dir>/failures.csv:

    epoch_ms,worker,error

A request fails on a connection error, a timeout or any status but 200.
After a failure the worker reconnects and goes on at the same pace, so a
second with zero successful requests means the app was unreachable.
Runs until SIGTERM/SIGINT or until --stop-file exists.
"""

import argparse
import http.client
import os
import signal
import threading
import time

stop = threading.Event()


def percentile(sorted_values, p):
    if not sorted_values:
        return 0.0
    k = min(len(sorted_values) - 1, max(0, int(round((len(sorted_values) - 1) * p))))
    return sorted_values[k]


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.buckets = {}  # second -> [ok, failed, [latencies]]
        self.failures = []

    def record(self, ok, latency_ms, worker, error=None):
        now = time.time()
        sec = int(now)
        with self.lock:
            b = self.buckets.setdefault(sec, [0, 0, []])
            if ok:
                b[0] += 1
                b[2].append(latency_ms)
            else:
                b[1] += 1
                self.failures.append((int(now * 1000), worker, error))

    def drain(self, before):
        with self.lock:
            done = sorted(s for s in self.buckets if s < before)
            rows = [(s, self.buckets.pop(s)) for s in done]
            failures, self.failures = self.failures, []
        return rows, failures


def worker(idx, args, stats, interval):
    conn = None
    next_at = time.monotonic() + (idx * interval / max(1, args.workers))
    while not stop.is_set():
        now = time.monotonic()
        if next_at > now:
            time.sleep(min(next_at - now, 0.5))
            continue
        next_at += interval
        # Don't try to catch up after a stall; keep the pace from now on.
        if next_at < now - 1.0:
            next_at = now + interval
        start = time.monotonic()
        try:
            if conn is None:
                conn = http.client.HTTPConnection(args.host, args.port, timeout=args.timeout)
            conn.request("GET", args.path, headers={"Host": args.host_header})
            resp = conn.getresponse()
            resp.read()
            latency = (time.monotonic() - start) * 1000.0
            if resp.status == 200:
                stats.record(True, latency, idx)
            else:
                stats.record(False, latency, idx, f"HTTP {resp.status}")
            if resp.will_close:
                conn.close()
                conn = None
        except Exception as e:  # noqa: BLE001 - every failure is data here
            stats.record(False, 0.0, idx, f"{type(e).__name__}: {e}".replace(",", ";")[:200])
            if conn is not None:
                conn.close()
            conn = None
            time.sleep(0.02)
    if conn is not None:
        conn.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--host-header", required=True)
    ap.add_argument("--path", default="/")
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--rate", type=float, default=100.0, help="requests per second, in total")
    ap.add_argument("--timeout", type=float, default=5.0)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--stop-file")
    args = ap.parse_args()

    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    signal.signal(signal.SIGINT, lambda *_: stop.set())

    load_csv = os.path.join(args.out_dir, "load.csv")
    fail_csv = os.path.join(args.out_dir, "failures.csv")
    with open(load_csv, "w") as f:
        f.write("epoch_s,ok,failed,p50_ms,p99_ms,max_ms\n")
    with open(fail_csv, "w") as f:
        f.write("epoch_ms,worker,error\n")

    stats = Stats()
    interval = args.workers / args.rate
    threads = [
        threading.Thread(target=worker, args=(i, args, stats, interval), daemon=True)
        for i in range(args.workers)
    ]
    for t in threads:
        t.start()

    def flush(before):
        rows, failures = stats.drain(before)
        with open(load_csv, "a") as f:
            for sec, (ok, failed, lat) in rows:
                lat.sort()
                f.write(f"{sec},{ok},{failed},{percentile(lat, 0.5):.2f},"
                        f"{percentile(lat, 0.99):.2f},{(lat[-1] if lat else 0):.2f}\n")
        if failures:
            with open(fail_csv, "a") as f:
                for ts, w, err in failures:
                    f.write(f"{ts},{w},{err}\n")

    while not stop.is_set():
        time.sleep(1.0)
        flush(int(time.time()))
        if args.stop_file and os.path.exists(args.stop_file):
            stop.set()
    for t in threads:
        t.join(timeout=args.timeout + 1)
    flush(int(time.time()) + 1)


if __name__ == "__main__":
    main()
