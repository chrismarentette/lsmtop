#!/usr/bin/env python3
"""M9 offline parity golden generator (dev tool — regenerates the committed
fixtures; the fixtures in tests/fixtures/ are the public contract).

Drives the reference MetricsState.rates/live_rates + curve_smooth through a
deterministic multi-tick scenario (seed → prefill → decode → idle-hold →
two-slot 'active' lane → failed scrape → recovery) and writes:

  tests/fixtures/engine_scenario.json  (inputs the Rust test replays)
  tests/fixtures/engine_golden.json    (expected rows + curve per tick)

The reference engine functions are IMPORTED from a reference checkout
(argument, required — never transcribed into this repo):

  python3 tools/paint_ref.py <path-to-reference-implementation>
"""
import json
import pathlib
import sys

LSMTOP = pathlib.Path(sys.argv[1]).expanduser()  # reference checkout, required
sys.path.insert(0, str(LSMTOP))
from app import MetricsState, curve_smooth, cache_pct  # reference engine

M = "llamacpp:"


def mets(tok, tsec, ptok, psec):
    return {
        M + "tokens_predicted_total": tok,
        M + "tokens_predicted_seconds_total": tsec,
        M + "prompt_tokens_total": ptok,
        M + "prompt_seconds_total": psec,
    }


def sl(id_, pp, pt, gen, proc, nctx=8192, cache=None):
    d = {
        "id": id_,
        "is_processing": proc,
        "n_ctx": nctx,
        "n_prompt_tokens": pt,
        "n_prompt_tokens_processed": pp,
        "n_generated_tokens": gen,
    }
    if cache is not None:
        d["n_prompt_tokens_cache"] = cache
    return d


T, LANE, PORT = "golden-lane", "golden-lane", 40001

# (ts, slots|None, metrics|None)
TICKS = [
    # seed: loaded, idle
    (1000.0, [sl(0, 500, 540, 10, False, cache=108)], mets(100.0, 2.0, 500.0, 1.0)),
    # prefill burst 1200 tok
    (1001.0, [sl(0, 1700, 1740, 10, True, cache=108)], mets(100.0, 2.0, 2240.0, 1.35)),
    # prefill continues (chunked)
    (1002.0, [sl(0, 3400, 3440, 10, True, cache=108)], mets(100.0, 2.0, 3440.0, 1.70)),
    # decode 80 t/s (organic)
    (1003.0, [sl(0, 3400, 3440, 90, True, cache=108)], mets(180.0, 3.0, 3440.0, 1.70)),
    (1004.0, [sl(0, 3400, 3440, 170, True, cache=108)], mets(260.0, 4.0, 3440.0, 1.70)),
    # slower decode 40 t/s
    (1005.0, [sl(0, 3400, 3440, 210, True, cache=108)], mets(300.0, 5.0, 3440.0, 1.70)),
    # run over — slots idle, counters frozen
    (1006.0, [sl(0, 3400, 3440, 210, False, cache=108)], mets(300.0, 5.0, 3440.0, 1.70)),
    # still idle (tests the 15 s HOLD across a 4 s gap)
    (1010.0, [sl(0, 3400, 3440, 210, False, cache=108)], mets(300.0, 5.0, 3440.0, 1.70)),
    # multi-slot reality (shared router): slot0 prefilling WHILE slot1 decodes
    (1011.0,
     [sl(0, 5000, 5040, 210, True, cache=108), sl(1, 100, 140, 50, True, cache=0)],
     mets(340.0, 5.5, 4440.0, 1.95)),
    (1012.0,
     [sl(0, 6600, 6640, 210, True, cache=108), sl(1, 100, 140, 90, True, cache=0)],
     mets(380.0, 6.0, 5440.0, 2.20)),
    # child scrape FAILED (slots and metrics gone) — table holds, curve rails
    (1013.0, None, None),
    # scrape back, both slots done — still holds, KV from occupancy
    (1014.0,
     [sl(0, 6600, 6640, 210, False, cache=108), sl(1, 100, 140, 90, False, cache=0)],
     mets(380.0, 6.0, 5440.0, 2.20)),
]


