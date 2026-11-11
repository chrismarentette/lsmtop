//! M9: the live sampling loop — per-tick
//! pipeline: MetricsState.rates (journal-synced busy-run averages,
//! ) + live_rates (per-slot engine truth v9.7,
//! ) + _render_snap merge () + curve_smooth
//! (). CURVES = live truth only; TABLE = journal-synced labels
//! with 15 s hold.
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
    /// v9.15 table cells (): ACT "N (+Dq)", CTX "Nk",
    /// ACCEPT "NN%" (None -> em dash).
    pub act: String,
    pub ctx_k: Option<u64>,
    pub acc: Option<f64>,
    /// B1 (backlog): TOTAL tk — the live journal running total for the
    /// lane: running gen total while decoding, running prompt total while
    /// prefilling; None (blank cell) whenever nothing is live/cold.
    pub total_tk: Option<f64>,
}

/// B1 law: which running counter is live right now. decode -> gen total;
/// prompt -> the running prompt total; ambiguous 'active' -> gen evidence
/// wins (a decode sighting outranks the prefill baseline); anything else
/// is cold -> None so the cell empties instead of freezing a stale number.
/// JOURNAL-TRUTH TOTAL (maintainer 2026-11-07): the engine emits the running
/// numbers itself — `n_gen = N` on every timing line while decoding,
/// `prompt processing, n_tokens = N, progress = P` while prefilling, and
/// `stop processing: n_tokens = N` at release. The estimator may shade;
/// the TOTAL column rides these literals and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct JournalTotal {
    pub decoding: bool,
    pub gen: Option<f64>,
    pub prompt_n: Option<f64>,
    pub prompt_pct: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JournalTotalEvent {
    Gen(f64),
    PromptRun { n: f64, pct: f64 },
    End,
}

/// first integer at the head of the slice (journal numbers are padded)
fn first_num(s: &str) -> Option<f64> {
    let t = s.trim_start();
    let end = t.find(|c: char| !c.is_ascii_digit())?;
    if end == 0 {
        return None;
    }
    t[..end].parse::<f64>().ok()
}

pub fn parse_journal_total(line: &str) -> Option<JournalTotalEvent> {
    if line.contains("stop processing: n_tokens =") {
        return Some(JournalTotalEvent::End);
    }
    if line.contains("print_timing") {
        if let Some((_, rest)) = line.split_once("n_gen =") {
            return first_num(rest).map(JournalTotalEvent::Gen);
        }
    }
    if let Some((_, rest)) = line.split_once("prompt processing, n_tokens =") {
        let n = first_num(rest)?;
        let pct = rest
            .split_once("progress =")
            .and_then(|(_, p)| {
                let p = p.trim_start();
                let head: String = p.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                head.parse::<f64>().ok()
            })
            .unwrap_or(0.0);
        return Some(JournalTotalEvent::PromptRun { n, pct });
    }
    None
}

pub fn apply_journal_total(cur: &mut Option<JournalTotal>, ev: JournalTotalEvent) {
    match ev {
        JournalTotalEvent::Gen(v) => {
            // the task's prompt size is kept across the decode phase: the
            // GEN label's gen% is measured against ctx minus this prompt
            let keep_prompt = cur.and_then(|j| j.prompt_n);
            *cur = Some(JournalTotal {
                decoding: true,
                gen: Some(v),
                prompt_n: keep_prompt,
                prompt_pct: None,
            });
        }
        JournalTotalEvent::PromptRun { n, pct } => {
            *cur = Some(JournalTotal {
                decoding: false,
                gen: None,
                prompt_n: Some(n),
                prompt_pct: Some(pct),
            });
        }
        JournalTotalEvent::End => *cur = None,
    }
}

