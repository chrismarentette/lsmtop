# The release gate

`scripts/release-gate.sh` is the one command every release passes: unit
suite, lint, release build, and an llvm-cov line-coverage check, run in
strict order. A failing stage aborts the run — later stages never execute
against a broken tree.

## Stages (order is the law)

| Stage | What runs | Red means |
| --- | --- | --- |
| P1 | `cargo test --no-fail-fast` — every suite must report `ok` | the suite is broken |
| P2 | `cargo clippy --all-targets -- -D warnings` and `cargo build --release` | lint or the shipping build is broken |
| COVERAGE | `cargo llvm-cov --summary-only --fail-under-lines <floor>` | measured line coverage is under the enforced floor |

The coverage step sits after P1+P2 deliberately and structurally: the script
aborts on the first failing stage, so coverage can only ever run over a tree
whose full unit suite and lint + release build are already green (a
planted-failure probe proves the abort: a red P1 prints
`coverage step NOT run`). A healthy percentage measured over a broken suite
is worthless.

With the pty opt-in (next section) the COVERAGE stage additionally counts the
pty harnesses' screen time, evaluated as `cargo llvm-cov report
--summary-only --fail-under-lines <floor>` over a merged profile set.

## Counting the pty harnesses (`--pty-coverage`)

Unit-only coverage has an instrumentation gap, not a test gap: the pty
harnesses (`tools/ui_verify_v3.py`, `tools/ui_verify_v4.py`,
`tools/sparkproof.py`) exercise the `main.rs` event loop and the gpu fork
glue for real, but their runs never contributed profile data. The opt-in
converts that screen time into counted coverage:

```sh
scripts/release-gate.sh --pty-coverage      # or: PTY_COVERAGE=1 scripts/release-gate.sh
```

How the merge works (verified against cargo-llvm-cov 0.9.1 — its `report`
subcommand merges every top-level `*.profraw` of `target/llvm-cov-target`
and only then applies the threshold; this version has no `--add-profdata`):

1. `cargo llvm-cov clean --profraw-only` — hermetic input set. `--no-report`
   implies `--no-clean`, so stale raw profiles from earlier runs would
   otherwise merge in and inflate the number.
2. `cargo llvm-cov --no-report` — the instrumented build plus the full suite;
   the raw profiles accumulate on disk.
3. `scripts/pty-coverage-harness.sh --bin <that same binary>` — the three
   harnesses against the exact binary step 2 built, each in its own fakehome
   with its own `LLVM_PROFILE_FILE` (see the two doctrine subsections
   below). The runner merges its raw profiles itself with
   `llvm-profdata merge -sparse` into `<out>/pty-harness.profdata` and
   self-proves them: `llvm-cov report` against that merged file must show
   real `main.rs` coverage (pty-only TOTAL 59.71% lines), because a report
   where the harness runs counted nothing is a silent lie.
4. The harness raws are staged next to the test raws and
   `cargo llvm-cov report --summary-only --fail-under-lines <floor>` merges
   all of them into ONE report; the threshold applies to that merged TOTAL
   (26 raw profiles in the clean-room run: the suite's plus five from the
   harnesses — one per v3/v4 boot, three from sparkproof's styles).

### The harnesses must run under the instrumented binary

Profile data only merges when every run came from the *same* instrumented
build: llvm's profiling records identify functions by hash, and a profile
from any other build contributes nothing (or noise) to the report. Two
mechanisms enforce this:

- The gate passes the harness runner its **own** `cargo llvm-cov` binary
  (`target/llvm-cov-target/debug/lsmtop`) via `--bin`, so harness profiles
  and test profiles share build identity. Standalone, the runner defaults
  to building that same artifact; explicit `--bin` is the recommended form.
- The runner **refuses an uninstrumented binary**: it scans the ELF for
  `__llvm_prf`/`__llvm_profi` symbols and aborts rather than run a build
  that would emit nothing — `target/release/lsmtop` is explicitly rejected.

Profile *flushing* is the other half of the law (probed on rustc 1.98): the
instrumentation runtime writes profiles on clean exit and on SIGSEGV only —
SIGTERM and SIGABRT leave 0-byte profraws, and continuous mode (`%c`) fails
to write at all. So every harness must **`q`-quit the app** (v3/v4 quit
proofs; sparkproof sends `q` and polls `waitpid`, TERM only as fallback),
and the runner counts only non-empty (`-size +0c`) emitted profiles — a
harness that PASSES but leaves a 0-byte profile means the child was killed,
not quit, and the runner fails the stage.

### Fakehome + `LSMTOP_CONFIG`: scratch copies only

The harnesses click, type and press `esc` — the app commits fields on blur
and saves config — so a harness pointed at the real `~/.lsmtop/config.json`
would edit a live config. The runner therefore gives **every harness a
fresh fakehome** (`mktemp -d`, never reused across harnesses — one shared
leftover once carried `tick_ms=1950` and failed v4's tick step), points
**both `HOME` and `LSMTOP_CONFIG` into it**, and guards defense-in-depth:
the live `~/.lsmtop/config.json` is sha256'd before and after the run and
any change fails the stage (`LIVE CONFIG MUTATED`).

`--report-only` with the pty opt-in re-reads the raw profiles the last full
run left behind; if the last full run was unit-only, the script says so
loudly (`no pty harness raw profiles on disk`) instead of pretending the
re-check includes pty counts.

## The 80% coverage gate

- **Contract:** TOTAL line coverage ≥ **80%**, the default and the shipping
  number. The exact command is `cargo llvm-cov --summary-only
  --fail-under-lines 80` — cargo-llvm-cov (0.9.x) has **no bare
  `--fail-under`**, so pin the metric name.
- **Metric pinned to LINES.** Regions and funcs read differently at the same
  number; branches are not instrumentable on stable Rust and are **never
  gated**. Their column prints `-` on the TOTAL row, which makes the lines
  percentage the **last field ending in `%`** on that row — the script
  parses right-to-left for the last `…%` token, because `$NF` steals the
  `-` of the branches column.
- **`COV_MIN_LINES`** overrides the enforced floor (default `80`). The pty
  form meets 80 with no override, so lowering the floor is a choice, not a
  stage of the project: any floor below the target appears in every
  transcript line (`floor=… target=80%`), keeping the gap visible instead
  of hidden.
- **Prohibited, permanently:** file **exclusions** (excluding `main.rs` +
  `gpu.rs` reads 87% today — that is metric gaming) and **branch gating**
  (branches show 0 on stable; a gate on them gates nothing).

### What each mode measures (all at `4d68a61`)

| Form | Measured TOTAL lines | vs 80 |
| --- | --- | --- |
| `--pty-coverage` (unit suite + harness counts merged) | **82.74%** | PASS |
| plain run (unit suite only) | 67.79% | FAIL |
| harnesses alone (pty-only profile) | 59.71% | reference |

The plain run's FAIL is an honest statement about an undercount, not the
bar: without the opt-in the harnesses' screen time contributes no profile,
so the merged-instrumentation reality (82.74%) reads as unit-only 67.79%.
The release path is the pty form; 80 stays fixed and green there.

Verified **clean-room**: a `git archive` checkout of the same tree (no
`.git`) ran `PTY_COVERAGE=1` with no `COV_MIN_LINES` override to
`RELEASE-GATE: PASS` (exit 0) — 22 unit suites green, 26 raw profiles
merged, TOTAL 82.74% lines, live config sha256-untouched. The same room run
plain measured 67.79% and failed at 80, exactly as documented above.

An environment that genuinely cannot run the pty stage (no pyte venv, port
8123 occupied) may pin an explicit lower floor so the gate still runs —
the floor shows in every transcript line — but that number is an escape
hatch for missing tooling, never the target:

```sh
COV_MIN_LINES=65 scripts/release-gate.sh    # pty stage impossible here (plain = 67.79%)
```

Raise any pinned floor alongside every coverage landing; the
`--pty-coverage` form is the one that demonstrates the 80 contract.

### Expected ceiling: 82–85%

100% is neither the goal nor attainable: the TTY event loop, NVML FFI and
subprocess bodies are roughly 15–18% of the tree and stay permanently out of
reach of the coverage profile. The realistic ceiling after the arc is
**82–85% lines**. 80 is a demanding bar just below that ceiling — the margin
exists to absorb new glue code, not to be rested on. The merged pty number
(82.74%) already sits inside that band.

## Known uncovered areas

Measured unit-only, then merged with the pty harness counts (clean-room run):

- **`main.rs`** — 7.93% → **54.88% lines**. The event loop and terminal
  bootstrap were the instrumentation gap; the harness boots the real screen
  and drives keys/clicks, so most of the loop now counts. The uncovered
  remainder is the long tail of terminal/panic paths the harnesses do not
  drive (resize storms, exotic key encodings, error exits).
- **`gpu.rs`** — 67.34% → **86.84% lines**. Booting the app runs the real
  `nvidia-smi`/sysfs glue once; the NVML FFI bindings and error branches
  stay unit- and pty-unreachable. The parsers behind them are
  fixture-tested (including drivers that report wattage as `N/A` — honest
  absence, never a fake zero).

## Running the gate locally

Prerequisites (dev box or CI):

```sh
cargo install cargo-llvm-cov && rustup component add llvm-tools-preview
```

The pty form additionally needs `~/lsmtop-py/.venv` (pyte) and a free port
8123 (sparkproof's fake router); the harness runner preflights both and its
failure aborts the gate at COVERAGE.

```sh
scripts/release-gate.sh --pty-coverage      # the release path: 80 contract met (82.74% lines)
PTY_COVERAGE=1 scripts/release-gate.sh      # same, via environment
scripts/release-gate.sh                     # unit-only profile (67.79%; red at the default 80 — see "What each mode measures")
scripts/release-gate.sh --report-only       # re-check the floor against the last coverage profile — no rebuild, no re-run
scripts/release-gate.sh --log gate.log      # append the full stage transcript to a file
```

A full run re-runs the entire suite instrumented; the coverage pass alone is
≈1.5 minutes, the whole gate a few minutes on a warm cache (the pty harness
stage adds ≈1 minute; the clean-room verification completed the full
pty-enabled gate in ≈2 minutes). The script resolves the repo root from its
own path, so it also works on `git archive` / CI checkouts without `.git`.
Exit code is 0 only when every stage passed, and the verdict is always one
greppable line:

```
COVERAGE RESULT: lines=82.74% floor=80% target=80% -> PASS
RELEASE-GATE: PASS (stages P1,P2,COVERAGE all green; coverage floor 80% lines, target 80%, pty-counting ON)
```

## What a failure means

- **`FAIL at P1`** — a test suite is red. Ship nothing; no coverage number
  was produced, and it would not mean anything over a broken suite anyway.
- **`FAIL at P2`** — clippy (`-D warnings`) or the release build broke.
  Coverage was again not run.
- **`FAIL at COVERAGE` with a `PTY:` line above it** — with the opt-in, a
  broken instrumented test run, harness, or profile stage aborts before the
  threshold is evaluated (`merged coverage NOT evaluated`): a number that
  silently dropped the pty counts would be a lie. Fix the harness or the
  precondition (venv, port 8123, stale binary) and re-run.
- **`FAIL at COVERAGE`** — measured line coverage is under the enforced
  floor: the release is blocked. If the run was plain (unit-only), the
  first move is to re-run with `--pty-coverage`, which meets the bar; if a
  real drop occurred, extract more logic into pure functions and test it
  there (the `src/input.rs` move is the pattern). Never exclude files,
  never gate branches, never lower the 80 target — an explicitly pinned
  `COV_MIN_LINES` exists only for environments that cannot run the pty
  stage, and its value stays visible in the transcript.
