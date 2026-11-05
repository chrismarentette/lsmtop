//! M9: the live sampling loop — faithful port of the Python per-tick
//! pipeline: MetricsState.rates (journal-synced busy-run averages,
//! app.py:2391-2468) + live_rates (per-slot engine truth v9.7,
//! app.py:2469-2723) + _render_snap merge (app.py:3165-3214) + curve_smooth
//! (app.py:2852). CURVES = live truth only; TABLE = journal-synced labels
//! with 15 s hold. Port lines carry `// port:` refs to app.py.
use crate::app::SampleData;
use crate::engine::leaky::LeakyMax;
use crate::hist::Ring;
use crate::net::child_ports;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

pub const HISTORY_CAP: usize = 600; // port: MetricsState.HISTORY
const HOLD: f64 = 15.0; // port: MetricsState.HOLD (run label hold, s)

#[derive(Debug, Clone, PartialEq)]
pub struct LaneRow {
    pub port: u16,
    pub model: String,
    pub gen_ts: f64,
    pub prompt_ts: f64,
    pub cache_pct: Option<f64>,
    pub kv: f64,
    pub active: bool,
    /// v9.15 table cells (app.py:3253-3277): ACT "N (+Dq)", CTX "Nk",
    /// ACCEPT "NN%" (None -> em dash).
    pub act: String,
    pub ctx_k: Option<u64>,
    pub acc: Option<f64>,
}

// ────────────────────────── table rates (journal-synced) ──────────────────────────

#[derive(Default, Clone)]
struct Burst {
    g0: Option<(f64, f64)>,
    p0: Option<(f64, f64)>,
    idle: i64,
    pidle: i64,
    neg: i32,
}

#[derive(Default, Clone)]
struct RatesState {
    samples: VecDeque<(f64, f64, f64, f64, f64)>, // (ts, tok, tsec, ptok, psec) maxlen 120
    prev: Option<(f64, f64, f64, f64, f64)>,
    burst: Burst,
    disp: [Option<(f64, f64)>; 2], // (ts, val) peak-held display, gen/prompt
}

/// port: app.py:2368-2377 `_held` — hold the last positive average ≤ HOLD s.
fn held(st: &mut RatesState, ts: f64) -> (Option<f64>, Option<f64>) {
    let mut out = [None; 2];
    for (i, slot) in st.disp.iter_mut().enumerate() {
        if let Some((dts, v)) = *slot {
            if ts - dts <= HOLD {
                out[i] = Some(v);
            } else {
                *slot = None;
            }
        }
    }
    (out[0], out[1])
}

