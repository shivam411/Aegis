#!/usr/bin/env bash
# Runs the benchmarks of a source tree and puts the results in one directory:
#   <out>/criterion/...        Criterion output (CRITERION_HOME)
#   <out>/deploy_saga.json     deploy saga timings (AEGIS_BENCH_RESULTS)
#
# Usage: scripts/run-benchmarks.sh <out-dir> [tree]
#   tree defaults to this checkout. CI runs it twice on the same runner, for
#   a PR's base commit (checked out in a worktree) and for the PR, then
#   compares the two with scripts/bench_report.py. Benchmarks that don't
#   exist in <tree> are skipped. Each tree needs its own CARGO_TARGET_DIR:
#   cargo would treat the same crate in two checkouts as one, and reuse
#   artifacts built from the other tree's source.
#
# Environment: BENCH_MEASUREMENT_SECS (default 4), BENCH_WARMUP_SECS (1),
# AEGIS_BENCH_DEPLOYS (8).
set -euo pipefail

OUT="$(mkdir -p "$1" && cd "$1" && pwd)"
TREE="$(cd "${2:-$(dirname "$0")/..}" && pwd)"
MEASURE="${BENCH_MEASUREMENT_SECS:-4}"
WARMUP="${BENCH_WARMUP_SECS:-1}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$TREE/target}"
export CRITERION_HOME="$OUT/criterion"
export AEGIS_BENCH_RESULTS="$OUT"

cd "$TREE"
criterion=()
for bench in event_throughput proxy_overhead; do
  if [ -f "benchmarks/benches/$bench.rs" ]; then criterion+=(--bench "$bench"); fi
done
if [ "${#criterion[@]}" -gt 0 ]; then
  echo "==> Criterion benchmarks in $TREE: ${criterion[*]}"
  cargo bench -p aegis-benchmarks "${criterion[@]}" -- \
    --warm-up-time "$WARMUP" --measurement-time "$MEASURE" --noplot
fi
if [ -f benchmarks/benches/deploy_saga.rs ]; then
  echo "==> Deploy saga timings in $TREE"
  cargo bench -p aegis-benchmarks --bench deploy_saga
fi
echo "Results in $OUT"
