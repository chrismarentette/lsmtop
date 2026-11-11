#!/usr/bin/env bash
# lsmtop release gate — unit suite, lint/build, and the llvm-cov coverage step
# (kanban t_fe7b2c2a; opt-in instrumented-pty counting t_8ac9aff4; threshold
# policy per the t_e2c75257 coverage audit).
#
# STAGES RUN STRICTLY IN ORDER. A failing stage aborts the run, so the
# coverage step can never execute before the prerequisite P1+P2 stages pass:
#
#   [P1]       unit suite: cargo test --no-fail-fast — every suite must report ok
#   [P2]       lint + release build: cargo clippy --all-targets -- -D warnings
#              && cargo build --release
#   [COVERAGE] cargo llvm-cov --summary-only --fail-under-lines <floor>
#              (re-runs the whole suite instrumented, logs the measured TOTAL
#              line-coverage and the PASS/FAIL verdict)
#
# PTY COUNTING — OPT-IN via PTY_COVERAGE=1 (env) or --pty-coverage.
# Without the opt-in this script behaves EXACTLY as before (unit-only
# profile). With it, the COVERAGE stage counts the pty harnesses' screen time
# (main.rs event loop, gpu fork glue) by merging their LLVM profiles into the
# same llvm-cov report, in this order:
#   a. cargo llvm-cov clean            (FULL clean, not --profraw-only)
#      hermetic input set: --no-report implies --no-clean (cargo-llvm-cov
#      accumulates on purpose), so raw profiles of earlier runs MUST be
#      cleared first — merging stale profiles would inflate the number.
#      FULL is required: `report` discovers EVERY instrumented binary in
#      target/llvm-cov-target/, and stale objects from an earlier source
#      revision (old hashes) would be counted at 0% and crater the TOTAL
#      (measured: 82.6% -> 66.7% with one old gate run's deps left behind).
#   b. cargo llvm-cov --no-report
#      same instrumented build, full suite; raw profiles accumulate in
#      target/llvm-cov-target/ instead of producing a report.
#   c. scripts/pty-coverage-harness.sh --bin <that same binary>
#      (t_6dd9aafd) runs ui_verify_v3, ui_verify_v4 and sparkproof against
#      target/llvm-cov-target/debug/lsmtop — the EXACT binary step b built,
#      so every profile hash matches the test profiles; each harness gets
#      its own fakehome and LLVM_PROFILE_FILE and MUST clean-exit to flush.
#   d. harness raw profiles are staged into target/llvm-cov-target/ and
#        cargo llvm-cov report --summary-only --fail-under-lines <floor>
#      merges every raw profile into ONE report and evaluates the threshold
#      on the merged TOTAL. (Mechanic verified against cargo-llvm-cov
#      0.9.1: `report` re-merges the top-level *.profraw of its target dir
#      on every call; this version has no --add-profdata.)
# Measured @8f8cf8a: TOTAL lines 67.79% -> 82.74% (src/main.rs 7.93% ->
# 54.88%, src/gpu.rs 67.34% -> 86.84%) — the 80 target is GREEN with pty
# counting, inside the 82-85% ceiling. A --report-only re-check re-reads
# the raw profiles the last full run left behind; with --pty-coverage and
# no pty raws on disk it says so loudly (the re-check would be unit-only).
#
# THRESHOLD POLICY (metric pinned to LINES — regions/funcs read differently
# at the same number; branches are not instrumentable on stable, never gate
# them):
#   TARGET = 80 lines, the coverage-arc contract (stretch card t_44f7e30d).
#   The enforced floor is COV_MIN_LINES (default 80). Unit-only P1+P2
#   plateaus ~76-78% (main.rs + gpu.rs TTY/FFI bodies are the gap; ceiling
#   is ~82-85%), so a PLAIN run at 80 is red until the arc finishes; per
#   doctrine a gate that starts red gets deleted, so pre-pty releases pin an
#   explicit floor one point below the measured plain total, e.g.
#       COV_MIN_LINES=65 scripts/release-gate.sh
#   The pty opt-in closes the gap honestly: at 82.74% measured, a pty run
#   passes the untouched 80 target. Never paper over the gap with file
#   exclusions — excluding main.rs+gpu.rs reads 87% and is metric gaming.
#
# USAGE
#   scripts/release-gate.sh [--report-only] [--pty-coverage] [--log FILE]
#     --report-only     cheap dry-run of the coverage check: re-reads the last
#                       stored coverage profile (cargo llvm-cov report) without
#                       recompiling or re-running tests. Requires a prior full
#                       run. A real release check re-runs the tests.
#     --pty-coverage    enable instrumented-pty counting; same as
#                       PTY_COVERAGE=1 (see PTY COUNTING above).
#     --log FILE        append the full stage transcript to FILE.
#
# EXIT 0 = every stage passed; non-zero = the first failing stage (its name is
# in the final RELEASE-GATE line). CI/dev boxes need:
#   cargo install cargo-llvm-cov && rustup component add llvm-tools-preview
# The pty stage additionally needs ~/lsmtop-py/.venv (pyte) and a free port
# 8123 (sparkproof's fake router) — the harness runner preflights both and
# exits 2 on a busy precondition (that aborts the gate at COVERAGE, loudly).

