#!/usr/bin/env bash
# Soak test of the real daemon (Linux only).
#
# Starts aegis-daemon from a build, deploys a copy of examples/node-app on a
# domain served by the built-in proxy, and keeps steady HTTP load on it
# through the proxy (scripts/soak_load.py) while it periodically redeploys,
# rolls back, restarts the app, changes resource limits and restarts the
# daemon. It samples the daemon's RSS, open fds and threads, the database,
# log and data directory sizes, and the app's pid and restart count. At the
# end scripts/soak_report.py writes report.md and summary.json next to the
# CSVs and fails the run on: failed requests during deploys/rollbacks/limit
# changes or steady state beyond a small threshold, a failed operation, a
# daemon crash, an app crash, leak-like RSS/fd growth, or ERROR log lines.
#
# Usage: BIN_DIR=target/release DURATION_MIN=5 OUT_DIR=soak-report scripts/soak.sh
# Run as root (sudo -E) to exercise cgroup resource limits; otherwise limit
# changes are skipped and the report says so. See docs/benchmarks.md.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$(cd "${BIN_DIR:-$ROOT/target/release}" && pwd)"
AEGIS_DAEMON="$BIN_DIR/aegis-daemon"
DURATION_MIN="${DURATION_MIN:-5}"
OUT_DIR="${OUT_DIR:-$ROOT/soak-report}"
SAMPLE_SECS="${SAMPLE_SECS:-5}"
OP_INTERVAL_SECS="${OP_INTERVAL_SECS:-20}"
LOAD_RATE="${LOAD_RATE:-100}"
LOAD_WORKERS="${LOAD_WORKERS:-8}"
GRPC_PORT="${GRPC_PORT:-50251}"
WEB_PORT="${WEB_PORT:-18520}"
PROXY_HTTP_PORT="${PROXY_HTTP_PORT:-18080}"
PROXY_HTTPS_PORT="${PROXY_HTTPS_PORT:-18443}"
APP_PORT="${APP_PORT:-3555}"
DOMAIN="${DOMAIN:-soak.test}"
PROJECT=soak-app

DURATION_SECS=$((DURATION_MIN * 60))
# Daemon restarts split the run into segments; leak detection looks at each
# segment, so keep them long: one restart at 75% for short runs, else every
# 20 minutes.
if [ -z "${DAEMON_RESTART_EVERY_SECS:-}" ]; then
  if [ "$DURATION_MIN" -le 10 ]; then
    DAEMON_RESTART_EVERY_SECS=$((DURATION_SECS * 3 / 4))
  else
    DAEMON_RESTART_EVERY_SECS=1200
  fi
fi

[ "$(uname -s)" = Linux ] || { echo "soak.sh needs Linux (/proc)"; exit 2; }
for tool in curl python3 node; do
  command -v "$tool" >/dev/null || { echo "soak.sh needs $tool"; exit 2; }
done
[ -x "$AEGIS_DAEMON" ] || { echo "no daemon at $AEGIS_DAEMON (cargo build --release -p aegis-daemon)"; exit 2; }

mkdir -p "$OUT_DIR"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
WORK="$(mktemp -d)"
APP="$WORK/node-app"
DATA="$WORK/data"
DAEMON_LOG="$OUT_DIR/daemon.log"
SAMPLES="$OUT_DIR/samples.csv"
OPS="$OUT_DIR/ops.csv"
JAR="$WORK/cookies"
API="http://127.0.0.1:$WEB_PORT/api/v1"
DAEMON_PID=""
LOAD_PID=""
SAMPLER_PID=""
: >"$DAEMON_LOG"

now_ms() { date +%s%3N; }
log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }

cleanup() {
  local code=$?
  if [ -n "$LOAD_PID" ]; then kill -TERM "$LOAD_PID" 2>/dev/null || true; fi
  if [ -n "$SAMPLER_PID" ]; then kill -TERM "$SAMPLER_PID" 2>/dev/null || true; fi
  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill -TERM "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  wait 2>/dev/null || true
  rm -rf "$WORK"
  # App cgroups are removed by the daemon; remove what is left of ours.
  if [ -n "${CGROUP_CREATED:-}" ]; then
    find "$CGROUP_ROOT" -mindepth 1 -depth -type d -exec rmdir {} \; 2>/dev/null || true
    rmdir "$CGROUP_ROOT" 2>/dev/null || true
  fi
  exit "$code"
}
trap cleanup EXIT

# --- daemon ----------------------------------------------------------------