def render_tick(st, sm, ts, lanes):
    """Mirror of _render_snap's per-tick merge (app.py:3165-3214)."""
    rows = []
    gen_live = prompt_live = 0.0
    kv_max = 0.0
    for mid, r in lanes.items():
        mts = r["m"]
        cur = None
        if mts is not None:
            cur = (
                float(mts.get(M + "tokens_predicted_total", 0.0) or 0.0),
                float(mts.get(M + "tokens_predicted_seconds_total", 0.0) or 0.0),
                float(mts.get(M + "prompt_tokens_total", 0.0) or 0.0),
                float(mts.get(M + "prompt_seconds_total", 0.0) or 0.0),
            )
        proc = (
            sum(1 for s in r["slots"] if s.get("is_processing"))
            if r["slots"] is not None
            else None
        )
        rr = st.rates(mid, ts, cur, processing=proc)
        lv = st.live_rates(mid, ts, r["slots"], r["m"])
        if lv and lv["phase"] in ("prompt", "decode", "active"):
            rp, rg = lv.get("run_prompt"), lv.get("run_gen")
            if rp is not None:
                rr["prompt"] = rp
            if rg is not None:
                rr["gen"] = rg
        if lv:
            rp = lv.get("run_prompt")
            ph = lv["phase"]
            pv = lv["prompt"]
            if rp and ph in ("prompt", "active"):
                pv = max(pv, rp)
            gv = max(lv["gen"], lv["est"])
            if ph == "prompt":
                gv = 0.0
            if ph not in ("prompt", "decode", "active"):
                gv = pv = 0.0
            gen_live = max(gen_live, gv)
            prompt_live = max(prompt_live, pv)
            r["_kv"] = lv["kv"]
        else:
            r["_kv"] = 0.0
            for s in r["slots"] or []:
                nctx = s.get("n_ctx") or 0
                if nctx:
                    r["_kv"] = max(
                        r["_kv"], 100.0 * (s.get("n_prompt_tokens", 0) or 0) / nctx
                    )
        kv_max = max(kv_max, r["_kv"])
        rows.append(
            {
                "model": mid,
                "gen_ts": rr.get("gen") or 0.0,
                "prompt_ts": rr.get("prompt") or 0.0,
                "cache_pct": cache_pct(r["slots"]),
                "kv": r["_kv"],
                "active": bool(proc),
                "phase": lv["phase"] if lv else None,
            }
        )
    gv, pv = curve_smooth(sm, gen_live, prompt_live)
    return rows, [gv, pv]


def main():
    st = MetricsState()
    sm = {"gen": 0.0, "prompt": 0.0}
    scenario, golden = [], []
    for ts, slots, metrics in TICKS:
        lanes = {LANE: {"slots": slots, "m": metrics}}
        scenario.append(
            {
                "ts": ts,
                "models": {
                    "data": [
                        {
                            "id": LANE,
                            "status": {
                                "value": "loaded",
                                "args": ["x", "--port", str(PORT)],
                            },
                        }
                    ]
                },
                "lanes": {LANE: {"slots": slots, "metrics": metrics}},
            }
        )
        rows, curve = render_tick(st, sm, ts, lanes)
        golden.append({"rows": rows, "curve": curve})
    out = pathlib.Path(__file__).resolve().parent.parent / "tests" / "fixtures"
    out.mkdir(parents=True, exist_ok=True)
    (out / "engine_scenario.json").write_text(json.dumps(scenario, indent=1))
    (out / "engine_golden.json").write_text(json.dumps(golden, indent=1))
    print(f"wrote {len(TICKS)} ticks -> {out}")
    for i, g in enumerate(golden):
        r = g["rows"][0]
        print(
            f"  T{i}: phase={str(r['phase']):7} gen={r['gen_ts']:8.2f} "
            f"prompt={r['prompt_ts']:9.2f} curve={g['curve'][0]:7.2f}/{g['curve'][1]:9.2f}"
        )


if __name__ == "__main__":
    main()
