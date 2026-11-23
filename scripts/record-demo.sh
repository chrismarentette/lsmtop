#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${LSMTOP_BIN:-$ROOT/target/release/lsmtop}"
ASSETS="${1:-$ROOT/assets}"
PYTE_SITE="${PYTE_SITE:-$HOME/lsmtop-py/.venv/lib/python3.14/site-packages}"

if [[ ! -x "$BIN" ]]; then
  echo "Building release binary..."
  cargo build --release --manifest-path "$ROOT/Cargo.toml"
fi

if [[ -d "$PYTE_SITE" ]]; then
  export PYTHONPATH="$PYTE_SITE${PYTHONPATH:+:$PYTHONPATH}"
fi

exec python3 "$ROOT/tools/record_demo.py" "$BIN" "$ASSETS"