# As root on cgroup v2, give the daemon a cgroup of its own with the cpu,
# memory and pids controllers enabled, so limit changes are exercised. (On
# hosted runners the daemon's own systemd service has no delegated
# controllers, so automatic detection finds none.)
CGROUP_ROOT="${SOAK_CGROUP_ROOT:-}"
CGROUP_CREATED=""
if [ -z "$CGROUP_ROOT" ] && [ "$(id -u)" = 0 ] && [ -f /sys/fs/cgroup/cgroup.controllers ]; then
  candidate="/sys/fs/cgroup/aegis-soak-$$"
  if echo "+cpu +memory +pids" >/sys/fs/cgroup/cgroup.subtree_control 2>/dev/null \
    && mkdir -p "$candidate" \
    && echo "+cpu +memory +pids" >"$candidate/cgroup.subtree_control" 2>/dev/null; then
    CGROUP_ROOT="$candidate"
    CGROUP_CREATED=1
  else
    rmdir "$candidate" 2>/dev/null || true
  fi
fi
RESOURCES_TOML=""
if [ -n "$CGROUP_ROOT" ]; then
  RESOURCES_TOML="[resources]
cgroup_root = \"$CGROUP_ROOT\""
fi

cat >"$WORK/daemon.toml" <<EOF
[daemon]
host = "127.0.0.1"
port = $GRPC_PORT
database_path = "$DATA/aegis.db"
log_level = "info"
data_dir = "$DATA"

[web]
enabled = true
host = "127.0.0.1"
port = $WEB_PORT

[proxy]
enabled = true
host = "127.0.0.1"
http_port = $PROXY_HTTP_PORT
https_port = $PROXY_HTTPS_PORT

$RESOURCES_TOML
EOF

start_daemon() {
  local t0 i
  t0=$(now_ms)
  (cd "$WORK" && NO_COLOR=1 AEGIS_CONFIG="$WORK/daemon.toml" exec "$AEGIS_DAEMON" >>"$DAEMON_LOG" 2>&1) &
  DAEMON_PID=$!
  echo "$DAEMON_PID" >"$WORK/daemon.pid"
  for i in $(seq 1 300); do
    if [ "$(curl -s --max-time 1 "http://127.0.0.1:$WEB_PORT/healthz" 2>/dev/null)" = ok ]; then
      echo "daemon_start,$t0,$(now_ms),ok,pid $DAEMON_PID" >>"$OPS"
      return 0
    fi
    kill -0 "$DAEMON_PID" 2>/dev/null || break
    sleep 0.1
  done
  echo "daemon_start,$t0,$(now_ms),fail,daemon did not become ready" >>"$OPS"
  log "daemon did not start"; tail -n 40 "$DAEMON_LOG"; exit 1
}

stop_daemon() {
  kill -TERM "$DAEMON_PID"
  wait "$DAEMON_PID" 2>/dev/null || true
  DAEMON_PID=""
}

# --- API helpers (session cookie + CSRF token, re-login when needed) --------

CSRF=""
json() { python3 -c "import sys, json; d = json.load(sys.stdin); print($1)"; }

login() {
  local password
  password=$(cat "$DATA/initial-admin-password")
  CSRF=$(curl -sS -c "$JAR" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$password\"}" "$API/auth/login" | json 'd["csrf_token"]')
}

ensure_login() {
  local code
  code=$(curl -s -o /dev/null -w '%{http_code}' -b "$JAR" "$API/auth/session" || true)
  [ "$code" = 200 ] || login
}

# api METHOD PATH [BODY] -> prints the body, then a last line with the status.
api() {
  local body="${3:-}"
  [ -n "$body" ] || body='{}'
  if [ "$1" = GET ]; then
    curl -sS --max-time 120 -w '\n%{http_code}' -b "$JAR" "$API$2"
  else
    curl -sS --max-time 120 -w '\n%{http_code}' -b "$JAR" -X "$1" -H "X-CSRF-Token: $CSRF" \
      -H 'Content-Type: application/json' -d "$body" "$API$2"
  fi
}

# Runs an API call; sets RESP_BODY / RESP_CODE.
call() {
  local out
  out=$(api "$@" || true)
  RESP_CODE="${out##*$'\n'}"
  RESP_BODY="${out%$'\n'*}"
}

via_proxy_ok() {
  [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 -H "Host: $DOMAIN" \
    "http://127.0.0.1:$PROXY_HTTP_PORT/")" = 200 ]
}

# Waits until the app answers through the proxy; prints milliseconds waited.
wait_serving() {
  local t0 i
  t0=$(now_ms)
  for i in $(seq 1 600); do
    if via_proxy_ok; then echo $(($(now_ms) - t0)); return 0; fi
    sleep 0.1
  done
  echo -1
  return 1
}

# --- operations -------------------------------------------------------------

DEPLOY_N=0
LIMITS_N=0
LIMITS_BACKEND=none

# record OP START RESULT DETAIL
record() { echo "$1,$2,$(now_ms),$3,$(echo "$4" | tr ',\n' ';  ')" >>"$OPS"; }

op_deploy() {
  local t0 id status=""
  t0=$(now_ms)
  DEPLOY_N=$((DEPLOY_N + 1))
  sed -i "s/version: '[^']*'/version: 's$DEPLOY_N'/" "$APP/index.js"
  call POST "/projects/$PROJECT/deploy" "{\"version\":\"s$DEPLOY_N\"}"
  if [ "$RESP_CODE" != 202 ]; then
    record deploy "$t0" fail "HTTP $RESP_CODE $RESP_BODY"; return
  fi
  id=$(echo "$RESP_BODY" | json 'd["deployment_id"]')
  for _ in $(seq 1 1200); do
    call GET "/deployments/$id"
    status=$(echo "$RESP_BODY" | json 'd.get("status", "")' 2>/dev/null || true)
    case "$status" in Queued|InProgress|"") sleep 0.1 ;; *) break ;; esac
  done
  if [ "$status" = Success ]; then
    record deploy "$t0" ok "s$DEPLOY_N"
  else
    record deploy "$t0" fail "s$DEPLOY_N ended $status"
  fi
}