/// port: app.py:2391-2468 `MetricsState.rates` — cumulative busy-run averages
/// (print_timing math). cur None = FAILED scrape → hold, touch nothing.
fn rates(
    st: &mut RatesState,
    ts: f64,
    cur: Option<(f64, f64, f64, f64)>,
    processing: Option<f64>,
) -> (Option<f64>, Option<f64>) {
    let Some(c) = cur else {
        return held(st, ts); // port: app.py:2404-2411 (hold; never poison)
    };
    if st.samples.len() >= 120 {
        st.samples.pop_front();
    }
    st.samples.push_back((ts, c.0, c.1, c.2, c.3));
    let mut gen: Option<f64> = None;
    let mut prompt: Option<f64> = None;
    if let Some(p) = st.prev {
        // port: app.py:2417-2427 — TWO consecutive regressions = restart
        if c.0 - p.1 < 0.0 || c.1 - p.2 < 0.0 || c.2 - p.3 < 0.0 || c.3 - p.4 < 0.0 {
            st.burst.neg += 1;
            if st.burst.neg >= 2 {
                st.samples.clear();
                st.samples.push_back((ts, c.0, c.1, c.2, c.3)); // keep NEW baseline
                st.burst.g0 = None;
                st.burst.p0 = None;
                st.burst.pidle = 0;
            }
        } else {
            st.burst.neg = 0;
            let moving = c.0 - p.1 > 0.0;
            let proc_active = processing.map(|v| v > 0.0).unwrap_or(false);
            if proc_active || moving {
                st.burst.idle = 0;
                if st.burst.g0.is_none() {
                    st.burst.g0 = Some((p.1, p.2)); // idle baseline pre-run
                }
                if let Some((g0t, g0s)) = st.burst.g0 {
                    let d_sec = c.1 - g0s;
                    if d_sec > 0.15 {
                        gen = Some((c.0 - g0t) / d_sec);
                    }
                }
            } else if st.burst.g0.is_some() {
                // port: app.py:2440-2451 — slots idle → close NOW (the run
                // label must not dilute with idle seconds); slots scrape
                // failed (None) → hold, close after ~10 ticks.
                if processing == Some(0.0) {
                    st.burst.g0 = None;
                } else {
                    st.burst.idle += 1;
                    if st.burst.idle >= 10 {
                        st.burst.g0 = None;
                        st.burst.idle = 0;
                    }
                }
            }
            // port: app.py:2452-2463 prompt window (generous: pidle >= 3)
            if c.3 - p.4 > 0.0005 {
                if st.burst.p0.is_none() {
                    st.burst.p0 = Some((p.3, p.4));
                }
                if let Some((p0t, p0s)) = st.burst.p0 {
                    let d_psec = c.3 - p0s;
                    if d_psec > 0.005 {
                        prompt = Some((c.2 - p0t) / d_psec);
                    }
                }
                st.burst.pidle = 0;
            } else if st.burst.p0.is_some()
                && (processing == Some(0.0) || processing.is_none())
            {
                st.burst.pidle += 1;
                if st.burst.pidle >= 3 {
                    st.burst.p0 = None;
                }
            }
        }
    }
    // port: app.py:2464-2467 — positive averages become the held display
    for (i, val) in [gen, prompt].into_iter().enumerate() {
        if let Some(v) = val {
            if v > 0.5 {
                st.disp[i] = Some((ts, v));
            }
        }
    }
    st.prev = Some((ts, c.0, c.1, c.2, c.3));
    held(st, ts)
}

// ────────────────────────── live per-slot engine (v9.7) ──────────────────────────

#[derive(Clone)]
struct SlotLive {
    run_start: Option<f64>,
    pp_base: f64,
    decode_start: Option<f64>,
    gen_acc: f64,
    streak: i64,
    prefill: bool,
    decoding: bool,
    pre_ticks: i64,
}

#[derive(Clone, Copy)]
struct SlotObs {
    g: f64,
    pp: f64,
    proc: bool,
    pt: f64,
}

