# lsmtop

A top-style terminal dashboard for [llama.cpp](https://github.com/ggml-org/llama.cpp)
servers and [llama-swap](https://github.com/Mostly-Mainly/llama-swap) routers:
live GEN / PROMPT throughput, KV occupancy, cache %, GPU busy/VRAM, engine
phases, service management, log journal and an HF model downloader — in one
keyboard-first TUI.

This is the Rust implementation, a from-scratch port of
the original Python/Textual app built for measurement fidelity and <0.1% idle
CPU. The engine-rate math is journal-synced: the busy-run labels in the lane
table match `print_timing` lines in the engine journal (verified to 0.3% on
NVIDIA + MTP spec-decode forks, live, as a release gate).

## What the numbers mean

- **GEN t/s** — decode throughput from the engine's own counters
  (organic `n_generated` growth, or the calibrated `n_decode_total`
  estimator where the field is missing). Prompt processing is *never*
  counted as generation; on chunked engines the pending prompt queue
  (`prompt_total - processed`) is explicitly not generated tokens.
- **PROMPT t/s** — prompt-eval rate (cumulative average over the busy run —
  the exact number the engine prints at completion).
- **KV** — context occupancy `prompt_tokens / n_ctx`.
- **CACHE** — prefix-cache hit ratio over the run.
- Graphs are per-tick engine truth, one sample per column, interval-max
  squish for older history — no smoothing that hides real spikes, no
  plateaus that survive a stop.

## Keys

| key   | action                                   |
|-------|------------------------------------------|
| `a`   | lanes / dashboard                         |
| `g`   | llama-server manager (units + owned)      |
| `l`   | journal / log viewer                      |
| `h`   | HF-CLI: list & download GGUF repos        |
| `s`   | settings (graph chars/fill/colors/ramp)   |
| `q`   | quit (owned servers keep running)         |

Sub-pages use `r` to refresh and `Esc` to go back. System-scope systemd
actions are printed as copy-paste commands — lsmtop never sudo's for you.

## Build & run

```sh
cargo build --release          # binary: target/release/lsmtop
./target/release/lsmtop
```

Requires a Rust stable toolchain (edition 2021, MSRV 1.77). NVIDIA GPU stats
need the `nvml` feature (default); build with `--no-default-features` to drop
the NVML dependency entirely (AMD/Apple backends read sysfs directly and are
GPU-poll-free when no GPU is present).

Static musl binary:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl --no-default-features
```

## Configuration

`~/.lsmtop/config.json` (created on first save, mode `0600`).
Shared with the Python app during dual-runs; unknown keys are preserved.
Targets, ports and display options are edited in the Settings screen (`s`).
The Hugging Face token (Settings, or `HF_TOKEN`, or `~/.cache/huggingface/token`)
is only ever passed to the `hf` CLI via the environment, never on argv, and
never printed.

## Headless / diagnostics

```sh
lsmtop --headless 60 --dump run.json   # 1 Hz timeline JSON, no terminal
lsmtop --probe-gpu                     # one-shot GPU backend probe
```

## License

Apache-2.0 — see [LICENSE](LICENSE).
