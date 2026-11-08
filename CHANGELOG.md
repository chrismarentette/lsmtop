# Changelog

## v0.2.1 — 2026-11-08
The gutter-and-veracity release: the scale ruler learned to move, single
lines learned to grow, and the repo grew a real release gate.

- DEFAULT PORT: fresh configs target `127.0.0.1:9931`, matching the
  current llama-server default (llama-swap users keep pointing at their
  own port via `targets`).
- CLI FLAGS: `-h/--help` and `-V/--version` per GNU/clap convention
  (stdout, exit 0); unknown arguments fail fast with exit 2 pointing at
  `--help`. Pure argv preflight in `src/cli.rs`, unit-tested.
- DOCS SPLIT + BINARY: README is getting-started only; help.md is the
  finalized user manual (keys, mouse, options, metric meanings). The
  release ships a static x86_64 Linux binary asset (static-pie musl,
  NVIDIA stats via runtime driver load).

- SCALE GUTTER: `metrics.scale` right|left|both|off (Settings chips,
  RIGHT is the new default). The three-column ruler mirrors; graph data
  is never drawn behind it; the live value retired from riding the trace
  and now rides the gutter vertically (both edges on `both`, gone on
  `off`). Locked by ink-guard + four position snapshots.
- SPARKLINE MINI GRAPHS fixed and lawed: block eighths grow again (a
  double normalization had flattened every spark to the rail); gradient
  fades LEFT-to-RIGHT across the whole single line, flow pulses travel
  RIGHT-to-LEFT with time, solid colors each glyph by value; `chars:
  line` sparks use a stroke-position ladder (bottom / middle / axis /
  top rules). Snapshots lock all three styles.
- KEY LAW extracted to `src/input.rs` as pure functions (CSI-u SHIFT
  normalization, quit chord, the p/g/v/u + P/G/V/U table) — eight-key
  table + exclusions locked by unit tests.
- Release gate `scripts/release-gate.sh`: ordered unit → lint/build →
  llvm-cov coverage step, with an opt-in instrumented-pty stage that
  counts the pty harnesses' screen time; measured 82.7% lines merged.
  `docs/release-gate.md` is canonical.
- Coverage arc: style/net/gpu/svc-logs/input law tests (gpu.rs 8% ->
  67%, style + input 100%); the journal-colour oracle lock pins the
  ported bare-number repaint quirk as law; lib suite 45 -> 77.
- the release gate counts pty harness time: `scripts/release-gate.sh
  --pty-coverage` (or `PTY_COVERAGE=1`) merges the instrumented
  `ui_verify_v3/v4` + `sparkproof` LLVM profiles with the test suite's into
  ONE llvm-cov report before `--fail-under-lines` is evaluated (clean
  profraw set -> `--no-report` suite run -> harness against the same binary
  -> `cargo llvm-cov report`). Harnesses must run under that same
  instrumented build (profile-hash identity; the runner refuses an
  uninstrumented binary and 0-byte profiles, and `q`-quits to flush), each
  in its own fakehome with `HOME`+`LSMTOP_CONFIG` pointed into it and the
  live config sha256-guarded. Merged TOTAL **82.74% lines** vs 67.79%
  unit-only (`main.rs` 7.93% -> 54.88%): the 80 target goes green with the
  opt-in, no exclusions, plain runs byte-identical to before. Verified
  clean-room: a `git archive` checkout (no `.git`) at the untouched default
  floor passed the full gate — 22 suites green, 26 raw profiles merged,
  TOTAL 82.74% lines, live config untouched — while the same room run plain
  measured 67.79% and failed at 80, the documented unit-only undercount.
  Harness runner fix: its orphan `pkill -f` is now `^`-anchored —
  unanchored, a caller passing `--bin` made the pattern match the runner's
  own argv and SIGTERM it at exit.
- release gate documented: `docs/release-gate.md` + a README section cover
  the llvm-cov 80% line gate (`--fail-under-lines`), its placement after
  P1+P2, the instrumented-pty counting path that meets 80 (82.74%
  measured), the expected 82–85% ceiling, and the known-uncoverable areas
  (TTY event loop, NVML/fork glue); `scripts/release-gate.sh` landed in
  this batch. With the pty path measured green, the interim-floor wording
  is retired: 80 is the default and the shipping number, `COV_MIN_LINES`
  below it is an explicit escape hatch for pty-less environments only, and
  file exclusions + branch gating stay prohibited doctrine.
- coverage arc P1–P2: the key law now lives in `src/input.rs` as pure
  functions (CSI-u SHIFT normalization, quit chord, the `p g v u` /
  `P G V U` table) with the eight-key table + exclusions locked;
  `gpu.rs` grew fs_root-injected sysfs fixtures and N/A-honest smi
  parsers; `svc/logs` carries an EXACT full-run oracle lock (the
  bare-number repaint quirk IS the law); style/net/chips/journal_cmd
  laws added. Lib suite 53 -> 75.
- sparkline mini graphs fixed and lawed: block eighths were dead
  (double normalization - `to_columns` already returns 0..1); flow pulses
  now travel right-to-left with time; gradient pinned left-to-right;
  `chars: line` sparks use a stroke-position ladder (⎽ ⎼ ─ ‾).
  `tools/sparkproof.py` + spark snapshots lock all three styles.
- SCALE GUTTER law: `metrics.scale` right|left|both|off (Settings chips,
  RIGHT is the new default). The three-column ruler mirrors; graph data
  is never drawn behind it; the live value retired from riding the trace
  and now rides the gutter vertically (both edges on `both`, gone on
  `off`). Locked by ink-guard + four position snapshots; proof:
  `tools/scaleproof.py`.