#[derive(Default, Clone)]
struct LiveState {
    ts: f64,
    s: HashMap<u64, SlotObs>,
    live: bool,
    ss: HashMap<u64, SlotLive>,
    gen_field: bool,
    ratio: Option<f64>,
    dec: Option<f64>,
    dec_streak: i64,
    tok0: Option<f64>,
    dec0: Option<f64>,
    idle_n: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LiveOut {
    pub gen: f64,
    pub prompt: f64,
    pub kv: f64,
    pub proc: bool,
    pub phase: &'static str,
    pub est: f64,
    pub run_prompt: Option<f64>,
    pub run_gen: Option<f64>,
}

fn fnum(slot: &Value, key: &str) -> Option<f64> {
    slot.get(key).and_then(Value::as_f64)
}

/// port: app.py:2469-2723 `MetricsState.live_rates` — THE per-slot wall:
/// pp advanced ⇒ PREFILL (vetoes decode for that slot); organic
/// n_generated growth ⇒ DECODE; modest pt-pp streaks only on engines without
/// the organic field; the n_decode_total estimator only when ONE slot is
/// busy. Returns None on a lane's first tick and on resync ticks (state is
/// still updated — exactly like Python's early return after `st` mutation).
fn live_rates(
    st: &mut LiveState,
    ts: f64,
    slots: Option<&Vec<Value>>,
    m: Option<&HashMap<String, f64>>,
) -> Option<LiveOut> {
    let slots = slots?;
    if slots.is_empty() {
        return None; // port: `if not slots: return None`
    }
    let mut cur: HashMap<u64, SlotObs> = HashMap::new();
    let mut kv = 0.0f64;
    let mut has_gen_field = false;
    for (i, sl) in slots.iter().enumerate() {
        let idx = sl.get("id").and_then(Value::as_u64).unwrap_or(i as u64);
        let gt = fnum(sl, "n_generated_tokens");
        if gt.is_some() {
            has_gen_field = true;
        }
        let pt = fnum(sl, "n_prompt_tokens").unwrap_or(0.0);
        let pp = fnum(sl, "n_prompt_tokens_processed").unwrap_or(pt);
        let proc = sl.get("is_processing").and_then(Value::as_bool).unwrap_or(false);
        cur.insert(idx, SlotObs { g: gt.unwrap_or(0.0), pp, proc, pt });
        // port: app.py:2519-2523 — KV = pt/n_ctx, NEVER pt+something
        if let Some(nctx) = fnum(sl, "n_ctx") {
            if nctx > 0.0 {
                kv = kv.max((100.0 * pt / nctx).min(100.0));
            }
        }
    }
    // NOTE: no first-tick special case — Python runs the FULL loop on the
    // first tick (seeding ss entries and st.s) and only then returns None
    // via the dt<0.2 resync path. st.ts defaults to 0.0, so the first call
    // has dt>10 → resync, the same net effect.
    st.gen_field |= has_gen_field; // port: app.py:2526-2527 sticky
    let dt = ts - st.ts;
    let resync = dt < 0.2 || dt > 10.0;
    let w = dt.max(0.0);

    st.ss.retain(|k, _| cur.contains_key(k)); // slot gone → drop state

    let mut gen = 0.0f64;
    let mut prompt = 0.0f64;
    let mut busy = 0i64;
    let mut organic = false;
    // iterate in engine slot order (max-based results are order-independent)
    let order: Vec<u64> = slots
        .iter()
        .enumerate()
        .map(|(i, sl)| sl.get("id").and_then(Value::as_u64).unwrap_or(i as u64))
        .collect();
    for idx in order {
        let Some(&o) = cur.get(&idx) else { continue };
        let proc = o.proc;
        let s = st.ss.entry(idx).or_insert_with(|| SlotLive {
            // port: app.py:2541-2546 seed watched baselines on first sight
            // (opened mid-run → watched totals ARE the baseline)
            run_start: if proc { Some(ts) } else { None },
            pp_base: if proc { o.pp } else { 0.0 },
            decode_start: None,
            gen_acc: 0.0,
            streak: 0,
            prefill: false,
            decoding: false,
            pre_ticks: 99,
        });
        if !proc {
            s.prefill = false;
            s.decoding = false;
            s.streak = 0;
            if o.pp == 0.0 {
                // port: app.py:2550-2553 slot cleared → fresh run windows
                s.run_start = None;
                s.pp_base = 0.0;
                s.decode_start = None;
                s.gen_acc = 0.0;
            }
            continue;
        }
        busy += 1;
        let old = if resync { None } else { st.s.get(&idx).copied() };
        let Some(old) = old else { continue };
        let dpp = o.pp - old.pp;
        let dg = o.g - old.g;
        if o.pp < old.pp - 1.0 || o.pt < old.pt - 1.0 {
            // port: app.py:2563-2570 counters reset → a NEW run on this slot
            s.run_start = Some(ts);
            s.pp_base = 0.0;
            s.decode_start = None;
            s.gen_acc = 0.0;
            s.streak = 0;
            s.prefill = true;
            s.decoding = false;
            s.pre_ticks = 0;
            continue;
        }
        let dqueue = (o.pt - o.pp) - (old.pt - old.pp);
        // port: app.py:2572-2578 — the 400 t/s ceiling is DELIBERATELY below
        // the 512-token chunk commit size (fake-decode class of bugs).
        let modest = dqueue > 0.0 && dqueue <= 400.0 * w.max(0.2);
        if dg > 0.0 {
            organic = true;
            gen += dg;
            s.gen_acc += dg;
            if !s.decoding {
                s.decoding = true;
                s.decode_start = Some(ts);
            }
        }
        if dpp > 0.0 {
            // port: app.py:2586-2594 — pp MOVES ⇒ prompt credit ONLY
            prompt += dpp;
            s.prefill = true;
            s.pre_ticks = 0;
            s.streak = 0;
            if s.run_start.is_none() {
                s.run_start = Some(ts);
                s.pp_base = 0.0;
            }
        } else {
            s.pre_ticks += 1;
            if s.pre_ticks > 2 {
                s.prefill = false; // pp truly quiet, not a chunk gap
            }
            if !st.gen_field {
                // port: app.py:2601-2615 old-engine decode-streak heuristic
                if modest && old.pp > 0.0 && (o.pt - o.pp) <= 256.0 {
                    s.streak += 1;
                } else {
                    s.streak = 0;
                }
                if s.streak >= 2 && !s.decoding {
                    s.decoding = true;
                    s.decode_start = Some(ts);
                    s.gen_acc = 0.0; // window opens HERE
                }
            }
        }
        if s.decoding && dpp == 0.0 && modest && !st.gen_field {
            gen += dqueue;
            s.gen_acc += dqueue;
        }
    }
    // port: app.py:2626-2631 SEPARATE sanity caps; discard lies, never clip
    if gen > 4000.0 * w {
        gen = 0.0;
    }
    if prompt > 250000.0 * w {
        prompt = 0.0;
    }
    let proc_any = busy > 0;

    // port: app.py:2635-2665 boundary calibration of tokens-per-iteration
    let empty: HashMap<String, f64> = HashMap::new();
    let m = m.unwrap_or(&empty);
    let dec = m.get("llamacpp:n_decode_total").copied();
    let tok = m.get("llamacpp:tokens_predicted_total").copied();
    if st.ratio.is_none()
        && dec.map(|d| d > 200.0 && d != 0.0).unwrap_or(false)
        && tok.map(|t| t != 0.0).unwrap_or(false)
        && !resync
    {
        if let (Some(d), Some(t)) = (dec, tok) {
            let r0 = t / d;
            if (0.25..=16.0).contains(&r0) {
                st.ratio = Some(r0);
            }
        }
    }
    let mut ddec = 0.0f64;
    if let Some(d) = dec {
        if let Some(pd) = st.dec {
            if !resync {
                ddec = (d - pd).max(0.0);
            }
        }
        st.dec = Some(d);
    }
    st.dec_streak = if ddec > 0.0 { st.dec_streak + 1 } else { 0 };
    if proc_any {
        st.idle_n = 0;
        if st.tok0.is_none() {
            if let (Some(d), Some(t)) = (dec, tok) {
                st.tok0 = Some(t);
                st.dec0 = Some(d - ddec);
            }
        }
    } else {
        st.idle_n += 1;
        if st.tok0.is_some() {
            if let (Some(d), Some(t), Some(t0), Some(dc0)) = (dec, tok, st.tok0, st.dec0) {
                let dtk = t - t0;
                let dd = d - dc0;
                if dtk > 0.0 && dd > 0.0 {
                    let r = dtk / dd;
                    if (0.25..=16.0).contains(&r) {
                        st.ratio = Some(r);
                    }
                    st.tok0 = None;
                    st.dec0 = None;
                } else if st.idle_n > 8 {
                    st.tok0 = None;
                    st.dec0 = None;
                }
            }
        }
    }
    // port: app.py:2667-2676 the iteration estimator: ONLY when ≤1 slot busy
    // and iterations move like real decode (>5 it/s)
    let mut est = 0.0f64;
    if busy <= 1
        && st.ratio.map(|r| r > 0.0).unwrap_or(false)
        && st.dec_streak >= 2
        && ddec / dt.max(0.2) >= 5.0
    {
        est = (ddec * st.ratio.unwrap() / dt.max(0.2)).min(4000.0);
    }
    let decoding = st.ss.values().any(|x| x.decoding) || est > 0.0;
    let prefilling = if est > 0.0 && busy <= 1 {
        // port: est belongs to the ONE busy slot; prefill grace is over
        false
    } else {
        st.ss.values().any(|x| x.prefill && !x.decoding)
    };
    let phase: &'static str = if !proc_any {
        st.live = false;
        "idle"
    } else if prefilling && decoding {
        "active"
    } else if decoding {
        "decode"
    } else {
        "prompt"
    };
    if gen > 0.0 || organic {
        st.live = true;
    }
    // port: app.py:2688-2711 per-slot run-so-far labels (fastest active slot)
    let mut run_prompt: Option<f64> = None;
    let mut run_gen: Option<f64> = None;
    for idx in slots.iter().enumerate().map(|(i, sl)| sl.get("id").and_then(Value::as_u64).unwrap_or(i as u64)).collect::<Vec<u64>>() {
        let Some(o) = cur.get(&idx) else { continue };
        let Some(s) = st.ss.get(&idx) else { continue };
        if !o.proc {
            continue;
        }
        if s.decoding {
            if let Some(d0) = s.decode_start {
                let el = ts - d0;
                // port: LABEL MATURITY — an average over <1.5 s lies
                if el >= 1.5 {
                    let v = s.gen_acc / el;
                    run_gen = Some(run_gen.map_or(v, |c: f64| c.max(v)));
                }
            }
        } else if s.prefill {
            if let Some(rs) = s.run_start {
                let v = (o.pp - s.pp_base).max(0.0) / (ts - rs).max(0.2);
                run_prompt = Some(run_prompt.map_or(v, |c: f64| c.max(v)));
            }
        }
    }
    if run_gen.is_none() && est > 0.0 {
        run_gen = Some(est);
    }
    if run_gen.is_none() && proc_any && !decoding {
        run_gen = Some(0.0); // GEN label pinned while no decode evidence
    }
    st.ts = ts;
    st.s = cur;
    if resync {
        return None; // re-anchor tick: keep last display
    }
    Some(LiveOut {
        gen: gen / dt,
        prompt: prompt / dt,
        kv,
        proc: proc_any,
        phase,
        est,
        run_prompt,
        run_gen,
    })
}

