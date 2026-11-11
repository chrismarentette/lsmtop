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
fn counter_evidence_lane_wins_focus_when_slots_down() {
    // /slots can be down while the engine moves — the counter window IS
    // the measured evidence, and focus must follow the lane that produced
    // numbers (floodproof regression: honest data was discarded unfocused).
    let mut v = Sampler::new();
    v.ingest(1000.0, &one(None, Some(m(100.0, 2.0, 0.0, 0.0))));
    v.ingest(1001.0, &one(None, Some(m(180.0, 3.0, 0.0, 0.0)))); // +80 tok / 1 s
    assert_eq!(v.focus.as_deref(), Some("test-lane"), "counted lane owns the curve");
    assert!(v.gen.values().last().unwrap() > &0.0, "counted sample lands on the curve");
}

#[test]
fn server_lane_wears_the_model_name_not_the_path() {
    // bare llama.cpp reports the FULL PATH as its OpenAI id — the LANES
    // word is the model name (maintainer 2026-11-10), and it keys stably
    let mut v = Sampler::new();
    let s = |pp: f64| {
        SampleData::Server(ServerSample {
            models: json!({"object":"list","data":[
                {"id":"/home/m/models/phi-4-mini-instruct-q4_k_m.gguf","object":"model"}]}),
            slots: Some(json!([slot(0, pp, pp + 40.0, 0.0, true)])),
            metrics: None,
        })
    };
    v.ingest(100.0, &s(10.0));
    v.ingest(101.0, &s(210.0));
    assert_eq!(v.rows[0].model, "phi-4-mini-instruct-q4_k_m");
    assert_eq!(v.focus.as_deref(), Some("phi-4-mini-instruct-q4_k_m"));
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
fn curve_is_the_focused_lane_never_a_sum() {
    // lane focus law (maintainer 2026-11-10): the curve IS the focused lane's
    // own measured value — never a sum across lanes. (Renamed from
    // curve_is_max_across_lanes_not_sum: with per-lane rings the graph no
    // longer max-blends; it follows ONE lane, and that lane is a.)
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
    assert_eq!(v.focus.as_deref(), Some("lane-a"), "auto-follow takes the generating lane");
    let g = *v.gen.values().last().unwrap();
    assert!((g - 80.0).abs() < 1e-9, "focused lane a = 80, NOT sum 110 → got {g}");
}

#[test]
fn next_focus_holds_retargets_and_holds_on_idle() {
    // pure auto-follow law: no ping-pong while the focused lane runs;
    // retarget when it stops; HOLD on full idle; honest None at start.
    use lsmtop::sampling::next_focus;
    let t = |s: &str| s.to_string();
    assert_eq!(next_focus(None, &[(t("a"), true), (t("b"), false)]).as_deref(), Some("a"));
    assert_eq!(
        next_focus(Some("a"), &[(t("a"), true), (t("b"), true)]).as_deref(),
        Some("a"),
        "concurrent lanes must not ping-pong the graph"
    );
    assert_eq!(
        next_focus(Some("a"), &[(t("a"), false), (t("b"), true)]).as_deref(),
        Some("b"),
        "lane switch retargets the curve"
    );
    assert_eq!(
        next_focus(Some("a"), &[(t("a"), false), (t("b"), false)]).as_deref(),
        Some("a"),
        "idle HOLDS the last lane (rails, never a blank subject)"
    );
    assert_eq!(next_focus(None, &[(t("a"), false)]).as_deref(), None);
}

#[test]
fn graph_continues_across_a_switch_and_the_seam_marks_the_break() {
    // maintainer ruling 2026-11-10: the curve is the ENDPOINT'S TIMELINE — a
    // lane switch must never erase history. Past columns keep the honest
    // record of their own ticks; from the seam rightward the curve wears
    // ONLY the new lane's measurements.
    let mut v = Sampler::new();
    let la = |gen: f64, proc: bool| {
        (
            "lane-a",
            37347,
            Some(json!([slot(0, 500.0, 540.0, gen, proc)])),
            Some(m(100.0 + (gen - 10.0), 2.0 + (gen - 10.0) / 80.0, 500.0, 1.0)),
        )
    };
    let lb = |gen: f64, proc: bool| {
        (
            "lane-b",
            37348,
            Some(json!([slot(1, 500.0, 540.0, gen, proc)])),
            Some(m(100.0 + (gen - 10.0), 2.0 + (gen - 10.0) / 80.0, 500.0, 1.0)),
        )
    };
    v.ingest(1000.0, &router(vec![la(10.0, true), lb(10.0, false)])); // seed
    v.ingest(1001.0, &router(vec![la(90.0, true), lb(10.0, false)])); // a runs +80
    assert_eq!(v.seam_back, None, "first focus is not a switch — no seam");
    v.ingest(1002.0, &router(vec![la(90.0, false), lb(90.0, true)])); // switch: b runs +80
    assert_eq!(v.focus.as_deref(), Some("lane-b"), "auto-follow moved a -> b");
    let g = v.gen.values();
    assert_eq!(g.len(), 3, "the timeline never resets — history survives the switch");
    assert!(g[1] > 10.0, "lane a's ink STAYS on the timeline (got {})", g[1]);
    assert!(g[2] > 10.0, "b's own measured value right of the seam (got {})", g[2]);
    // the table still shows BOTH lanes — focus names the graph, not the rows
    assert_eq!(v.rows.len(), 2);
    // the seam: appears at the right edge on the switch tick...
    assert_eq!(v.seam_back, Some(0), "switch column is the newest sample");
    v.ingest(1003.0, &router(vec![la(90.0, false), lb(90.0, false)])); // idle, holds b
    assert_eq!(v.focus.as_deref(), Some("lane-b"), "idle holds the lane");
    let g = v.gen.values();
    assert_eq!(g[3], 0.0, "idle rides the rail — no stale replay");
    assert_eq!(v.seam_back, Some(1), "...and travels left with time");
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
fn single_instance_lane_rides_the_openai_id() {
    // bare llama.cpp /models is OpenAI shape: the name rides "id", and a
    // generic "llama-server" label must never shadow a real name
    let mut v = Sampler::new();
    let s = |pp: f64| {
        SampleData::Server(ServerSample {
            models: json!({"object":"list","data":[
                {"id":"gemma-3-12b","object":"model","owned_by":"llamacpp"}]}),
            slots: Some(json!([slot(0, pp, pp + 40.0, 0.0, true)])),
            metrics: None,
        })
    };
    v.ingest(100.0, &s(10.0));
    assert_eq!(v.rows.len(), 1);
    assert_eq!(v.rows[0].model, "gemma-3-12b", "OpenAI id is the lane name");
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