op_rollback() {
  local t0
  t0=$(now_ms)
  call POST "/projects/$PROJECT/rollback"
  if [ "$RESP_CODE" = 200 ]; then
    record rollback "$t0" ok "to $(echo "$RESP_BODY" | json 'd.get("version")')"
  else
    record rollback "$t0" fail "HTTP $RESP_CODE $RESP_BODY"
  fi
}

op_restart() {
  local t0 waited
  t0=$(now_ms)
  call POST "/processes/$PROJECT/restart"
  if [ "$RESP_CODE" != 200 ]; then
    record restart "$t0" fail "HTTP $RESP_CODE $RESP_BODY"; return
  fi
  if waited=$(wait_serving); then
    record restart "$t0" ok "serving again after ${waited} ms"
  else
    record restart "$t0" fail "not serving 60 s after the restart"
  fi
}

op_limits() {
  local t0 body
  t0=$(now_ms)
  if [ "$LIMITS_BACKEND" = none ]; then
    record limits "$t0" skipped "no cgroup backend (run as root on cgroup v2; see resources.json)"; return
  fi
  LIMITS_N=$((LIMITS_N + 1))
  if [ $((LIMITS_N % 2)) = 1 ]; then
    body='{"cpu_cores":0.5,"memory_bytes":268435456,"pids_max":256}'
  else
    body='{"cpu_cores":1.5,"memory_bytes":402653184}'
  fi
  call PUT "/projects/$PROJECT/resources" "$body"
  if [ "$RESP_CODE" = 200 ]; then
    record limits "$t0" ok "$body"
  else
    record limits "$t0" fail "HTTP $RESP_CODE $RESP_BODY"
  fi
}

DAEMON_RESTARTS=0
op_daemon_restart() {
  local t0 waited
  DAEMON_RESTARTS=$((DAEMON_RESTARTS + 1))
  fd_snapshot "before-daemon-restart-$DAEMON_RESTARTS"
  t0=$(now_ms)
  touch "$WORK/restarting"
  stop_daemon
  start_daemon
  rm -f "$WORK/restarting"
  ensure_login
  if waited=$(wait_serving); then
    record daemon_restart "$t0" ok "app serving ${waited} ms after the daemon was ready"
  else
    record daemon_restart "$t0" fail "app not serving 60 s after the daemon restart"
  fi
}

# Where each of the daemon's open fds points, for telling what kind of fd
# grows when the count does (fds-<label>.txt).
fd_snapshot() {
  local pid fd
  pid=$(cat "$WORK/daemon.pid" 2>/dev/null || echo 0)
  kill -0 "$pid" 2>/dev/null || return 0
  for fd in /proc/"$pid"/fd/*; do
    printf '%s %s\n' "${fd##*/}" "$(readlink "$fd" 2>/dev/null)"
  done >"$OUT_DIR/fds-$1.txt"
}