set -uo pipefail
# repo root = one level above this script (NOT `git rev-parse`: the gate must
# also run on archive/CI checkouts that have no .git)
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2

TARGET_LINES=80
FLOOR="${COV_MIN_LINES:-$TARGET_LINES}"
case "$FLOOR" in (''|*[!0-9]*) echo "release-gate: COV_MIN_LINES must be an integer, got '$COV_MIN_LINES'" >&2; exit 2;; esac

# opt-in instrumented-pty counting (t_8ac9aff4); 0 = today's plain behavior
PTY="${PTY_COVERAGE:-0}"
case "$PTY" in
  (1|true|yes|on)   PTY=1;;
  (0|''|false|no|off) PTY=0;;
  (*) echo "release-gate: PTY_COVERAGE must be 0/1 (or true/false/yes/no/on/off), got '$PTY_COVERAGE'" >&2; exit 2;;
esac

REPORT_ONLY=0
LOG=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --report-only) REPORT_ONLY=1; shift;;
    --pty-coverage) PTY=1; shift;;
    --log) LOG="${2:?--log needs a file}"; shift 2;;
    -h|--help) awk 'NR>1{ if (!/^#/) exit; sub(/^# ?/,""); print }' "$0"; exit 0;;
    *) echo "release-gate: unknown option '$1' (--report-only | --pty-coverage | --log FILE)" >&2; exit 2;;
  esac
done

# one tee for the whole run, on top of the console
if [[ -n "$LOG" ]]; then exec > >(tee -a "$LOG") 2>&1; fi

say()  { printf '%s\n' "$*"; }
banner() { printf '\n===== [%s] %s =====\n' "$1" "$2"; }
finish() { # stage verdict -> single greppable line + exit
  local mode="coverage floor $FLOOR% lines, target $TARGET_LINES%"
  [[ $PTY -eq 1 ]] && mode="$mode, pty-counting ON"
  if [[ "$2" -eq 0 ]]; then say "RELEASE-GATE: PASS (stages P1,P2,COVERAGE all green; $mode)"
  else say "RELEASE-GATE: FAIL at $1 ($mode)"; fi
  exit "$2"
}

# ---- preflight ------------------------------------------------------------
command -v cargo >/dev/null || { say "release-gate: cargo not found"; exit 2; }
if ! cargo llvm-cov --version >/dev/null 2>&1; then
  say "release-gate: cargo-llvm-cov missing — install with:"
  say "  cargo install cargo-llvm-cov && rustup component add llvm-tools-preview"
  exit 2
fi
if [[ $REPORT_ONLY -eq 0 ]]; then
  banner P1 "unit suite (cargo test --no-fail-fast)"
  P1_OUT=$(cargo test --no-fail-fast 2>&1); P1_RC=$?
  printf '%s\n' "$P1_OUT" | grep -E '^(test result|error|failures:)' | grep -v '^test result: ok' || true
  N_OK=$(printf '%s\n' "$P1_OUT" | grep -cE '^test result: ok')
  if [[ $P1_RC -ne 0 || $N_OK -lt 1 ]]; then
    say "P1: unit suite FAILED (ok-suites=$N_OK, cargo exit $P1_RC) — coverage step NOT run"
    finish P1 1
  fi
  say "P1: unit suite GREEN ($N_OK suites, all ok)"

  banner P2 "lint + release build"
  if ! cargo clippy --all-targets -- -D warnings; then
    say "P2: clippy FAILED — coverage step NOT run"; finish P2 1
  fi
  if ! cargo build --release; then
    say "P2: release build FAILED — coverage step NOT run"; finish P2 1
  fi
  say "P2: clippy clean (-D warnings), release build finished"
else
  say "NOTE: --report-only — P1/P2 and the instrumented test run are SKIPPED;"
  say "      re-checking the threshold against the last coverage profile only."
fi

# ---- coverage step (only ever reached after P1+P2 passed above) -----------
if [[ ! -f src/input.rs ]]; then
  say "NOTE: P2 not landed in this tree (no src/input.rs): the 80% target is"
  say "      unreachable until the input.rs extraction + instrumented-pty"
  say "      counting land — pin COV_MIN_LINES per the header policy."
