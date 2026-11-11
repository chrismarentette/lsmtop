# lsmtop performance gate

Third pillar next to coverage (`scripts/release-gate.sh --pty-coverage`) and
validation (the test suites + pty harnesses): **performance**. Run it on
demand — nothing here is scheduled:

```bash
scripts/perf-check.sh [--log FILE]     # PERF-GATE: PASS|FAIL at <stage>
```

Measured artifact: the plain `--release` binary. NEVER run the perf gate
against an llvm-cov instrumented build — the coverage counters change both
speed and RSS identity.

## Gates and thresholds

| Stage | What it asserts | Floor (env escape hatch) |
|---|---|---|
| P1 | `cargo build --release` of the measured binary | — |
| B | full 120×36 dashboard frame (1k→100k history, 1→11 lanes) | 8 ms (`PERF_MAX_FRAME_MS`) |
| B | buffer diff of an UNCHANGED frame == 0 cells (asserted in-bench; a nonzero diff panics) | — |
| B | worst-case buffer diff pass | 5 ms (`PERF_MAX_DIFF_MS`) |
| B | sampler ingest per tick | 50 000 ticks/s (`PERF_MIN_INGEST_TPS`) |
| S | 200k-tick stress: every `hist::Ring` stays at its fixed cap, rows/maps bounded | — |
| F | floodproof (pty): oversized payloads, 2k lines/s journal, ~200 input events/s; live RSS ceiling + flatline; `q` reap ≤ 4 s | 30 MB max / 5 MB growth (`PERF_MAX_RSS_MB`, `PERF_MAX_RSS_GROWTH_MB`) |

Doctrine (same as the coverage gate): floors are **measured minus margin**,
never aspirational; changing a floor requires changing this table and the
measured baselines below in the same commit.

The 16.6 ms frame budget is the terminal ideal; lsmtop repaints once per tick
(≥500 ms), so the 8 ms floor keeps even the worst frame an order of magnitude
inside one tick. Frame cost must stay flat as history grows — that is why the
1k/10k/100k groups all carry the same floor.

## Measured baselines

Measured 2026-11-10 on the dev box (release build, plain hardware while the box
also hosts the model router — numbers are conservative):

| Metric | Measured |
|---|---|
| frame/history/1k-1lane | 0.151 ms median |
| frame/history/10k-1lane | 0.225 ms median |
| frame/history/100k-1lane | 1.058 ms median |
| frame/history/100k-11lanes | 1.082 ms median |
| diff/frame | 0.057 ms median (unchanged frame diffs 0 cells) |
| ingest/tick | 29,820 ticks/s (floor set to 12,000 = measured minus ~2.5x margin; real workload is 2 ticks/s) |
| floodproof RSS max / growth | 13.6 MB / 0.0 MB (first run) |
| floodproof quit after 1k-event flood | 50 ms |

## Profiling recipes

CPU flamegraph (frame pointers are passed via RUSTFLAGS — the Cargo manifest
key is unstable and its lone warning fails the clippy `-D warnings` gate):

```bash
RUSTFLAGS="-C force-frame-pointers" cargo build --profile profiling --bin lsmtop
# against the fake router, easiest under the floodproof timeline:
perf record -g --call-graph dwarf -p $(pgrep -f target/profiling/lsmtop) -- sleep 20
perf script | inferno-collapse-perf > flame.folded   # or: perf report
```
`cargo flamegraph` (if installed) does the same in one step:
`cargo flamegraph --profile profiling --bin lsmtop -- --help-is-not-a-daemon` —
note lsmtop needs a live target; point it at the fake router via a scratch
`LSMTOP_CONFIG`. If `perf` is blocked, check `kernel.perf_event_paranoid`.

Allocation churn on the hot path is a Phase-2 item (`dhat` behind an opt-in
feature; the floodproof RSS flatline is the cheap standing proxy until then).

## Rejected / adapted from the original proposal

* `tracing` + `console-subscriber`: lsmtop has **no async runtime** (plain
  `std::thread` + mpsc) — there are no tasks or channels to trace there.
* SSE/chat-stream ingestion: lsmtop is a **pull poller** (one scrape set per
  tick of `/models`, `/slots`, `/metrics`); the flood scenario prices oversized
  scrape payloads instead.
* `tikv-jemalloc-ctl`: external allocator changes the artifact identity;
  `dhat` (Phase 2) profiles the real allocator in place.
* 100k "data points" as a render knob: history lives in fixed-cap rings; the
  honest knobs are ring capacity, fed tick count, and lane count — the benches
  vary exactly those and assert frame cost stays flat.

RSS (floodproof, 2026-11-09): peak 15.4 MB, growth 0.0 MB over the
full payload+journal+input flood, quit-reap 50 ms. Floor 30 MB / 5 MB growth.

Baseline hardware: the dev box (i5-12500B, DDR4, release build, criterion
measurement-time default). Floors = measured-minus-margin, never aspirational
(maintainer doctrine 2026-11-10): re-measure and re-tighten when the hot path
changes, loosen only with a written reason here.