/// port: app.py:2852-2869 `curve_smooth` — live truth EMA; ZERO IS STRICT
/// (no decay tail; no ghost from a finished PROMPT spike into a new GEN run).
fn curve_smooth(sm: &mut [f64; 2], gv: f64, pv: f64) -> (f64, f64) {
    let mut out = [0.0; 2];
    for (i, x) in [gv, pv].into_iter().enumerate() {
        if x <= 0.0 {
            sm[i] = 0.0;
        } else {
            let s = sm[i];
            sm[i] = if s <= 0.0 { x } else { s * 0.55 + x * 0.45 };
        }
        out[i] = sm[i];
    }
    (out[0], out[1])
}

/// port: app.py:2340-2354 `cache_pct(slots)` — SUM across slots, no clamp.
pub fn lane_cache_pct(slots: Option<&Vec<Value>>) -> Option<f64> {
    let mut c = 0.0f64;
    let mut p = 0.0f64;
    for sl in slots.unwrap_or(&vec![]) {
        let pc = fnum(sl, "n_prompt_tokens_cache");
        let pt = fnum(sl, "n_prompt_tokens");
        if let (Some(pc), Some(pt)) = (pc, pt) {
            if pt != 0.0 {
                c += pc.max(0.0);
                p += pt;
            }
        }
    }
    if p > 0.0 {
        Some(100.0 * c / p)
    } else {
        None
    }
}

