//! M9: the sampling loop under PYTHON-TRUTH semantics (port of
//! MetricsState.rates + live_rates + curve_smooth). Curve = live truth
//! (MAX across lanes, EMA 0.55/0.45 between positives, strict zero);
//! table row = journal-synced busy-run label with 15 s hold + live overrides.
use lsmtop::app::SampleData;
use lsmtop::net::{LaneSample, RouterSample, ServerSample};
use lsmtop::sampling::Sampler;
use serde_json::{json, Value};
use std::collections::HashMap;

fn m(tok: f64, tsec: f64, ptok: f64, psec: f64) -> HashMap<String, f64> {
    [
        ("llamacpp:tokens_predicted_total".to_string(), tok),
        ("llamacpp:tokens_predicted_seconds_total".to_string(), tsec),
        ("llamacpp:prompt_tokens_total".to_string(), ptok),
        ("llamacpp:prompt_seconds_total".to_string(), psec),
    ]
    .into_iter()
    .collect()
}

fn slot(id: u64, pp: f64, pt: f64, gen: f64, proc: bool) -> Value {
    json!({ "id": id, "is_processing": proc, "n_ctx": 8192,
            "n_prompt_tokens": pt, "n_prompt_tokens_processed": pp,
            "n_prompt_tokens_cache": (pt / 5.0), "n_generated_tokens": gen })
}

