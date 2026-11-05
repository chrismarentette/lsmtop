#!/usr/bin/env bash
set -euo pipefail
mkdir -p tests/fixtures
curl -sf http://127.0.0.1:8085/models > tests/fixtures/models.json
PORT=$(python3 -c '
import json
try:
    d = json.load(open("tests/fixtures/models.json"))
    for m in d.get("data", []):
        status = m.get("status", {})
        if status.get("value") == "loaded":
            args = status.get("args", [])
            if "--port" in args:
                idx = args.index("--port")
                print(args[idx+1])
                exit(0)
except Exception:
    pass
')
if [ -n "$PORT" ]; then
    echo "Found loaded port: $PORT"
    curl -sf "http://127.0.0.1:${PORT}/slots" > tests/fixtures/slots.json
    curl -sf "http://127.0.0.1:${PORT}/metrics" > tests/fixtures/metrics.prom
else
    echo "No loaded port found!"
fi
echo "captured: $(ls tests/fixtures)"