# --- sampler ----------------------------------------------------------------

proc_field() { awk -v k="$2:" '$1 == k {print $2}' "/proc/$1/status" 2>/dev/null || true; }
# RSS of a process and its children, in kB.
tree_rss() {
  local total=0 p r
  for p in "$1" $(pgrep -P "$1" 2>/dev/null); do
    r=$(proc_field "$p" VmRSS)
    total=$((total + ${r:-0}))
  done
  echo "$total"
}
size_of() { du -sb "$@" 2>/dev/null | awk '{s += $1} END {print s + 0}'; }

sampler() {
  echo "epoch_s,daemon_pid,daemon_alive,rss_kb,fds,threads,zombies,db_bytes,wal_bytes,app_logs_bytes,data_bytes,daemon_log_bytes,app_pid,app_rss_kb,app_restarts" >"$SAMPLES"
  local pid alive rss fds threads zombies app app_pid app_rss restarts
  while true; do
    pid=$(cat "$WORK/daemon.pid" 2>/dev/null || echo 0)
    alive=0
    rss=""; fds=""; threads=""; zombies=""
    if kill -0 "$pid" 2>/dev/null; then
      alive=1
      rss=$(proc_field "$pid" VmRSS)
      threads=$(proc_field "$pid" Threads)
      fds=$(find "/proc/$pid/fd" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l)
      zombies=$(pgrep -c -r Z -P "$pid" || true)
    elif [ -e "$WORK/restarting" ]; then
      alive=2  # stopped on purpose
    fi
    app=$(curl -s --max-time 2 -b "$JAR" "$API/processes?project=$PROJECT" 2>/dev/null \
      | python3 -c 'import sys, json
try:
    ps = [p for p in json.load(sys.stdin) if p.get("status") == "Running"]
    p = ps[-1] if ps else {}
    print(p.get("pid") or "", p.get("restart_count", ""))
except Exception:
    print(" ")' 2>/dev/null || echo " ")
    app_pid="${app%% *}"; restarts="${app#* }"
    app_rss=""
    [ -n "$app_pid" ] && app_rss=$(tree_rss "$app_pid")
    echo "$(date +%s),$pid,$alive,$rss,$fds,$threads,$zombies,$(size_of "$DATA/aegis.db"),$(size_of "$DATA/aegis.db-wal"),$(size_of "$DATA"/projects/*/logs),$(size_of "$DATA"),$(size_of "$DAEMON_LOG"),$app_pid,$app_rss,$restarts" >>"$SAMPLES"
    sleep "$SAMPLE_SECS"
  done
}

# --- run ----------------------------------------------------------------------

echo "op,start_ms,end_ms,result,detail" >"$OPS"
log "Soak: ${DURATION_MIN} min, ${LOAD_RATE} req/s via the proxy, an operation every ${OP_INTERVAL_SECS} s, daemon restart every ${DAEMON_RESTART_EVERY_SECS} s"
log "Work dir $WORK, report in $OUT_DIR"

cp -R "$ROOT/examples/node-app" "$APP"
sed -i "s/status: 'ok'/status: 'ok', version: 's0'/" "$APP/index.js"
cat >"$APP/aegis.toml" <<EOF
[project]
name = "$PROJECT"

[build]
install_command = ""
build_command = "npm run build"
start_command = "node index.js"

[deploy]
port = $APP_PORT
health_check_url = "http://127.0.0.1:$APP_PORT/"
health_check_timeout_secs = 30
drain_timeout_secs = 10
EOF

start_daemon
login
call POST /projects "{\"name\":\"$PROJECT\",\"source_dir\":\"$APP\"}"
case "$RESP_CODE" in 200|201) ;; *) log "registering the app failed: $RESP_CODE $RESP_BODY"; exit 1 ;; esac
call POST "/projects/$PROJECT/domains" "{\"domain\":\"$DOMAIN\",\"tls\":\"off\"}"
[ "$RESP_CODE" = 201 ] || { log "mapping $DOMAIN failed: $RESP_CODE $RESP_BODY"; exit 1; }
call GET /resources
echo "$RESP_BODY" >"$OUT_DIR/resources.json"
LIMITS_BACKEND=$(echo "$RESP_BODY" | json 'd.get("backend", "none")' 2>/dev/null || echo none)
log "Resource limits backend: $LIMITS_BACKEND ($(echo "$RESP_BODY" | json 'd.get("reason") or d.get("location")' 2>/dev/null || true))"

