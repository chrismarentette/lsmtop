#!/usr/bin/env bash
# lsmtop instrumented-pty coverage runner (kanban t_6dd9aafd).
#
# WHAT
#   Runs the pty harnesses (tools/ui_verify_v3.py, tools/ui_verify_v4.py,
#   tools/sparkproof.py) against a -C instrument-coverage build of lsmtop,
#   each run with its own LLVM_PROFILE_FILE, then merges every emitted
#   profraw into ONE indexed profile the llvm-cov release-gate step can
#   consume. This converts harness screen-time into counted coverage:
#   main.rs's event loop IS exercised by these harnesses but reads ~8%
#   because the runs never contributed profile data (docs/release-gate.md
#   "path to green", audit t_e2c75257).
#
# USAGE
#   scripts/pty-coverage-harness.sh [--bin PATH] [--out DIR] [--keep]
#     --bin PATH   instrumented lsmtop binary to exec. Default: build via
#                  `cargo llvm-cov --no-run` and take the artifact at
#                  target/llvm-cov-target/debug/lsmtop — the exact
#                  instrumented build the gate's test step uses, so profiles
#                  from this runner merge into one llvm-cov report with the
#                  test profiles without hash mismatches.
#     --out DIR    artifacts dir: profraw/, frames/, merged profile.
#                  Default target/llvm-cov (gitignored).
#                  Merged profile: <out>/pty-harness.profdata
#     --keep       keep the fakehomes (under $TMPDIR) for forensics.
#
# HARNESS CONVENTIONS ENFORCED (lsmtop-dev doctrine)
#   * argv form  $PY tools/ui_verify_X.py OUT TAG ENV=VAL... -- BIN  —
#     env pairs BEFORE the --, TAG mandatory (a token before -- is parsed
#     as TAG; a leading -- gives a blank-screen run that looks like breakage).
#   * a fresh fakehome per harness (mktemp -d, never reused — refine's
#     leftover tick_ms=1950 once failed v4's tick_plus_steps).
#   * HOME + LSMTOP_CONFIG both point INTO the fakehome; the runner also
#     checksums the live ~/.lsmtop/config.json around the run and fails
#     loudly if anything touched it.
#   * absolute BIN path after the --: the harness execs the instrumented
#     binary under target/, never a stale target/release/lsmtop.
#   * python = ~/lsmtop-py/.venv/bin/python (pyte lives there).
#   * profile flush law (probed on rustc 1.98): clean exit and SIGSEGV
#     flush; SIGTERM and SIGABRT leave 0-byte profraws; %c continuous mode
#     fails to write. All three harnesses therefore 'q'-quit the app
#     (v3/v4 quit proofs; sparkproof sends q + polls waitpid, TERM fallback).
#   * port 127.0.0.1:8123 must be free (sparkproof's fake router binds it);
#     stray lsmtop children exec'd from the exact BIN path are pkill'ed.
#
# WHAT THIS IS NOT
#   No file exclusions, no branch gating (both banned by the coverage
#   doctrine). This runner only PRODUCES merged profile data; threshold
#   enforcement stays scripts/release-gate.sh's job.
#
# EXIT  0 = all harnesses green + merged profdata written.
#       1 = a harness failed / no profiles / live config mutated.
#       2 = precondition (missing venv/toolchain/port/busy).
set -uo pipefail
# repo root = one level above this script (NOT `git rev-parse`: must also
# work on archive/CI checkouts with no .git — same law as release-gate.sh)
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2
ROOT=$PWD

say() { printf '%s\n' "$*"; }

BIN="" OUT="" KEEP=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin)  BIN="${2:?--bin needs a path}"; shift 2;;
    --out)  OUT="${2:?--out needs a dir}"; shift 2;;
    --keep) KEEP=1; shift;;
    -h|--help) sed -n '2,48p' "$0"; exit 0;;
    *) say "pty-coverage-harness: unknown option '$1' (--bin|--out|--keep)" >&2; exit 2;;
  esac
done

PY="$HOME/lsmtop-py/.venv/bin/python"
[[ -x "$PY" ]] || { say "missing $PY — the harnesses need the pyte venv"; exit 2; }
command -v cargo >/dev/null || { say "cargo not found"; exit 2; }
HOST=$(rustc -vV | awk '/^host:/{print $2}')
TOOLS="$(rustc --print sysroot)/lib/rustlib/$HOST/bin"
PROFDATA="$TOOLS/llvm-profdata" PROFCOV="$TOOLS/llvm-cov"
[[ -x "$PROFDATA" && -x "$PROFCOV" ]] || {
  say "llvm-profdata/llvm-cov missing — rustup component add llvm-tools-preview"; exit 2; }

# ---- instrumented binary ---------------------------------------------------
if [[ -z "$BIN" ]]; then
  say "[BUILD] cargo llvm-cov --no-run (same instrumented build as the gate's tests)"
  mkdir -p "${OUT:-$ROOT/target/llvm-cov}"
  if ! cargo llvm-cov --no-run >"${OUT:-$ROOT/target/llvm-cov}/build.log" 2>&1; then
    say "[BUILD] FAILED (see ${OUT:-$ROOT/target/llvm-cov}/build.log)"; exit 1
  fi
  BIN="$ROOT/target/llvm-cov-target/debug/lsmtop"
fi
[[ -x "$BIN" ]] || { say "binary not found/executable: $BIN"; exit 2; }
BIN=$(readlink -f "$BIN")
# grep -c NOT -q: -q early-exits, nm eats SIGPIPE, pipefail reads that as 141
NSYM=$(nm "$BIN" 2>/dev/null | grep -c '__llvm_profi\|__llvm_prf')
[[ "$NSYM" -gt 0 ]] || {
  say "$BIN is NOT instrumented — needs a -C instrument-coverage build"
  say "(target/release/lsmtop is the stale uninstrumented daily: refuse)"
  exit 2
}