// ─────────────────────────────────── Sampler ───────────────────────────────────

pub struct Sampler {
    pub rows: Vec<LaneRow>,
    pub gen: Ring,
    pub prompt: Ring,
    pub gen_leaky: LeakyMax,
    pub prompt_leaky: LeakyMax,
    pub polls: u64,
    pub gpu_busy: Option<f64>,
    pub ram_percent: Option<f64>,
    pub last_error: Option<String>,
    pub kv_max: f64,
    /// v9.15 parity rings (app.py:3208-3214): pushed EVERY tick —
    /// kv = max lane KV≈% ; apu = gpu_busy else 0.0 (only while the
    /// gpu_graph option is on, mirroring 'off stops all GPU polling').
    pub kv: Ring,
    pub apu: Ring,
    /// last per-tick live engine output per lane (Some/None) — for headless
    /// dumps and the live parity harnesses; reset every ingest tick.
    pub last_live: HashMap<String, Option<LiveOut>>,
    rates_st: HashMap<String, RatesState>,
    live_st: HashMap<String, LiveState>,
    sm: [f64; 2],
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            gen: Ring::new(HISTORY_CAP),
            prompt: Ring::new(HISTORY_CAP),
            gen_leaky: LeakyMax::new(0.997),
            prompt_leaky: LeakyMax::new(0.997),
            polls: 0,
            gpu_busy: None,
            ram_percent: None,
            last_error: None,
            kv_max: 0.0,
            kv: Ring::new(HISTORY_CAP),
            apu: Ring::new(HISTORY_CAP),
            last_live: HashMap::new(),
            rates_st: HashMap::new(),
            live_st: HashMap::new(),
            sm: [0.0; 2],
        }
    }

    /// A failed scrape HOLDS (Python banners instead of rendering a snap):
    /// no state touched, no sample pushed.
    pub fn ingest_error(&mut self, msg: &str) {
        self.last_error = Some(msg.to_string());
    }

    /// port: app.py:3150-3156 — cur tuple from llamacpp: totals; FAILED
    /// metrics scrape → None (never (0,0,0,0)).
    fn lane_metrics(metrics: Option<&HashMap<String, f64>>) -> Option<(f64, f64, f64, f64)> {
        let m = metrics?;
        let g = |k: &str| m.get(k).copied().unwrap_or(0.0);
        Some((
            g("llamacpp:tokens_predicted_total"),
            g("llamacpp:tokens_predicted_seconds_total"),
            g("llamacpp:prompt_tokens_total"),
            g("llamacpp:prompt_seconds_total"),
        ))
    }

    pub fn ingest(&mut self, ts_secs: f64, data: &SampleData) {
        self.polls += 1;
        self.last_error = None;

        type LaneEntry<'a> = (
            String,
            u16,
            Option<&'a Vec<Value>>,
            Option<&'a HashMap<String, f64>>,
        );
        // (key = lane id (Python `mid`), port, slots, metrics)
        let mut lanes: Vec<LaneEntry> = Vec::new();
        match data {
            SampleData::Router(rs) => {
                for (model, port) in child_ports(&rs.models) {
                    let lane = rs.lanes.get(&model); // python: lanes keyed by lane id (mid)
                    lanes.push((
                        model.clone(),
                        port,
                        lane.and_then(|l| l.slots.as_ref()).and_then(Value::as_array),
                        lane.and_then(|l| l.metrics.as_ref()),
                    ));
                }
            }
            SampleData::Server(ss) => {
                let model = ss
                    .models
                    .get("data")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .unwrap_or("llama-server")
                    .to_string();
                lanes.push((
                    model,
                    0,
                    ss.slots.as_ref().and_then(Value::as_array),
                    ss.metrics.as_ref(),
                ));
            }
        }

        let mut rows: Vec<LaneRow> = Vec::new();
        let mut gen_live = 0.0f64;
        let mut prompt_live = 0.0f64;
        let mut kv_max = 0.0f64;

        for (mid, port, slots, metrics) in lanes {
            // ---- table: journal-synced busy-run labels ----
            let cur = Self::lane_metrics(metrics);
            let proc_count = slots.map(|s| {
                s.iter()
                    .filter(|sl| sl.get("is_processing").and_then(Value::as_bool).unwrap_or(false))
                    .count() as f64
            });
            let rs = self.rates_st.entry(mid.clone()).or_default();
            let (mut rr_gen, mut rr_prompt) = rates(rs, ts_secs, cur, proc_count);

            // ---- per-slot live engine ----
            let ls = self.live_st.entry(mid.clone()).or_default();
            let lv = live_rates(ls, ts_secs, slots, metrics);
            self.last_live
                .insert(mid.clone(), lv.as_ref().map(|l| (*l).clone()));

            // port: app.py:3165-3175 — live run labels override the table
            if let Some(lv) = &lv {
                if matches!(lv.phase, "prompt" | "decode" | "active") {
                    if let Some(rp) = lv.run_prompt {
                        rr_prompt = Some(rp);
                    }
                    if let Some(rg) = lv.run_gen {
                        rr_gen = Some(rg);
                    }
                }
            }

            // port: app.py:3175-3204 — CURVE values (v9.7 slot evidence)
            let mut lane_kv = 0.0f64;
            if let Some(lv) = &lv {
                let ph = lv.phase;
                let mut pv = lv.prompt;
                if let Some(rp) = lv.run_prompt {
                    if matches!(ph, "prompt" | "active") {
                        pv = pv.max(rp); // average never dips on chunk stalls
                    }
                }
                let mut gv = lv.gen.max(lv.est);
                if ph == "prompt" {
                    gv = 0.0; // zero decode evidence → zero GEN
                }
                if !matches!(ph, "prompt" | "decode" | "active") {
                    gv = 0.0;
                    pv = 0.0;
                }
                gen_live = gen_live.max(gv);
                prompt_live = prompt_live.max(pv);
                lane_kv = lv.kv;
            } else if let Some(slots) = slots {
                // no live signal (resync/first tick): KV from occupancy
                for sl in slots {
                    if let Some(nctx) = fnum(sl, "n_ctx") {
                        if nctx > 0.0 {
                            let pt = fnum(sl, "n_prompt_tokens").unwrap_or(0.0);
                            lane_kv = lane_kv.max(100.0 * pt / nctx);
                        }
                    }
                }
            }
            kv_max = kv_max.max(lane_kv);

            // table cell truth (app.py:3253-3277); missing metrics == empty
            // dict in Python: act renders "0", acc renders the dash.
            let act_s = {
                let pn = metrics
                    .and_then(|m| m.get("llamacpp:requests_processing"))
                    .copied()
                    .unwrap_or(0.0) as u64;
                let dq = metrics
                    .and_then(|m| m.get("llamacpp:requests_deferred"))
                    .copied()
                    .unwrap_or(0.0) as u64;
                if dq > 0 {
                    format!("{pn} (+{dq}q)")
                } else {
                    format!("{pn}")
                }
            };
            let acc_o = {
                let d = metrics
                    .and_then(|m| m.get("llamacpp:spec_decode_num_draft_tokens_total"))
                    .copied()
                    .unwrap_or(0.0);
                let ac = metrics
                    .and_then(|m| m.get("llamacpp:spec_decode_num_accepted_tokens_total"))
                    .copied()
                    .unwrap_or(0.0);
                if d > 0.0 {
                    Some(100.0 * ac / d)
                } else {
                    None
                }
            };
            let mut ctx_k: Option<u64> = None;
            if let Some(slots) = slots {
                for sl in slots {
                    if let Some(n) = fnum(sl, "n_ctx") {
                        if n > 0.0 {
                            ctx_k = Some((n as u64) / 1024);
                            break;
                        }
                    }
                }
            }
            rows.push(LaneRow {
                port,
                model: mid,
                gen_ts: rr_gen.unwrap_or(0.0),
                prompt_ts: rr_prompt.unwrap_or(0.0),
                cache_pct: lane_cache_pct(slots),
                kv: lane_kv,
                active: lv.as_ref().map(|l| l.proc).unwrap_or(false),
                act: act_s,
                ctx_k,
                acc: acc_o,
            });
        }

        // port: app.py:3208-3214 — push EVERY tick; rail is rail.
        let (gv, pv) = curve_smooth(&mut self.sm, gen_live, prompt_live);
        self.gen.push(gv);
        self.prompt.push(pv);
        self.gen_leaky.observe(gv);
        self.prompt_leaky.observe(pv);
        self.kv_max = kv_max;
        self.kv.push(kv_max);
        self.apu.push(self.gpu_busy.unwrap_or(0.0));
        self.rows = rows;
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}