pub fn total_tk_of(lv: Option<&LiveOut>) -> Option<f64> {
    let l = lv?;
    match l.phase {
        "decode" => l.run_gen,
        "prompt" => l.run_prompt,
        "active" => l.run_gen.or(l.run_prompt),
        _ => None,
    }
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

/// 2377 `_held` — hold the last positive average ≤ HOLD s.
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

/// 2468 `MetricsState.rates` — cumulative busy-run averages
/// (print_timing math). cur None = FAILED scrape → hold, touch nothing.
fn rates(
    st: &mut RatesState,
    ts: f64,
    cur: Option<(f64, f64, f64, f64)>,
    processing: Option<f64>,
) -> (Option<f64>, Option<f64>) {
    let Some(c) = cur else {
        return held(st, ts); // hold; never poison
    };
    if st.samples.len() >= 120 {
        st.samples.pop_front();
    }
    st.samples.push_back((ts, c.0, c.1, c.2, c.3));
    let mut gen: Option<f64> = None;
    let mut prompt: Option<f64> = None;
    if let Some(p) = st.prev {
        // TWO consecutive regressions = restart
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
                // slots idle → close NOW (the run
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
            // 2463 prompt window (generous: pidle >= 3)
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
    // positive averages become the held display
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

/// 2723 `MetricsState.live_rates` — THE per-slot wall:
/// pp advanced ⇒ PREFILL (vetoes decode for that slot); organic
/// n_generated growth ⇒ DECODE; modest pt-pp streaks only on engines without
/// the organic field; the n_decode_total estimator only when ONE slot is
/// busy. Returns None on a lane's first tick and on resync ticks (state is
/// still updated — exactly like  early return after `st` mutation).
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
        // KV = pt/n_ctx, NEVER pt+something
        if let Some(nctx) = fnum(sl, "n_ctx") {
            if nctx > 0.0 {
                kv = kv.max((100.0 * pt / nctx).min(100.0));
            }
        }
    }
    // NOTE: no first-tick special case — runs the FULL loop on the
    // first tick (seeding ss entries and st.s) and only then returns None
    // via the dt<0.2 resync path. st.ts defaults to 0.0, so the first call
    // has dt>10 → resync, the same net effect.
    st.gen_field |= has_gen_field; // sticky flag
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
            // 2546 seed watched baselines on first sight
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
                // 2553 slot cleared → fresh run windows
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
            // 2570 counters reset → a NEW run on this slot
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
        // the 400 t/s ceiling is DELIBERATELY below
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
            // pp MOVES ⇒ prompt credit ONLY
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
                // 2615 old-engine decode-streak heuristic
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
    // 2631 SEPARATE sanity caps; discard lies, never clip
    if gen > 4000.0 * w {
        gen = 0.0;
    }
    if prompt > 250000.0 * w {
        prompt = 0.0;
    }
    let proc_any = busy > 0;

    // 2665 boundary calibration of tokens-per-iteration
    let empty: HashMap<String, f64> = HashMap::new();
    let m = m.unwrap_or(&empty);
    let dec = m.get("llamacpp:n_decode_total").copied();
    let tok = m.get("llamacpp:tokens_predicted_total").copied();
    if st.ratio.is_none()
        // was `> 200.0`: a freshly loaded child's FIRST run could never seed
        // the ratio — on /slots-frozen engines there is no organic fallback,
        // so the GEN curve stayed dead the whole run (repro h1, 2026-11-09).
        // Any handful of lifetime iterations averages the tokens/iteration
        // ratio; the 0.25..=16 range check still rejects garbage.
        && dec.map(|d| d >= 12.0).unwrap_or(false)
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
    // 2676 the iteration estimator: ONLY when ≤1 slot busy
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
    // 2711 per-slot run-so-far labels (fastest active slot)
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

/// Task-6 H3 fallback (maintainer 2026-11-09): the `/slots` scrape failed (or a
/// resync ate the tick) but the CUMULATIVE counters answered — derive window
/// rates from the two newest samples (cumulative-counter math, NEVER the
/// per-scrape *_seconds gauges). None when the counters say nothing about
/// the window; caps mirror the live-path sanity walls.
fn counter_evidence(rs: &RatesState, ts_secs: f64) -> Option<(f64, f64)> {
    let n = rs.samples.len();
    if n < 2 {
        return None;
    }
    let p = rs.samples[n - 2];
    let c = rs.samples[n - 1];
    // Freshness law: the newest sample must be THIS tick's push. Re-using an
    // older window would replay stale data through the fallback door — a
    // fully-failed tick has NO new evidence and must rail (test
    // failed_scrape_holds_rows_and_banner_not_state; maintainer: metrics is meant
    // to be correct data).
    if (ts_secs - c.0).abs() > 1e-6 {
        return None;
    }
    let dg = (c.1 - p.1).max(0.0);
    let dp = (c.2 - p.2).max(0.0);
    if dg <= 0.0 && dp <= 0.0 {
        return None;
    }
    let w = (c.0 - p.0).clamp(0.2, 12.0);
    Some(((dg / w).min(4000.0), (dp / w).min(250_000.0)))
}

/// 2869 `curve_smooth` — live truth EMA; ZERO IS STRICT
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

/// 2354 `cache_pct(slots)` — SUM across slots, no clamp.
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
    // SM clock MHz (voltage proxy — NVIDIA exposes no voltage)
    pub gpu_clk: Option<f64>,
    /// board power watts where the driver reports it (N/A on some NVIDIA stacks)
    pub gpu_pwr: Option<f64>,
    pub ram_percent: Option<f64>,
    pub last_error: Option<String>,
    pub kv_max: f64,
    /// v9.15 parity rings (): pushed EVERY tick —
    /// kv = max lane KV≈% ; apu = gpu_busy else 0.0 (only while the
    /// gpu_graph option is on, mirroring 'off stops all GPU polling').
    pub kv: Ring,
    pub apu: Ring,
    /// last per-tick live engine output per lane (Some/None) — for headless
    /// dumps and the live parity harnesses; reset every ingest tick.
    pub last_live: HashMap<String, Option<LiveOut>>,
    rates_st: HashMap<String, RatesState>,
    live_st: HashMap<String, LiveState>,
    /// lane focus (maintainer 2026-11-10: "pinning an active lane ... be the
    /// graph"): the lane whose measured values the GEN/PROMPT curves show.
    /// Auto-follow keeps a lane while it generates, retargets on a lane
    /// switch, and HOLDS the last lane while nothing runs. The public
    /// rings mirror the focused lane's own history — never a max-blend.
    pub focus: Option<String>,
    lanes_hist: HashMap<String, LaneVals>,
    lane_seq: u64,
    /// Seam (lane-switch marker, experiment for maintainer's eye): column-back
    /// offset of the sample where focus last moved, counted from the newest
    /// sample. The graph paints a dim vertical line there; the widget maps
    /// it through the SAME warp law (hist::seam_column) the ink used.
    pub seam_back: Option<usize>,
    /// pushes made to the PUBLIC rings (the timeline clock the seam rides)
    pub_pushes: u64,
    /// the column pushed last tick was a rail placeholder: the curve's
    /// lane had slots down and no counter evidence — correctable by the
    /// first measured value once evidence returns (2026-11-09 law)
    pub_prev_gap: bool,
    seam_switch_seen: Option<u64>,
    /// consecutive failed scrapes (Task 5): ONE blip holds the last truth;
    /// from the SECOND the caller pushes unreachable samples — time moves.
    pub err_streak: u32,
}

/// Per-lane ENGINE state behind the focused-lane law. The curve history
/// itself lives on the Sampler's public rings (the endpoint's timeline);
/// per lane we keep only what must not leak between lanes: the EMA
/// smoothing state and last-seen for the LRU bound.
struct LaneVals {
    sm: [f64; 2],
    seen: u64,
}

/// Rings are kept for every lane this server has ever shown; the cap is a
/// memory bound only (R9 says the same about ghosts on screen).
const LANE_HIST_MAX: usize = 16;

/// Auto-follow law (pure, locked): stay put while the focused lane keeps
/// generating (no ping-pong between concurrent lanes); when it stops, move
/// to the first lane that IS generating; when nothing generates, HOLD the
/// current focus — the last lane rides its rails, the subject never blanks.
pub fn next_focus(cur: Option<&str>, lanes: &[(String, bool)]) -> Option<String> {
    if let Some(c) = cur {
        if lanes.iter().any(|(l, g)| *g && l == c) {
            return Some(c.to_string());
        }
    }
    lanes
        .iter()
        .find(|(_, g)| *g)
        .map(|(l, _)| l.clone())
        .or_else(|| cur.map(|c| c.to_string()))
}

/// Display/identity name for a lane: bare llama.cpp reports the FULL MODEL
/// PATH as its OpenAI `id` — maintainer ruled the LANES word is the model name,
/// so a path-shaped id becomes its filename without extension (stable for
/// the keying that lanes_hist/rates/live use).
pub fn friendly_lane_name(id: &str) -> String {
    let base = id.rsplit(['/', '\\']).next().unwrap_or(id);
    match base.rfind('.') {
        Some(i) if i > 0 => base[..i].to_string(),
        _ => base.to_string(),
    }
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
            gpu_clk: None,
            gpu_pwr: None,
            ram_percent: None,
            last_error: None,
            kv_max: 0.0,
            kv: Ring::new(HISTORY_CAP),
            apu: Ring::new(HISTORY_CAP),
            last_live: HashMap::new(),
            rates_st: HashMap::new(),
            live_st: HashMap::new(),
            focus: None,
            pub_pushes: 0,
            pub_prev_gap: false,
            lanes_hist: HashMap::new(),
            lane_seq: 0,
            seam_back: None,
            seam_switch_seen: None,
            err_streak: 0,
        }
    }

    /// A failed scrape HOLDS :
    /// no state touched, no sample pushed.
    pub fn ingest_error(&mut self, msg: &str) {
        self.last_error = Some(msg.to_string());
        self.err_streak += 1;
    }

    /// Time must not stop when the target does (maintainer 2026-11-09): after
    /// the SECOND consecutive unreachable tick the UI pushes rail samples
    /// so past activity scrolls away instead of freezing mid-graph. One
    /// blip still HOLDS — the never-poison wall. GPU rides its own truth:
    /// the card stays busy even with the llama server down.
    pub fn ingest_unreachable(&mut self) {
        // EMA dies with the target (zero is strict). The TIMELINE scrolls a
        // rail — one push per tick; the placeholder is corrected by the
        // first measured evidence when the target answers again, never by
        // replay.
        for lvh in self.lanes_hist.values_mut() {
            lvh.sm = [0.0; 2];
        }
        self.gen.push(0.0);
        self.prompt.push(0.0);
        self.pub_pushes += 1;
        self.pub_prev_gap = self.focus.is_some();
        self.gen_leaky.observe(0.0);
        self.prompt_leaky.observe(0.0);
        self.kv.push(0.0); // no server -> no occupancy; honest zero
        self.apu.push(self.gpu_busy.unwrap_or(0.0));
        self.update_seam();
    }

    /// Seam position from the newest timeline column, valid only while the
