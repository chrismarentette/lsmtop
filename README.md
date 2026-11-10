# lsmtop

A top-style terminal dashboard for [llama.cpp](https://github.com/ggml-org/llama.cpp)
servers and [llama-swap](https://github.com/Mostly-Mainly/llama-swap) routers:
live GEN / PROMPT throughput, KV occupancy, cache %, GPU busy/VRAM, engine
phases and the service journal — in one keyboard-first TUI.

lsmtop measures. Server lifecycle (start/stop/reload) and model downloads
are separate tools with separate UIs — one tool does one thing, well.

Built from scratch in Rust on plain ratatui. Built for measurement
fidelity: rates come from the engine's own counters and journal lines,
graphs are unsmoothed, and the busy-run numbers match what the engine
itself prints.

## Install

**Prebuilt binary** (static, x86_64 Linux — runs on any distro): download
`lsmtop-vX.Y.Z-x86_64-linux.tar.gz` from the
[latest release](https://github.com/chrismarentette/lsmtop/releases),
extract, and run:

```sh
tar xzf lsmtop-v0.2.1-x86_64-linux.tar.gz
./lsmtop
```

**From source**, with a Rust stable toolchain (edition 2021, MSRV 1.77):

```sh
cargo install --git https://github.com/chrismarentette/lsmtop
```

## First run

lsmtop looks for an inference endpoint at `http://localhost:9931`, the
current llama-server default. llama-swap typically listens on `8080` —
point lsmtop at it by editing the config file, created at
`~/.lsmtop/config.json` (mode `0600`) the first time you save anything:

```json
{
  "targets": [
    { "kind": "server", "name": "Server 0", "host": "127.0.0.1", "port": 9931 }
  ]
}
```

Unknown keys are preserved across saves; display options live under
`metrics`. Use the `LSMTOP_CONFIG` environment variable to keep multiple
setups.

## Using it

The full user guide is **[help.md](help.md)** — keys, mouse, every
Settings option, and what the numbers mean. Everything configurable is
also live inside the app under `s` (Settings); changes save and apply
immediately.

## Command line

```sh
lsmtop                              # the dashboard
lsmtop --help                       # usage summary (also -h)
lsmtop --version                    # version (also -V)
lsmtop --headless 60 --dump run.json      # sample for 60 s, write a timeline JSON
lsmtop --headless 60 --target "Server 0"  # pick a target by name (headless mode)
lsmtop --probe-gpu                  # one-shot GPU backend report, then exit
```

Unknown options are rejected with exit code 2, pointing at `--help`.

## Building from source

```sh
cargo build --release          # binary: target/release/lsmtop
```

NVIDIA GPU stats use the `nvml` feature (default). Build with
`--no-default-features` to drop the NVML dependency entirely — AMD/Apple
backends read sysfs directly. A fully static binary:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl --no-default-features
```

## Release quality

Every release passes `scripts/release-gate.sh`: unit suite green, clippy
`-D warnings`, then an llvm-cov gate — line coverage ≥ **80%** with the
interactive harnesses counted in (`--pty-coverage`). A failing stage
aborts, so no release ships over a broken tree. Details:
[docs/release-gate.md](docs/release-gate.md).

## License

Apache-2.0 — see [LICENSE](LICENSE).
