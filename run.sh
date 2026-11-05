#!/usr/bin/env bash
# lsmtop launcher — Rust build (metrics-first llama.cpp/llama-swap monitor)
set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$SCRIPT_DIR/target/release/lsmtop"
if [[ ! -x "$BIN" ]]; then
    echo "building release..."
    (cd "$SCRIPT_DIR" && cargo build --release)
fi
exec "$BIN" "$@"