/// ring still holds the switch boundary.
fn update_seam(&mut self) {
    self.seam_back = self.seam_switch_seen.and_then(|sw| {
        let b = self.pub_pushes.saturating_sub(1).saturating_sub(sw) as usize;
        if b < self.gen.data.len() {
            Some(b)
        } else {
            None
        }
    });
}

/// cur tuple from llamacpp: totals/// cur tuple from llamacpp: totals; FAILED
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
        self.err_streak = 0; // any good scrape ends the down-streak

        type LaneEntry<'a> = (
            String,
            u16,
            Option<&'a Vec<Value>>,
            Option<&'a HashMap<String, f64>>,
        );
        // (key = lane id , port, slots, metrics)
        let mut lanes: Vec<LaneEntry> = Vec::new();
        match data {
            SampleData::Router(rs) => {
                for (model, port) in child_ports(&rs.models) {
                    let lane = rs.lanes.get(&model); // lanes keyed by lane id (mid)
                    lanes.push((
                        model.clone(),
                        port,
                        lane.and_then(|l| l.slots.as_ref()).and_then(Value::as_array),
                        lane.and_then(|l| l.metrics.as_ref()),
                    ));
                }
                // proxy-fallback lanes (no --port in status.args — llama-swap
                // `--port 0` shape): the COLLECTOR scraped them through the
                // router port; they ride with port 0 (no child port to show)
                let kid_ids: std::collections::HashSet<String> = child_ports(&rs.models)
                    .into_iter()
                    .map(|(m, _)| m)
                    .collect();
                for (model, lane) in &rs.lanes {
                    if kid_ids.contains(model.as_str()) {
                        continue;
                    }
                    lanes.push((
                        model.clone(),
                        0,
                        lane.slots.as_ref().and_then(Value::as_array),
                        lane.metrics.as_ref(),
                    ));
                }
            }
            SampleData::Server(ss) => {
                // llama.cpp /v1/models is OpenAI shape — the name rides
                // "id"; "model" stays a legacy fallback, but a real name
                // must never lose to the generic label (Phase 1).
                let first = ss
                    .models
                    .get("data")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first());
                let model = first
                    .and_then(|m| m.get("id"))
                    .and_then(Value::as_str)
                    .or_else(|| first.and_then(|m| m.get("model")).and_then(Value::as_str))
                    .unwrap_or("llama-server")
                    .to_string();
                let model = friendly_lane_name(&model);
                lanes.push((
                    model,
                    0,
                    ss.slots.as_ref().and_then(Value::as_array),
                    ss.metrics.as_ref(),
                ));
            }
        }

        let mut rows: Vec<LaneRow> = Vec::new();
        let mut kv_max = 0.0f64;
        // per-lane curve feed (lane focus, 2026-11-10): (lane id, GEN,
        // PROMPT, had evidence, slots were down, is generating)
        let mut lane_out: Vec<(String, f64, f64, bool, bool, bool)> = Vec::new();

        for (mid, port, slots, metrics) in lanes {
            let mut lane_gv = 0.0f64;
            let mut lane_pv = 0.0f64;
            let mut lane_evid = false;
            let mut lane_gen = false;
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

            // live run labels override the table
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

            // CURVE values (v9.7 slot evidence)
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
                lane_gv = gv;
                lane_pv = pv;
                lane_gen = matches!(ph, "prompt" | "decode" | "active");
                lane_evid = true;
                lane_kv = lv.kv;
            } else {
                if let Some(slots) = slots {
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
                // H3 gap fallback: /slots down but counters answered — the
                // window rate is measured data, feed the curve (never rail
                // while a measurement says the engine moved).
                if let Some((g, p)) = counter_evidence(rs, ts_secs) {
                    lane_gv = lane_gv.max(g);
                    lane_pv = lane_pv.max(p);
                    lane_evid = true;
                }
            }
            kv_max = kv_max.max(lane_kv);

            // table cell truth (); missing metrics == empty
            // act renders "0", acc renders the dash.
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
                model: mid.clone(),
                gen_ts: rr_gen.unwrap_or(0.0),
                prompt_ts: rr_prompt.unwrap_or(0.0),
                cache_pct: lane_cache_pct(slots),
                kv: lane_kv,
                active: lv.as_ref().map(|l| l.proc).unwrap_or(false),
                act: act_s,
                ctx_k,
                acc: acc_o,
                total_tk: total_tk_of(lv.as_ref()),
            });
            // A lane wins focus on BUSY PHASE *or* counted evidence: /slots
            // can be down while the engine moves and the counter window IS
            // the measurement — dropping it un-focused would discard honest
            // data (floodproof regression, 2026-11-10).
            let focusable = lane_gen || (lane_evid && (lane_gv > 0.0 || lane_pv > 0.0));
            lane_out.push((mid, lane_gv, lane_pv, lane_evid, slots.is_none(), focusable));
        }

        // lane focus (2026-11-10): decide the lane FIRST, then feed the
        // endpoint's timeline with the FOCUSED lane's measurement.
        self.lane_seq += 1;
        let order: Vec<(String, bool)> = lane_out
            .iter()
            .map(|(m, _, _, _, _, g)| (m.clone(), *g))
            .collect();
        let new_focus = next_focus(self.focus.as_deref(), &order);
        // a seam marks a REAL switch (old lane -> new lane); taking the
        // very first focus is not a switch — nothing wore the curve before
        let switched = match (&self.focus, &new_focus) {
            (Some(prev), Some(now)) => prev != now,
            _ => false,
        };
        self.focus = new_focus;
        if switched {
            // the seam rides the boundary LEFT of the new stint's first
            // column, counted on the shared timeline clock
            self.seam_switch_seen = Some(self.pub_pushes);
        }
        let focus_id = self.focus.clone();
        let mut focus_sample: Option<(f64, f64)> = None;
        let mut focus_placeholder = false;
        for (mid, gv, pv, evid, sl_none, _gen) in lane_out {
            let is_focus = focus_id.as_deref() == Some(mid.as_str());
            let lvh = self.lanes_hist.entry(mid).or_insert_with(|| LaneVals {
                sm: [0.0; 2],
                seen: 0,
            });
            lvh.seen = self.lane_seq;
            // smoothing stays PER LANE: a lane regaining focus must never
            // have its first sample smoothed toward another lane's tail
            let (sg, sp) = curve_smooth(&mut lvh.sm, gv, pv);
            if is_focus {
                // gap placeholder correction (maintainer 2026-11-09): the rail
                // pushed last tick for the CURVE's lane is replaced by the
                // window's MEASURED mean the moment evidence returns — one
                // tick late, never a guess, never a held-over sample. A
                // switch moves the curve, so a gap left by another lane's
                // stint stays an honest rail.
                if self.pub_prev_gap && !switched {
                    if gv > 0.0 {
                        self.gen.replace_from_back(1, gv);
                    }
                    if pv > 0.0 {
                        self.prompt.replace_from_back(1, pv);
                    }
                }
                focus_placeholder = !evid && sl_none;
                focus_sample = Some((sg, sp));
            }
        }
        // THE CURVE IS THE ENDPOINT'S TIMELINE (maintainer 2026-11-10 ruling):
        // it never resets across a lane switch — each column carries the
        // measurement of whichever lane owned that moment, and the seam
        // marks where the curve changed identity. Past columns are the
        // record of their own tick; future columns belong to the new lane.
        match focus_sample {
            Some((sg, sp)) => {
                self.gen.push(sg);
                self.prompt.push(sp);
            }
            // no lane ever seen: the curve keeps scrolling time on rails
            None => {
                self.gen.push(0.0);
                self.prompt.push(0.0);
            }
        }
        self.pub_pushes += 1;
        self.pub_prev_gap = focus_placeholder;
        self.gen_leaky.observe(focus_sample.map(|(g, _)| g).unwrap_or(0.0));
        self.prompt_leaky.observe(focus_sample.map(|(_, p)| p).unwrap_or(0.0));
        self.update_seam();
        // bounded lane memory: rings for lanes this server stopped showing
        // retire LRU (the R9 ghost law, applied to memory)
        if self.lanes_hist.len() > LANE_HIST_MAX {
            let mut stale: Vec<(u64, String)> = self
                .lanes_hist
                .iter()
                .filter(|(k, _)| match focus_id.as_deref() {
                    Some(f) => k.as_str() != f,
                    None => true,
                })
                .map(|(k, v)| (v.seen, k.clone()))
                .collect();
            stale.sort();
            let drop_n = self.lanes_hist.len() - LANE_HIST_MAX;
            for (_, k) in stale.into_iter().take(drop_n) {
                self.lanes_hist.remove(&k);
            }
        }
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