type LaneFx = (&'static str, u16, Option<Value>, Option<HashMap<String, f64>>);

fn router(entries: Vec<LaneFx>) -> SampleData {
    let mut models = vec![];
    let mut lanes = HashMap::new();
    for (id, port, slots, metrics) in entries {
        models.push(json!({"id": id, "status": {"value": "loaded", "args": ["x", "--port", port.to_string()]}}));
        lanes.insert(
            id.to_string(),
            LaneSample { slots, metrics },
        );
    }
    SampleData::Router(RouterSample { models: json!({"data": models}), lanes })
}

fn one(slots: Option<Value>, metrics: Option<HashMap<String, f64>>) -> SampleData {
    router(vec![("test-lane", 37347, slots, metrics)])
}

#[test]
fn first_tick_has_rows_but_no_rates() {
    let mut v = Sampler::new();
    v.ingest(1000.0, &one(Some(json!([slot(0, 100.0, 100.0, 0.0, false)])), Some(m(0.0, 0.0, 0.0, 0.0))));
    assert_eq!(v.rows.len(), 1);
    assert_eq!(v.rows[0].model, "test-lane");
    assert_eq!((v.rows[0].gen_ts, v.rows[0].prompt_ts), (0.0, 0.0));
    assert_eq!(v.gen.values().last().copied(), Some(0.0));
}

#[test]
fn prefill_advances_prompt_never_gen() {
    // engine-truth wall: prompt processing must NEVER paint the GEN curve
    let mut v = Sampler::new();
    v.ingest(
        1000.0,
        &one(Some(json!([slot(0, 100.0, 100.0, 0.0, false)])), Some(m(500.0, 10.0, 900.0, 3.0))),
    );
    v.ingest(
        1001.0,
        &one(
            Some(json!([slot(0, 1200.0, 1200.0, 0.0, true)])),
            Some(m(500.0, 10.0, 2000.0, 3.5)),
        ),
    );
    let r = &v.rows[0];
    assert!(v.prompt.values().last().unwrap() > &100.0, "prompt curve live");
    assert_eq!(v.gen.values().last().copied(), Some(0.0), "PREFILL-NEVER-GEN rail");
    assert_eq!(r.gen_ts, 0.0, "GEN label pinned at 0 while prefilling");
    assert!(r.active);
}

#[test]
fn decode_curve_ema_and_run_label_maturity() {
    // metrics + organic slot growth at 80,80,40 t/s across three ticks
    let mut v = Sampler::new();
    let st = |gen: f64| Some(json!([slot(0, 500.0, 540.0, gen, true)]));
    v.ingest(1000.0, &one(st(10.0), Some(m(100.0, 2.0, 500.0, 1.0)))); // seed
    v.ingest(1001.0, &one(st(90.0), Some(m(180.0, 3.0, 500.0, 1.0)))); // +80
    assert!((v.gen.values().last().unwrap() - 80.0).abs() < 1e-9, "first positive: no EMA yet");
    // run label silent until the window is honest (>=1.5 s)
    assert_eq!(v.rows[0].gen_ts, 80.0, "table rides journal-synced rates()");
    v.ingest(1002.0, &one(st(170.0), Some(m(260.0, 4.0, 500.0, 1.0)))); // +80
    assert!((v.gen.values().last().unwrap() - 80.0).abs() < 1e-9);
    v.ingest(1003.0, &one(st(210.0), Some(m(300.0, 5.0, 500.0, 1.0)))); // +40
    // curve: EMA 80*0.55 + 40*0.45 = 62
    assert!((v.gen.values().last().unwrap() - 62.0).abs() < 1e-9, "curve EMA (v9.2)");
    // watched decode window [1001..1003] = 200 tok / 2 s = 100, OVERRIDES
    // the raw rates() cumulative (which would say 66.67)
    assert!((v.rows[0].gen_ts - 100.0).abs() < 1e-9, "run_gen label after maturity");
}

#[test]
fn idle_rails_but_table_holds_run_label() {
    let mut v = Sampler::new();
    let st = |gen: f64, proc: bool| Some(json!([slot(0, 500.0, 540.0, gen, proc)]));
    v.ingest(1000.0, &one(st(10.0, true), Some(m(100.0, 2.0, 500.0, 1.0))));
    v.ingest(1001.0, &one(st(90.0, true), Some(m(180.0, 3.0, 500.0, 1.0))));
    let held_gen = v.rows[0].gen_ts;
    v.ingest(1002.0, &one(st(90.0, false), Some(m(180.0, 3.0, 500.0, 1.0))));
    assert_eq!(v.gen.values().last().copied(), Some(0.0), "rail the instant it stops");
    assert!((v.rows[0].gen_ts - held_gen).abs() < 1e-9, "run label HOLDS (15 s)");
    assert!(!v.rows[0].active);
    assert_eq!(v.rows[0].cache_pct, Some(20.0), "cache = 108/540 → 20%");
}

#[test]
fn curve_is_max_across_lanes_not_sum() {
    // gen_live = max(gen_live, gv) — the graph shows the busiest lane
    let mut v = Sampler::new();
    let two = |g1: f64, g2: f64, proc: bool| {
        router(vec![
            (
                "lane-a",
                37347,
                Some(json!([slot(0, 500.0, 540.0, g1, proc)])),
                Some(m(100.0 + (g1 - 10.0), 2.0 + (g1 - 10.0) / 80.0, 500.0, 1.0)),
            ),
            (
                "lane-b",
                37348,
                Some(json!([slot(0, 500.0, 540.0, g2, proc)])),
                Some(m(100.0 + (g2 - 10.0), 2.0 + (g2 - 10.0) / 80.0, 500.0, 1.0)),
            ),
        ])
    };
    v.ingest(1000.0, &two(10.0, 10.0, true)); // seed
    v.ingest(1001.0, &two(90.0, 40.0, true)); // a: +80, b: +30
    let g = *v.gen.values().last().unwrap();
    assert!((g - 80.0).abs() < 1e-9, "max(80,30)=80, NOT sum 110 → got {g}");
}

#[test]
fn failed_scrape_holds_rows_and_banner_not_state() {
    let mut v = Sampler::new();
    let st = |gen: f64| Some(json!([slot(0, 500.0, 540.0, gen, true)]));
    v.ingest(1000.0, &one(st(10.0), Some(m(100.0, 2.0, 500.0, 1.0))));
    v.ingest(1001.0, &one(st(90.0), Some(m(180.0, 3.0, 500.0, 1.0))));
    let before = v.rows.clone();
    v.ingest_error("connection refused");
    assert_eq!(v.rows, before, "failed scrape must HOLD, not zero");
    assert_eq!(v.gen.values().len(), 2, "no fake sample pushed");
    // a tick where the CHILD scrape failed (slots+metrics gone) also holds
    // the table (disp hold) while the curve rails — parity
    v.ingest(1002.0, &one(None, None));
    assert_eq!(v.rows[0].gen_ts, before[0].gen_ts, "table holds via disp");
    assert_eq!(v.gen.values().last().copied(), Some(0.0), "curve rails on failed scrape");
}

#[test]
fn server_target_yields_single_lane() {
    let mut v = Sampler::new();
    let s = |pp: f64| {
        SampleData::Server(ServerSample {
            models: json!({"data":[{"model":"direct-llm"}]}),
            slots: Some(json!([slot(0, pp, pp + 40.0, 0.0, true)])),
            metrics: None,
        })
    };
    v.ingest(100.0, &s(10.0));
    v.ingest(101.0, &s(210.0));
    assert_eq!(v.rows.len(), 1);
    assert_eq!(v.rows[0].model, "direct-llm");
    assert!(v.prompt.values().last().unwrap() > &100.0, "prompt curve live");
    assert_eq!(v.gen.values().last().copied(), Some(0.0), "prefill never GEN");
}

#[test]
fn counter_reset_is_new_run_not_huge_rate() {
    // engine delta: counters shrinking (service restart) → NEW RUN reset,
    // never a huge/negative rate (live_rates reset path)
    let mut v = Sampler::new();
    let st = |pp: f64| Some(json!([slot(0, pp, pp, 0.0, true)]));
    v.ingest(1000.0, &one(st(900.0), None));
    v.ingest(1001.0, &one(st(10.0), None));
    // the reset tick's run_prompt = pp / 0.2 s window FLOOR
    // (max(..., 0.2)) — 10/0.2 = 50, never the huge/negative
    // delta (900-10)/1s = -890 nor 890 that a naive port would print.
    assert_eq!(v.prompt.values().last().copied(), Some(50.0), "reset: window floor, not huge rate");
    assert_eq!(v.gen.values().last().copied(), Some(0.0), "reset must never forge GEN");
    assert_eq!(v.rows[0].prompt_ts, 50.0);
}