op_deploy
grep -q '^deploy,.*,ok,' "$OPS" || { log "first deploy failed"; cat "$OPS"; tail -n 40 "$DAEMON_LOG"; exit 1; }
wait_serving >/dev/null || { log "app not reachable through the proxy"; exit 1; }
log "First deploy is live behind http://$DOMAIN (proxy port $PROXY_HTTP_PORT)"

sampler &
SAMPLER_PID=$!
fd_snapshot start
python3 "$ROOT/scripts/soak_load.py" --port "$PROXY_HTTP_PORT" --host-header "$DOMAIN" \
  --rate "$LOAD_RATE" --workers "$LOAD_WORKERS" --out-dir "$OUT_DIR" &
LOAD_PID=$!
sleep 5

START=$(date +%s)
END=$((START + DURATION_SECS))
NEXT_DAEMON_RESTART=$((START + DAEMON_RESTART_EVERY_SECS))
CYCLE=(deploy limits deploy rollback restart deploy limits)
i=0
while [ "$(date +%s)" -lt "$END" ]; do
  t=$(date +%s)
  if [ "$t" -ge "$NEXT_DAEMON_RESTART" ] && [ $((END - t)) -gt 30 ]; then
    op=daemon_restart
    NEXT_DAEMON_RESTART=$((t + DAEMON_RESTART_EVERY_SECS))
  else
    op=${CYCLE[$((i % ${#CYCLE[@]}))]}
    i=$((i + 1))
  fi
  if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
    log "the daemon died"
    record daemon_crash "$(now_ms)" fail "daemon pid $DAEMON_PID exited"
    break
  fi
  slot_end=$((t + OP_INTERVAL_SECS))
  ensure_login
  "op_$op"
  log "$(tail -n 1 "$OPS")"
  # Steady load until the next slot (or the end of the run).
  while [ "$(date +%s)" -lt "$slot_end" ] && [ "$(date +%s)" -lt "$END" ]; do
    sleep 1
    kill -0 "$DAEMON_PID" 2>/dev/null || break
  done
done

log "Load finished; collecting results"
sleep 3
kill -TERM "$LOAD_PID"; wait "$LOAD_PID" 2>/dev/null || true; LOAD_PID=""
kill -TERM "$SAMPLER_PID" 2>/dev/null; wait "$SAMPLER_PID" 2>/dev/null || true; SAMPLER_PID=""

DAEMON_ALIVE_AT_END=0
if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
  DAEMON_ALIVE_AT_END=1
  fd_snapshot end
  t0=$(now_ms)
  stop_daemon
  echo "daemon_stop,$t0,$(now_ms),ok,graceful shutdown" >>"$OPS"
fi

# Event counts by type, and every crash/failure event, straight from the
# database (the API returns at most 1000 events).
python3 - "$DATA/aegis.db" "$OUT_DIR/events.json" <<'PY' || echo '{}' >"$OUT_DIR/events.json"
import json, sqlite3, sys
db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
counts = dict(db.execute("SELECT event_type, COUNT(*) FROM events GROUP BY event_type"))
crashes = [
    {"event_type": t, "created_at": c, "payload": json.loads(p)}
    for t, p, c in db.execute(
        "SELECT event_type, payload_json, created_at FROM events WHERE event_type IN "
        "('ProcessCrashed', 'ProcessFailed', 'DeploymentFailed', 'ResourceAlert') ORDER BY seq")
]
json.dump({"counts": counts, "events": crashes}, open(sys.argv[2], "w"), indent=1)
PY

cat >"$OUT_DIR/run.json" <<EOF
{"duration_min": $DURATION_MIN, "load_rate": $LOAD_RATE, "load_workers": $LOAD_WORKERS,
 "op_interval_secs": $OP_INTERVAL_SECS, "cgroup_root": "$CGROUP_ROOT", "sample_secs": $SAMPLE_SECS,
 "daemon_restart_every_secs": $DAEMON_RESTART_EVERY_SECS, "limits_backend": "$LIMITS_BACKEND",
 "daemon_alive_at_end": $DAEMON_ALIVE_AT_END, "domain": "$DOMAIN",
 "kernel": "$(uname -r)", "cpus": $(nproc), "commit": "${GITHUB_SHA:-$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)}"}
EOF

python3 "$ROOT/scripts/soak_report.py" "$OUT_DIR"
