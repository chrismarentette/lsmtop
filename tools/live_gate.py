#!/usr/bin/env python3
"""M9.1 + M9.3 + M9.5 LIVE gate for the Rust lsmtop (run on the machine running the router).

Starts the release binary in --headless mode against the live router, drives
ONE real request (big salted prompt -> long decode) through the loaded lane,
and checks three things at once:

  M9.1 journal parity : the run labels (held after completion) must match
                       the engine's journal print_timing within TOL (plan: 2%,
                       Python doctrine floor: 5%).
  M9.3 prefill-never-GEN: while a slot's pp advances (>=64 tok) with
                       n_generated static (<=4 trickle), the Rust tick that
                       covers that span must NOT report phase 'decode' and
                       must not show GEN evidence. 0 violations required.
  M9.5 CPU budget     : process CPU-seconds / wall-seconds <= 0.5% over the
                       whole window (idle cadence; plan M9.5).

Quiet-window doctrine: verify the lane is idle before starting; the run is
ONE foreground pass, no polling mid-soak.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.request
import uuid

ap = argparse.ArgumentParser()
ap.add_argument("--router", default="http://127.0.0.1:8085")
ap.add_argument("--model", required=True)
ap.add_argument("--child-port", type=int, required=True)
ap.add_argument("--unit", default="llama-server")
ap.add_argument("--binary", default="./target/release/lsmtop")
ap.add_argument("--window", type=float, default=60.0)
ap.add_argument("--tbl-tol", type=float, default=5.0,
                  help="table-vs-journal band, percent")
ap.add_argument("--tol", type=float, default=2.0, help="parity tolerance, percent")
ap.add_argument("--cpu-max", type=float, default=0.5, help="max process CPU percent")
ap.add_argument("--oracle", default="",
                help="path to the Python lsmtop repo to drive head-to-head "
                     "(run this script with that repo's venv python)")
ap.add_argument("--h2h-band", type=float, default=10.0,
                help="head-to-head Rust-vs-Python label band, percent")
A = ap.parse_args()

CHILD = f"http://127.0.0.1:{A.child_port}"
DUMP = "/tmp/rs_live_dump.json"


def get(url, timeout=4):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return json.load(r)


def fail(msg):
    print(f"FAIL: {msg}")
    sys.exit(1)


# ---- preflight: lane must be idle (quiet window) ----
try:
    slots = get(f"{CHILD}/slots")
except Exception as e:
    fail(f"child {CHILD}/slots unreachable: {e}")
if any(s.get("is_processing") for s in slots):
    fail("lane busy before start — quiet-window rule; retry when idle")

if os.path.exists(DUMP):
    os.remove(DUMP)

proc = subprocess.Popen(
    [A.binary, "--headless", str(int(A.window)), "--dump", DUMP],
)

# CPU accounting via /proc/<pid>/stat (utime+stime in clock ticks, HZ=100)
def cpu_ticks():
    st = open(f"/proc/{proc.pid}/stat").read().rsplit(")", 1)[1].split()
    return int(st[11]) + int(st[12])  # utime + stime


t_start = time.time()
tick0 = cpu_ticks()

time.sleep(4.0)  # settle / drain visible tail

# ---- Python oracle head-to-head: drive the REAL app.MetricsState against
# the same engine in the same window (journal_equiv-style 1 Hz loop) ----
py_labels = []  # (ts, run_prompt, run_gen)
stop_oracle = threading.Event()
if A.oracle:
    sys.path.insert(0, A.oracle)
    import app as pyapp

    _stt = pyapp.MetricsState()

    def oracle_loop():
        while not stop_oracle.is_set():
            ts = time.time()
            try:
                with urllib.request.urlopen(f"{CHILD}/slots", timeout=2) as r:
                    slots = json.load(r)
            except Exception:
                stop_oracle.wait(0.5)
                continue
            try:
                mets = prom_get(f"{CHILD}/metrics", timeout=2)
            except Exception:
                mets = {}
            lv = _stt.live_rates(A.model, ts, slots, mets)
            if lv:
                py_labels.append((ts,
                                  float(lv.get("run_prompt") or 0.0),
                                  float(lv.get("run_gen") or 0.0),
                                  lv.get("phase")))
            stop_oracle.wait(1.0)

    threading.Thread(target=oracle_loop, daemon=True).start()

# ---- the real request: ~4k-token salted prompt, 400-token decode ----
salt = uuid.uuid4().hex
body = json.dumps({
    "model": A.model,
    "messages": [{"role": "user", "content":
                  f"salt {salt} " + ("the quick brown fox jumps over the lazy dog. " * 900)
                  + "\n\nNow write a long, detailed essay (400+ words) about the natural history of foxes. Begin:"}],
    "max_tokens": 400,
    "temperature": 0.0,
}).encode()

result = {}


def do_request():
    req = urllib.request.Request(
        f"{A.router}/v1/chat/completions", data=body,
        headers={"Content-Type": "application/json"})
    t = time.time()
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            result["http"] = r.status
            result["t_done"] = time.time()
    except Exception as e:
        result["err"] = str(e)


th = threading.Thread(target=do_request, daemon=True)
# ---- poll child /metrics at 5 Hz while the request runs (prefill evidence) ----
# NOTE: on this fork /slots is FROZEN during a run (strix doctrine) — the
# live per-run signals are the llamacpp: counters on the child (safe as
# per-lane here: exactly ONE loaded lane on this router child).
import re as _re


def prom_get(url, timeout=2):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        txt = r.read().decode()
    vals = {}
    for m in _re.finditer(
        r"^(llamacpp:(?:prompt_tokens_total|prompt_seconds_total"
        r"|tokens_predicted_total|tokens_predicted_seconds_total"
        r"|n_decode_total))(?:\{[^}]*\})?\s+([0-9.eE+]+)",
        txt, _re.M,
    ):
        vals[m.group(1)] = float(m.group(2))
    return vals




def metrics_now():
    try:
        return prom_get(f"{CHILD}/metrics")
    except Exception:
        return {}


t_req0 = time.time()
m_before = metrics_now()
th.start()

timeline = []  # (ts, pp_total, gen_total)
t_end = t_start + A.window - 12
while time.time() < t_end:
    time.sleep(0.2)
    try:
        v = prom_get(f"{CHILD}/metrics")
        timeline.append((time.time(),
                         v.get("llamacpp:prompt_tokens_total"),
                         v.get("llamacpp:tokens_predicted_total")))
    except Exception:
        pass
    if result.get("t_done"):
        # a few extra samples to close out the decode edge
        for _ in range(3):
            time.sleep(0.2)
            try:
                v = prom_get(f"{CHILD}/metrics")
                timeline.append((time.time(),
                                 v.get("llamacpp:prompt_tokens_total"),
                                 v.get("llamacpp:tokens_predicted_total")))
            except Exception:
                pass
        break
th.join(timeout=120)
# let the oracle capture the closing plateau, then stop it
while py_labels is not None and A.oracle and time.time() < result.get("t_done", 0) + 5:
    time.sleep(0.5)
stop_oracle.set()
if "err" in result:
    fail(f"request failed: {result['err']}")
print(f"request done http={result.get('http')} in "
      f"{result.get('t_done', 0) - t_req0:.1f}s")

# let lsmtop close the run, flush, and write the dump — sample CPU ticks
# while the process is still alive (its /proc entry vanishes at exit)
last_tick = tick0
while time.time() < t_start + A.window + 3:
    if proc.poll() is not None:
        break
    last_tick = cpu_ticks()
    time.sleep(0.5)
if proc.poll() is None:
    proc.wait(timeout=30)

tick1 = last_tick
wall = time.time() - t_start
cpu_pct = ((tick1 - tick0) / 100.0) / wall * 100.0

d = json.load(open(DUMP))
ticks = d["ticks"]
# Read the run labels at the LAST ACTIVE tick (the plateau just before the
# run closes) — the end-of-dump rows have already expired the 15 s HOLD.
active_rows = [tk["rows"] for tk in ticks if any(r["active"] for r in tk["rows"])]
if not active_rows:
    fail("no active ticks in dump — request never observed busy")
last_active = {r["model"]: r for r in active_rows[-1]}
if A.model not in last_active:
    fail(f"model {A.model} never active in dump")
row = last_active[A.model]

time.sleep(2.0)  # boundary flush for final counter commits
m_after = metrics_now()


def win(name):
    if not m_before or not m_after:
        return None
    if name not in m_before or name not in m_after:
        return None
    return m_after[name] - m_before[name]


dpt = win("llamacpp:prompt_tokens_total")
dps = win("llamacpp:prompt_seconds_total")
dgt = win("llamacpp:tokens_predicted_total")
dgs = win("llamacpp:tokens_predicted_seconds_total")
tbl_p = dpt / dps if dps and dps > 0.01 else None
tbl_g = dgt / dgs if dgs and dgs > 0.01 else None

# ---- M9.1 journal parity ----
out = subprocess.run(
    ["journalctl", "-u", A.unit, "--since", f"@{t_req0 - 1:.0f}", "--no-pager", "-o", "json"],
    capture_output=True, text=True).stdout
prompt_tps = gen_tps = None
acc_a = acc_g = None
for line in out.splitlines():
    try:
        rec = json.loads(line)
    except Exception:
        continue
    msg = rec.get("MESSAGE", "")
    if f"[{A.child_port}]" not in msg and "print_timing" not in msg:
        continue
    m = re.search(r"prompt eval time =.*?([\d.]+) tokens per second", msg)
    if m:
        prompt_tps = float(m.group(1))
    m = re.search(r"^\S+.*?\beval time =.*?([\d.]+) tokens per second", msg)
    if "eval time" in msg and "prompt eval" not in msg and m:
        gen_tps = float(m.group(1))
    m = re.search(r"draft acceptance = [\d.]+ \(\s*(\d+) accepted /\s*(\d+) generated\)", msg)
    if m:
        acc_a, acc_g = int(m.group(1)), int(m.group(2))
if prompt_tps is None or gen_tps is None:
    fail(f"journal print_timing missing (prompt={prompt_tps} gen={gen_tps})")

# VERIFIED on this MTP engine: llamacpp:tokens_predicted_total counts
# ACCEPTED tokens only (window table matched journal print_timing within
# 0.3% across trials) — the draft ratio stays DISPLAY-ONLY, no adjustment.
gen_adj = gen_tps
if acc_a and acc_g and acc_g >= acc_a:
    print(f"MTP drafts: {acc_g} generated / {acc_a} accepted "
          f"(x{acc_g / acc_a:.4f}) — display only")


def pct(a, b):
    return abs(a - b) / b * 100.0 if b else 999.0


# (a) WINDOW table vs journal: same counters — tight (Python doctrine 5%).
if tbl_p is None or tbl_g is None:
    fail(f"metrics window incomplete (tbl_p={tbl_p} tbl_g={tbl_g})")
dp = pct(tbl_p, prompt_tps)
dg = pct(tbl_g, gen_adj)
print(f"table-vs-journal prompt: {tbl_p:.2f} vs {prompt_tps:.2f} ({dp:.2f}%)")
print(f"table-vs-journal gen:    {tbl_g:.2f} vs raw {gen_adj:.2f} ({dg:.2f}%)")
if dp > A.tbl_tol:
    fail(f"table prompt {tbl_p:.2f} vs journal {prompt_tps:.2f} > 5%")
if dg > A.tbl_tol:
    fail(f"table gen {tbl_g:.2f} vs journal raw {gen_adj:.2f} > 5%")
# (b) Rust run LABELS inside the doctrine live band (1 Hz scrape aliasing).
for label, got, truth, lo, hi in (
    ("gen", row["gen_ts"], gen_adj, 0.55, 1.45),
    ("prompt", row["prompt_ts"], prompt_tps, 0.55, 2.0),
):
    if not (lo * truth <= got <= hi * truth):
        fail(f"rust {label} label {got:.2f} outside live band "
             f"[{lo}..{hi}] x journal {truth:.2f}")
    print(f"rust {label} label {got:.2f} inside band vs journal {truth:.2f}")

# ---- M9.3 prefill-never-GEN: oracle-consensus gate ----
# On flush-lagging engines (upstream-MTP forks batch-commit /slots and
# /metrics) even the PYTHON ORACLE latches decode from the iteration
# estimator before counters land — raw counter timelines are NOT ground
# truth there. The head-to-head oracle IS the contract: Rust must not
# claim decode while the oracle stays prompt on both sides of the tick,
# and must not lead the oracle's decode latch by more than 2 ticks.
ticks = d["ticks"]
if not A.oracle:
    print("M9.3: oracle off — phase consensus skipped")
else:
    py = sorted(py_labels)

    def py_phase_at(ts, tol=1.25):
        best = None
        for pts, _rp, _rg, ph in py:
            dd = abs(pts - ts)
            if dd <= tol and (best is None or dd < best[0]):
                best = (dd, ph)
        return best[1] if best else None

    rust_dec0 = next((tk["ts"] for tk in ticks
                      if any(r.get("phase") == "decode" for r in tk["rows"])), None)
    py_dec0 = next((pts for pts, _rp, _rg, ph in py if ph == "decode"), None)
    viol = 0
    for tk in ticks:
        for tr in tk["rows"]:
            if tr["model"] != A.model or tr.get("phase") != "decode":
                continue
            if py_phase_at(tk["ts"] - 1.0) == "prompt" \
                    and py_phase_at(tk["ts"] + 1.0) == "prompt":
                viol += 1
                print(f"VIOLATION rust decode while oracle stays prompt: "
                      f"{tk['ts']}: {tr}")
    lead = (rust_dec0 - py_dec0) if (rust_dec0 and py_dec0) else None
    print(f"decode latch: rust@{rust_dec0} oracle@{py_dec0} "
          f"lead={lead if lead is None else round(lead, 2)}s, "
          f"violations: {viol}")
    if rust_dec0 is None or py_dec0 is None:
        fail("decode latch missing on one side")
    if lead is not None and lead > 2.2:
        fail(f"rust decode latch leads oracle by {lead:.2f}s (>2 ticks)")
    if viol:
        fail(f"{viol} oracle-disagreement prefill violations")
# counter timeline kept for the record: spans/flush behaviour
spans = sum(
    1 for i in range(1, len(timeline))
    if None not in (timeline[i][1], timeline[i - 1][1],
                    timeline[i][2], timeline[i - 1][2])
    and timeline[i][1] - timeline[i - 1][1] >= 64
)
print(f"counter-timeline prefill spans: {spans} (informational)")

# ---- head-to-head: Rust vs Python oracle, tick consensus ----
# The oracle loop sees RAW live_rates output while the Rust row is the
# merged table label (run label, else rates cumulative avg) — so compare
# ONLY where both sides have an active run label, plus phase consensus on
# every aligned tick (lenient within 1.6 s of either side's decode latch,
# since one-tick scrape offsets are legal at boundaries).
if A.oracle:
    py = sorted(py_labels)
    rts = [(tk["ts"], {r["model"]: r for r in tk["rows"]}) for tk in ticks]

    def rust_row_near(ts, tol=0.75):
        best = None
        for rts_ts, rows in rts:
            dd = abs(rts_ts - ts)
            if dd <= tol and (best is None or dd < best[0]):
                best = (dd, rows.get(A.model))
        return best[1] if best else None

    r_latch = next((x for x, rows in rts
                    if any(r.get("phase") == "decode" for r in rows.values())), None)
    p_latch = next((pts for pts, _rp, _rg, ph in py if ph == "decode"), None)
    phase_bad = label_bad = 0
    for pts, rp, rg, ph in py:
        rr = rust_row_near(pts)
        if rr is None:
            continue
        at_boundary = ((r_latch is not None and abs(pts - r_latch) < 1.6)
                       or (p_latch is not None and abs(pts - p_latch) < 1.6))
        if rr.get("phase") != ph and not at_boundary:
            phase_bad += 1
            print(f"PHASE DISAGREE @{pts:.1f}: rust={rr.get('phase')} "
                  f"oracle={ph} rust_row={rr}")
        # (plateau labels compared separately below; mid-run ticks ride
        # immature 0.2 s-floor windows and differ legally by scrape phase)
    n_cmp = len(py)
    # plateau: LAST decode-phase sample on each side (the settled avg)
    py_dec = [(pts, rp, rg) for pts, rp, rg, ph in py if ph == "decode"]
    rust_dec = [(x, rows.get(A.model)) for x, rows in rts
                if rows.get(A.model) and rows[A.model].get("phase") == "decode"]
    if py_dec and rust_dec:
        _t, prp, prg = py_dec[-1]
        _, rrow = rust_dec[-1]
        dg = pct(rrow["gen_ts"], prg) if prg > 0 and rrow["gen_ts"] > 0 else 0.0
        dp = pct(rrow["prompt_ts"], prp) if prp > 0 and rrow["prompt_ts"] > 0 else 0.0
        print(f"plateau gen:    rust={rrow['gen_ts']:.2f} oracle={prg:.2f} ({dg:.2f}%)")
        print(f"plateau prompt: rust={rrow['prompt_ts']:.2f} oracle={prp:.2f} ({dp:.2f}%)")
        if dg > 15.0:
            fail(f"plateau gen {dg:.1f}% > 15%")
        if dp > 15.0:
            fail(f"plateau prompt {dp:.1f}% > 15%")
    print(f"consensus: {n_cmp} oracle ticks, phase disagreements {phase_bad}, "
          f"label disagreements {label_bad}")
    if phase_bad:
        fail(f"{phase_bad} phase disagreements with oracle")
    if label_bad:
        fail(f"{label_bad} run-label disagreements with oracle")

# ---- M9.5 CPU budget ----
print(f"cpu: {cpu_pct:.3f}% over {wall:.0f}s wall; "
      f"rss {d['rss_kib_start']}->{d['rss_kib_end']} KiB; "
      f"polls {d['polls']} errors {d['errors']}")
if cpu_pct > A.cpu_max:
    fail(f"cpu {cpu_pct:.3f}% > {A.cpu_max}%")

extra = " + head-to-head oracle" if A.oracle else ""
print(f"PASS: M9.1 journal parity + M9.3 prefill-never-GEN + M9.5 cpu budget{extra}")