#[cfg(test)]
mod b1_tests {
    use super::{total_tk_of, LiveOut};

    #[test]
    fn total_tk_tracks_the_live_counter_and_blanks_when_cold() {
        let l = |phase: &'static str, rp: Option<f64>, rg: Option<f64>| LiveOut {
            gen: 0.0,
            prompt: 0.0,
            kv: 0.0,
            proc: true,
            phase,
            est: 0.0,
            run_prompt: rp,
            run_gen: rg,
        };
        assert_eq!(total_tk_of(Some(&l("decode", Some(9.0), Some(137.0)))), Some(137.0));
        assert_eq!(total_tk_of(Some(&l("prompt", Some(612.0), None))), Some(612.0));
        assert_eq!(total_tk_of(Some(&l("active", Some(612.0), Some(3.0)))), Some(3.0));
        assert_eq!(total_tk_of(Some(&l("idle", None, None))), None);
        assert_eq!(total_tk_of(None), None); // cold lane -> blank cell
    }
}

#[cfg(test)]
mod journal_total_tests {
    //! the TOTAL column rides literal journal numbers (maintainer 2026-11-07);
    //! these fixtures are VERBATIM llama-server journal lines
    //! (hostname masked).
    use super::*;

    const DECODE: &str = "Nov 07 16:49:42 host llama-server[615877]: [57219] 40.46.066.364 I slot print_timing: id  0 | task 6448 | n_gen =    909, tg = 100.79 t/s, tg_3s = 105.63 t/s";
    const PROMPT: &str = "Nov 07 07:15:13 host llama-server[4128]: [60613] 0.08.908.942 I slot print_timing: id  0 | task 2 | prompt processing, n_tokens =  14210, progress = 1.00, t =   3.01 s / 4721.52 tokens per second";
    const END: &str = "Nov 07 16:49:54 host llama-server[615877]: [57219] 40.57.576.004 I slot      release: id  0 | task 6448 | stop processing: n_tokens = 14789, truncated = 0";

