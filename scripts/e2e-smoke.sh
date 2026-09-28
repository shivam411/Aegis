#!/usr/bin/env bash
# End-to-end check of the real binaries: deploys examples/node-app and walks
# through redeploy, stop/start/restart, failed builds, unhealthy releases,
# rollback and daemon restarts.
#
# Usage: scripts/e2e-smoke.sh            (uses target/debug)
#        BIN_DIR=target/release scripts/e2e-smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$(cd "${BIN_DIR:-$ROOT/target/debug}" && pwd)"
AEGIS_CLI="$BIN_DIR/aegis-cli"
AEGIS_DAEMON="$BIN_DIR/aegis-daemon"
GRPC_PORT="${GRPC_PORT:-50151}"
APP_PORT="${APP_PORT:-3456}"

WORK="$(mktemp -d)"
APP="$WORK/node-app"
DAEMON_LOG="$WORK/daemon.log"
DAEMON_PID=""

cleanup() {
  local code=$?
  if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill -TERM "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  if [ "$code" -ne 0 ]; then
    echo "---- daemon log (last 60 lines) ----"
    tail -n 60 "$DAEMON_LOG" 2>/dev/null || true
    echo "FAILED (work dir kept at $WORK)"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

step() { printf '\n==> %s\n' "$*"; }
fail() { echo "ASSERTION FAILED: $*" >&2; exit 1; }
aegis() { (cd "$APP" && "$AEGIS_CLI" -c "$WORK/daemon.toml" "$@"); }

# Body of GET / from the app, or empty when nothing is listening.
app_body() { curl -sf --max-time 2 "http://127.0.0.1:$APP_PORT/" 2>/dev/null || true; }

expect_serving() {
  local want="$1"
  for _ in $(seq 1 50); do
    if app_body | grep -q "\"version\":\"$want\""; then return 0; fi
    sleep 0.2
  done
  fail "expected version $want to be served, got: $(app_body)"
}

expect_down() {
  for _ in $(seq 1 50); do
    if [ -z "$(app_body)" ]; then return 0; fi
    sleep 0.2
  done
  fail "expected the app to be down"
}

start_daemon() {
  # exec so that $! is the daemon itself, not a wrapping subshell.
  (cd "$WORK" && AEGIS_CONFIG="$WORK/daemon.toml" exec "$AEGIS_DAEMON" >>"$DAEMON_LOG" 2>&1) &
  DAEMON_PID=$!
  for _ in $(seq 1 100); do
    if "$AEGIS_CLI" -c "$WORK/daemon.toml" status >/dev/null 2>&1; then return 0; fi
    sleep 0.1
  done
  fail "daemon did not start"
}

stop_daemon() {
  kill -TERM "$DAEMON_PID"
  wait "$DAEMON_PID" 2>/dev/null || true
  DAEMON_PID=""
}

set_version() {
  sed -i.bak "s/version: '[^']*'/version: '$1'/" "$APP/index.js" && rm -f "$APP/index.js.bak"
}

# ---------------------------------------------------------------------------
step "Preparing a copy of examples/node-app in $WORK"
cp -R "$ROOT/examples/node-app" "$APP"
# Tag responses with a version so we can tell releases apart.
sed -i.bak "s/status: 'ok'/status: 'ok', version: 'v1'/" "$APP/index.js" && rm -f "$APP/index.js.bak"
cat >"$WORK/daemon.toml" <<EOF
[daemon]
host = "127.0.0.1"
port = $GRPC_PORT
database_path = "$WORK/aegis.db"
log_level = "info"
data_dir = "$WORK/data"
EOF

step "Starting daemon"
start_daemon

step "aegis init + validate"
aegis init --name smoke-app
sed -i.bak "s/3000/$APP_PORT/g" "$APP/aegis.toml" && rm -f "$APP/aegis.toml.bak"
aegis validate

step "First deploy"
aegis deploy --release v1
expect_serving v1
aegis list | tee "$WORK/list.txt"
grep -q "smoke-app.*Running.*v1" "$WORK/list.txt" || fail "list does not show smoke-app running v1"
aegis logs -n 20 | grep -q "running on port $APP_PORT" || fail "app output missing from logs"

step "Restart changes the pid and keeps serving"
PID_BEFORE=$(aegis list | awk '/smoke-app/ {print $5}')
aegis restart
expect_serving v1
PID_AFTER=$(aegis list | awk '/smoke-app/ {print $5}')
[ "$PID_BEFORE" != "$PID_AFTER" ] || fail "restart did not change the pid ($PID_BEFORE)"

step "Stop and start"
aegis stop
expect_down
aegis start
expect_serving v1

step "Second deploy (v2)"
set_version v2
aegis deploy --release v2
expect_serving v2

step "Broken build is rejected and v2 keeps serving"
cp "$APP/package.json" "$WORK/package.json.good"
sed -i.bak 's/"build": "[^"]*"/"build": "echo compile error \&\& exit 1"/' "$APP/package.json"
if aegis deploy --release v3-broken-build >"$WORK/out.txt" 2>&1; then
  cat "$WORK/out.txt"; fail "broken build deployed successfully"
fi
cat "$WORK/out.txt"
grep -q "live release was not changed" "$WORK/out.txt" || fail "missing 'not changed' message"
grep -q "compile error" "$WORK/out.txt" || fail "build output not shown"
expect_serving v2
cp "$WORK/package.json.good" "$APP/package.json"

step "Unhealthy release is rolled back automatically"
sed -i.bak 's/"start": "[^"]*"/"start": "node missing.js"/' "$APP/package.json"
if aegis deploy --release v3-crashes >"$WORK/out.txt" 2>&1; then
  cat "$WORK/out.txt"; fail "crashing release deployed successfully"
fi
cat "$WORK/out.txt"
grep -q "release v2 is serving again" "$WORK/out.txt" || fail "missing rollback message"
expect_serving v2
cp "$WORK/package.json.good" "$APP/package.json"

step "Manual rollback to the previous release"
aegis rollback
expect_serving v1
aegis releases
aegis deployments
aegis timeline -n 15

step "Daemon restart brings the app back"
stop_daemon
expect_down
start_daemon
expect_serving v1

step "A stopped app stays stopped across daemon restarts"
aegis stop
expect_down
stop_daemon
start_daemon
sleep 1
[ -z "$(app_body)" ] || fail "stopped app came back after daemon restart"
aegis start
expect_serving v1

printf '\nAll end-to-end checks passed.\n'