ss -ltn 2>/dev/null | grep -q ':8123 ' && {
  say "port 8123 busy — sparkproof's fake router binds it; free it first"; exit 2; }

OUT=${OUT:-$ROOT/target/llvm-cov}
mkdir -p "$OUT/profraw" "$OUT/frames" "$OUT/logs" || exit 2
rm -f "$OUT/profraw"/*.profraw "$OUT/pty-harness.profdata"
MERGED="$OUT/pty-harness.profdata"

# live-config guard (defense in depth over the env isolation)
LIVE_CFG="$HOME/.lsmtop/config.json"
LIVE_BEFORE=$(sha256sum "$LIVE_CFG" 2>/dev/null | awk '{print $1}')

FAKEHOMES=()
cleanup() {
  # orphan policy: only children exec'd from THIS absolute bin path.
  # The '^' anchor is load-bearing: pkill -f matches the WHOLE command line
  # unanchored, so a caller that passed --bin would see this script's own
  # argv self-match and SIGTERM it at exit (found integrating t_8ac9aff4).
  # A child exec'd the binary directly, so its argv STARTS with $BIN.
  pkill -f -- "^$BIN" 2>/dev/null
  if [[ $KEEP -eq 0 ]]; then
    local fh; for fh in "${FAKEHOMES[@]:-}"; do [[ -n "$fh" ]] && rm -rf "$fh"; done
  fi
}
trap cleanup EXIT

# run_one NAME SCRIPT EXPECTED_PROFILES  (frames land under $OUT/frames/NAME)
run_one() {
  local name=$1 script=$2 want=$3
  local fh frames rc n
  fh=$(mktemp -d "${TMPDIR:-/tmp}/lsmtop-cov-$name.XXXXXX") || { say "mktemp failed"; return 1; }
  FAKEHOMES+=("$fh")
  frames="$OUT/frames/$name"; mkdir -p "$fh/.lsmtop" "$frames"
  say ""
  say "=== [$name] $script  fakehome=$fh ==="
  # argv law: OUT TAG ENV=VAL... -- BIN  (TAG positional, env pairs before --)
  "$PY" "$script" "$frames" "$name" \
      HOME="$fh" LSMTOP_CONFIG="$fh/.lsmtop/config.json" \
      LLVM_PROFILE_FILE="$OUT/profraw/$name-%p-%m.profraw" \
      -- "$BIN" >"$OUT/logs/$name.log" 2>&1
  rc=$?
  tail -6 "$OUT/logs/$name.log"
  n=$(find "$OUT/profraw" -name "$name-*.profraw" -size +0c | wc -l)
  if [[ $rc -ne 0 ]]; then
    say "[$name] HARNESS FAILED (rc=$rc, log $OUT/logs/$name.log)"; return 1
  fi
  if [[ $n -lt $want ]]; then
    say "[$name] harness passed but only $n/$want non-empty profraw emitted"
    say "        (a 0-byte profile means the child was killed, not quit)"; return 1
  fi
  say "[$name] PASS — $n profile(s) emitted"
  return 0
}

FAILED=""
run_one v3         tools/ui_verify_v3.py 1 || FAILED="$FAILED v3"
run_one v4         tools/ui_verify_v4.py 1 || FAILED="$FAILED v4"
run_one sparkproof tools/sparkproof.py   3 || FAILED="$FAILED sparkproof"

if [[ -n "$FAILED" ]]; then
  say ""; say "PTY-COVERAGE: FAIL at harness(es)$FAILED (merged step NOT run)"
  exit 1
fi

# ---- merge ------------------------------------------------------------------
say ""
say "=== [MERGE] llvm-profdata merge -sparse ==="
NALL=$(find "$OUT/profraw" -name '*.profraw' -size +0c | wc -l)
if [[ $NALL -lt 5 ]]; then
  say "expected >=5 profiles (v3, v4, sparkproof x3), found $NALL"; exit 1
fi
if ! "$PROFDATA" merge -sparse "$OUT"/profraw/*.profraw -o "$MERGED"; then
  say "llvm-profdata merge FAILED"; exit 1
fi
say "merged $NALL profiles -> $MERGED ($(stat -c %s "$MERGED") bytes)"

# proof the harness runs actually counted: main.rs must show real coverage
REPORT=$("$PROFCOV" report "$BIN" --instr-profile="$MERGED" 2>&1)
TOTAL=$(printf '%s\n' "$REPORT" | awk '/^TOTAL/{for(i=NF;i>0;i--) if($i ~ /%$/){print $i; exit}}')
MAIN=$(printf '%s\n' "$REPORT" | awk '/src\/main\.rs/{for(i=NF;i>0;i--) if($i ~ /%$/){print $i; exit}}')
say "PTY-ONLY coverage vs $BIN:"
say "  TOTAL lines=$TOTAL   src/main.rs lines=${MAIN:-0%}  (unit-only main.rs sits at ~8%)"
[[ -n "$MAIN" ]] || { say "main.rs shows NO harness coverage — profile is hollow"; exit 1; }

if [[ -n "$LIVE_BEFORE" ]]; then
  LIVE_AFTER=$(sha256sum "$LIVE_CFG" 2>/dev/null | awk '{print $1}')
  [[ "$LIVE_BEFORE" == "$LIVE_AFTER" ]] || {
    say "LIVE CONFIG MUTATED ($LIVE_CFG) — isolation broken"; exit 1
  }
  say "live config untouched (sha256 verified)"
fi

say ""
say "PTY-COVERAGE: PASS ($NALL profiles merged; TOTAL $TOTAL lines; merged=$MERGED)"
exit 0