fi

COV_TARGET_DIR="$PWD/target/llvm-cov-target"
COV_BIN="$COV_TARGET_DIR/debug/lsmtop"
PTY_OUT="$PWD/target/llvm-cov"

if [[ $PTY -eq 1 ]]; then
  banner COVERAGE "llvm-cov gate WITH instrumented-pty counts (floor $FLOOR% lines; arc target $TARGET_LINES%)"
  if [[ $REPORT_ONLY -eq 1 ]]; then
    # raw profiles persist after a full run and `report` re-merges what is on
    # disk — say loudly when the stored set carries no pty counts at all.
    if ! { compgen -G "$COV_TARGET_DIR/v3-*.profraw" \
         || compgen -G "$COV_TARGET_DIR/v4-*.profraw" \
         || compgen -G "$COV_TARGET_DIR/sparkproof-*.profraw"; } >/dev/null; then
      say "NOTE: no pty harness raw profiles on disk — the last full run was"
      say "      unit-only; this re-check does NOT include pty counts."
    fi
    COV_OUT=$(cargo llvm-cov report --summary-only --fail-under-lines "$FLOOR" 2>&1); COV_RC=$?
  else
    # a. hermetic input set (see header): FULL clean — stale raws AND stale
    #    instrumented objects (report counts their regions at 0% otherwise)
    if ! cargo llvm-cov clean; then
      say "PTY: profile clean FAILED — merged coverage NOT evaluated"; finish COVERAGE 1
    fi
    # b. instrumented build + full suite; raw profiles stay on disk (--no-clean
    #    is implied by --no-report, which is exactly what the merge wants)
    if ! cargo llvm-cov --no-report; then
      say "PTY: instrumented test run FAILED — merged coverage NOT evaluated"; finish COVERAGE 1
    fi
    [[ -x "$COV_BIN" ]] || { say "PTY: instrumented binary missing: $COV_BIN"; finish COVERAGE 1; }
    # c. pty harnesses against the SAME instrumented binary (hash identity is
    #    required to merge); the runner self-proves a pty-only report first
    if ! scripts/pty-coverage-harness.sh --bin "$COV_BIN" --out "$PTY_OUT"; then
      say "PTY: harness/profile stage FAILED — merged coverage NOT evaluated"; finish COVERAGE 1
    fi
    # d. stage the harness raws next to the test raws (top level = the glob
    #    cargo llvm-cov merges at report time) and evaluate the merged TOTAL
    if ! cp "$PTY_OUT"/profraw/*.profraw "$COV_TARGET_DIR/"; then
      say "PTY: could not stage harness profiles — merged coverage NOT evaluated"; finish COVERAGE 1
    fi
    say "PTY: merged input set: $(ls "$COV_TARGET_DIR"/*.profraw | wc -l) raw profiles (tests + v3/v4/sparkproof)"
    COV_OUT=$(cargo llvm-cov report --summary-only --fail-under-lines "$FLOOR" 2>&1); COV_RC=$?
  fi
else
  banner COVERAGE "llvm-cov line-coverage gate (floor $FLOOR% lines; arc target $TARGET_LINES%)"
  if [[ $REPORT_ONLY -eq 1 ]]; then
    COV_OUT=$(cargo llvm-cov report --summary-only --fail-under-lines "$FLOOR" 2>&1); COV_RC=$?
  else
    COV_OUT=$(cargo llvm-cov --summary-only --fail-under-lines "$FLOOR" 2>&1); COV_RC=$?
  fi
fi
printf '%s\n' "$COV_OUT"
# lines-cover% = the LAST field ending in '%' on the TOTAL row (the trailing
# branches column prints '-', so $NF is wrong)
MEASURED=$(printf '%s\n' "$COV_OUT" | awk '/^TOTAL/{for(i=NF;i>0;i--) if($i ~ /%$/){print $i; exit}}')
if [[ "$MEASURED" == *% ]]; then
  say "COVERAGE RESULT: lines=${MEASURED} floor=${FLOOR}% target=${TARGET_LINES}% -> $([[ $COV_RC -eq 0 ]] && echo PASS || echo FAIL)"
else
  say "COVERAGE RESULT: no TOTAL row parsed (cargo llvm-cov exit $COV_RC)"
fi
if [[ $COV_RC -ne 0 ]]; then finish COVERAGE 1; fi
if [[ "$FLOOR" -lt "$TARGET_LINES" ]]; then
  say "floor $FLOOR < target $TARGET_LINES: raise COV_MIN_LINES toward 80 as coverage lands"
fi
say "coverage stage PASS at floor $FLOOR (measured ${MEASURED:-?} lines)"
finish ALL 0