    #[test]
    fn journal_literals_drive_the_total() {
        assert_eq!(parse_journal_total(DECODE), Some(JournalTotalEvent::Gen(909.0)));
        assert_eq!(
            parse_journal_total(PROMPT),
            Some(JournalTotalEvent::PromptRun { n: 14210.0, pct: 1.0 })
        );
        assert_eq!(parse_journal_total(END), Some(JournalTotalEvent::End));
        assert_eq!(parse_journal_total("graph reallocs are enabled"), None);
        let mut cur = None;
        apply_journal_total(&mut cur, JournalTotalEvent::Gen(42.0));
        assert_eq!(cur.map(|j| (j.decoding, j.gen)), Some((true, Some(42.0))));
        apply_journal_total(&mut cur, JournalTotalEvent::End);
        assert_eq!(cur, None, "release must empty the cell, never freeze it");
    }
}

// Task 5 (maintainer 2026-11-09): time must not stop when the target does.
#[cfg(test)]
mod time_mover_tests {
    use super::Sampler;
    use crate::app::SampleData;
    use crate::net::RouterSample;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn unreachable_target_keeps_time_moving() {
        let mut s = Sampler::new();
        s.ingest_error("Router unreachable"); // ONE blip: hold, never poison
        assert_eq!(s.gen.values().len(), 0);
        s.ingest_error("Router unreachable"); // second consecutive = down
        assert_eq!(s.err_streak, 2);
        s.gpu_busy = Some(42.0);
        s.ingest_unreachable();
        assert_eq!(s.gen.values(), vec![0.0]);
        assert_eq!(s.apu.values(), vec![42.0]); // GPU keeps its own truth
        let ok = SampleData::Router(RouterSample {
            models: json!({"data": []}),
            lanes: HashMap::new(),
        });
        s.ingest(10.0, &ok); // recovery clears the streak
        assert_eq!(s.err_streak, 0);
    }
}
