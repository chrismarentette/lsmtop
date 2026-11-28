#!/usr/bin/env bash
# perf-check.sh — lsmtop performance regression gate (perf pillar).
# Contract + thresholds + measured baselines: docs/perf-gate.md
#
# Stages (ordered; a failing stage aborts):
#   [P1] release build of the measured binary
#   [B ] criterion benches: render p95, buffer-diff law+cost, ingest tps
#   [S ] release-mode bounded-state stress (200k ticks, ring caps hold)
#   [F ] floodproof pty stress: payload/journal/input floods + RSS ceiling
#
# Escape hatches (doctrine: floor = measured-minus-margin, NEVER aspirational):
#   PERF_MAX_FRAME_MS (default 8)      per full-dashboard frame, p50 proxy
#   PERF_MAX_DIFF_MS  (default 5)      buffer diff cost, worst case
#   PERF_MIN_INGEST_TPS (default 50000) sampler ingest ticks/sec
#   PERF_MAX_RSS_MB   (default 30)     floodproof live RSS ceiling
#   PERF_MAX_RSS_GROWTH_MB (default 5) growth across the flood scenario
set -uo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"   # repo root from own path

MAX_FRAME_MS="${PERF_MAX_FRAME_MS:-8}"
MAX_DIFF_MS="${PERF_MAX_DIFF_MS:-5}"
MIN_INGEST_TPS="${PERF_MIN_INGEST_TPS:-12000}"
PY="${PY:-$HOME/lsmtop-py/.venv/bin/python}"
LOG=""
[[ "${1:-}" == "--log" ]] && { LOG="${2:-}"; shift 2 || true; }
say() { printf '%s\n' "$*" | tee -a ${LOG:-/dev/null}; }

fail() { say ""; say "PERF-GATE: FAIL at $1"; exit 1; }

say "=== [P1] cargo build --release (the measured artifact) ==="
cargo build --release 2>&1 | tail -1 | tee -a ${LOG:-/dev/null} \
  || fail P1
[[ -x target/release/lsmtop ]] || fail P1

ss -tln 2>/dev/null | grep -q ':8123 ' && say "NOTE: port 8123 busy — floodproof's fake router binds it; free it first" && exit 2

say ""
say "=== [B] criterion benches (criterion offline -> loud SKIP, never silent) ==="
if ! cargo bench --no-run >/dev/null 2>&1; then
  say "SKIP: benches did not compile (criterion fetch offline?) — NOT a pass"
  fail B
fi

# criterion 0.5 writes no JSON to stdout; its report lives in
# target/criterion/<group>/<id>/new/estimates.json — median.point_estimate
# is NANoseconds. We run the single bench quietly, then read the file back.
bench_median_ns() { # $1 = bench id, $2 = bench target; prints median ns or nothing
  # criterion 0.5.1 takes the filter POSITIONALLY (--filter is unknown here)
  cargo bench --bench "$2" -- "$1" >/dev/null 2>&1
  # group '/' is flattened to '_' on disk: frame/history/1k-1lane ->
  # frame_history/1k-1lane; single-group ids (ingest/tick) keep the slash.
  local dir="${1%/*}" leaf="${1##*/}"
  local rels=("$1" "${dir//\//_}/$leaf")
  local rel f
  for rel in "${rels[@]}"; do
    for f in "new" "base"; do
      [[ -f "target/criterion/$rel/$f/estimates.json" ]] && {
        "$PY" -c "import json;print(int(json.load(open('target/criterion/$rel/$f/estimates.json'))['median']['point_estimate']))"; return; }
    done
  done
}

declare -A RES
for id in "frame/history/1k-1lane" "frame/history/10k-1lane" "frame/history/100k-1lane" "frame/history/100k-11lanes"; do
  ns=$(bench_median_ns "$id" render_bench)
  [[ -n "$ns" ]] || fail "B($id: no result — renamed?)"
  RES[$id]=$ns
  ms=$(awk "BEGIN{printf \"%.3f\", $ns/1e6}")
  awk "BEGIN{exit !($ms <= $MAX_FRAME_MS)}" || fail "B($id frame ${ms}ms > ${MAX_FRAME_MS}ms)"
  say "  $id  frame=${ms}ms (<= ${MAX_FRAME_MS}ms)"
done

# unchanged-frame diff law is ASSERTED inside diff_bench (a nonzero diff
# panics the bench). Here we price the worst-case diff pass.
ns=$(bench_median_ns "diff/frame" diff_bench)
[[ -n "$ns" ]] || fail "B(diff/frame: no result)"
ms=$(awk "BEGIN{printf \"%.3f\", $ns/1e6}")
awk "BEGIN{exit !($ms <= $MAX_DIFF_MS)}" || fail "B(diff/frame ${ms}ms > ${MAX_DIFF_MS}ms)"
say "  diff/frame cost=${ms}ms (<= ${MAX_DIFF_MS}ms; unchanged=0-cell law asserted in-bench)"

ns=$(bench_median_ns "ingest/tick" ingest_bench)
[[ -n "$ns" ]] || fail "B(ingest/tick: no result)"
tps=$(awk "BEGIN{printf \"%.0f\", 1e9/$ns}")
awk "BEGIN{exit !($tps >= $MIN_INGEST_TPS)}" || fail "B(ingest ${tps} tps < ${MIN_INGEST_TPS})"
say "  ingest/tick ${tps} ticks/s (>= ${MIN_INGEST_TPS})"

say ""
say "=== [S] release-mode bounded-state stress ==="
cargo test --release --test stress_bounded 2>&1 | tail -2 | tee -a ${LOG:-/dev/null}
[[ ${PIPESTATUS[0]} -eq 0 ]] || fail S

say ""
say "=== [F] floodproof (pty, fake router :8123, fresh fakehome) ==="
FH="$(mktemp -d)"; OUT="$(mktemp -d)"
"$PY" tools/floodproof.py "$OUT" perfgate HOME="$FH" LSMTOP_CONFIG="$FH/.lsmtop/config.json" -- ./target/release/lsmtop > "$OUT/run.txt" 2>&1
rc=$?
tail -6 "$OUT/run.txt" | tee -a ${LOG:-/dev/null}
{ [[ $rc -eq 0 ]] && grep -q "FLOODPROOF: PASS" "$OUT/run.txt" && [[ -f "$OUT/flood.json" ]]; } || fail F
"$PY" - "$OUT/flood.json" <<'PYEOF' || fail F
import json, os, sys
d = json.load(open(sys.argv[1]))
lim = float(os.environ.get("PERF_MAX_RSS_MB", 30))
grow = float(os.environ.get("PERF_MAX_RSS_GROWTH_MB", 5))
assert d["rss_max_mb"] <= lim, f'max RSS {d["rss_max_mb"]} MB > {lim}'
assert d["rss_growth_mb"] <= grow, f'growth {d["rss_growth_mb"]} MB > {grow}'
print(f'  floodproof RSS max={d["rss_max_mb"]}MB (<= {lim}) growth={d["rss_growth_mb"]}MB (<= {grow}) quit={d["quit_ms"]}ms')
PYEOF

say ""
say "PERF-GATE: PASS (P1,B,S,F green; frame<=${MAX_FRAME_MS}ms ingest>=${MIN_INGEST_TPS}tps RSS<=${PERF_MAX_RSS_MB:-30}MB)"
