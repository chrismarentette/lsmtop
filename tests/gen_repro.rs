//! Task-6 REPRO suite (scratch copy only — will land in the repo after the
//! child finishes Task 1 on the shared tree). Two suspects, two tests:
//!
//! H3  scrape gap: /slots times out (the child is busiest exactly then) while
//!     /metrics still answers. Today's code treats "no slot evidence" as
//!     "generated zero" and pushes a RAIL into the GEN curve mid-run, while
//!     the table keeps a correct rate (rates() path survives) — maintainer's
//!     exact report: journal right, LANES right, graph dead.
//!
//! H1  fresh-child dead zone: /slots frozen while decoding (house strix/MTP
//!     truth) + young lifetime counters -> the tokens-per-iteration ratio
//!     never seeds (gate: lifetime n_decode_total > 200), so the estimator
//!     is 0 and the GEN curve rails the WHOLE first run.
use lsmtop::app::SampleData;
use lsmtop::net::{LaneSample, RouterSample};
use lsmtop::sampling::Sampler;
use serde_json::{json, Value};
use std::collections::HashMap;

fn lane(slots: Option<Value>, metrics: Option<HashMap<String, f64>>) -> SampleData {
    let mut lanes = HashMap::new();
    lanes.insert(
        "repro-lane".to_string(),
        LaneSample { slots, metrics },
    );
    SampleData::Router(RouterSample {
        models: json!({"data": [{"id": "repro-lane", "status": {"value": "loaded",
            "args": ["x", "--port", "9"]}}]}),
        lanes,
    })
}

fn metrics(tok: f64, tsec: f64, dec: f64) -> HashMap<String, f64> {
    [
        ("llamacpp:tokens_predicted_total".to_string(), tok),
        ("llamacpp:tokens_predicted_seconds_total".to_string(), tsec),
        ("llamacpp:n_decode_total".to_string(), dec),
        ("llamacpp:prompt_tokens_total".to_string(), 4000.0),
        ("llamacpp:prompt_seconds_total".to_string(), 2.0),
    ]
    .into_iter()
    .collect()
}

/// slot whose n_generated_tokens GROWS (organic engine class)
fn live_slot(gen: f64, proc: bool) -> Value {
    json!({ "id": 0, "is_processing": proc, "n_ctx": 8192,
            "n_prompt_tokens": 4000.0, "n_prompt_tokens_processed": 4000.0,
            "n_generated_tokens": gen })
}

/// slot whose counters are FROZEN mid-decode (strix/MTP fork truth)
fn frozen_slot() -> Value {
    json!({ "id": 0, "is_processing": true, "n_ctx": 8192,
            "n_prompt_tokens": 4000.0, "n_prompt_tokens_processed": 4000.0,
            "n_generated_tokens": 812.0 })
}

#[test]
fn h3_curve_survives_a_one_tick_slots_gap() {
    let mut s = Sampler::new();
    // t0 resync tick
    s.ingest(1000.0, &lane(Some(json!([live_slot(100.0, true)])), Some(metrics(200.0, 10.0, 100.0))));
    // t1 organic decode: +26 tok over 2 s -> 13 t/s
    s.ingest(1002.0, &lane(Some(json!([live_slot(126.0, true)])), Some(metrics(226.0, 12.0, 112.0))));
    // t2 THE GAP: /slots scrape failed (None), /metrics answered and moved
    s.ingest(1004.0, &lane(None, Some(metrics(252.0, 14.0, 124.0))));
    // t3 organic again: +26 tok
    s.ingest(1006.0, &lane(Some(json!([live_slot(152.0, true)])), Some(metrics(278.0, 16.0, 136.0))));

    let gen = s.gen.values();
    let table_gen = s.rows.first().map(|r| r.gen_ts).unwrap_or(0.0);
    println!("curve={gen:?} table_gen={table_gen}");
    assert!(gen.len() >= 4, "four ingest ticks expected");
    assert!(table_gen > 0.0, "table GEN must be alive during the gap (it is today)");
    // THE DESIRED LAW (maintainer 2026-11-09: measured evidence, never stale replay):
    // during the gap the metrics evidence (252-226 tok / 2.0 tok-s = 13 t/s)
    // must feed the curve instead of a fabricated zero.
    assert!(gen[2] > 0.0, "REPRO H3: gap tick railed the GEN curve — curve={gen:?}");
}

#[test]
fn h1_fresh_frozen_slot_engine_reaches_the_gen_curve() {
    let mut s = Sampler::new();
    let mut dec = 50.0; // lifetime decodes YOUNG (freshly loaded child)
    let mut tok = 100.0; // ratio tok/dec ~ 2.0
    let mut tsec = 4.0;
    for i in 0..8u32 {
        let ts = 1000.0 + 2.0 * i as f64;
        dec += 12.0; // 6 iterations/s — real decode by the >5 it/s gate
        tok += 24.0; // ~13 t/s
        tsec += 1.8;
        s.ingest(ts, &lane(Some(json!([frozen_slot()])), Some(metrics(tok, tsec, dec))));
    }
    let gen = s.gen.values();
    println!("curve={gen:?}");
    assert!(
        gen.iter().any(|v| *v > 0.0),
        "REPRO H1: ratio never seeds below 200 lifetime decodes -> GEN curve \
         dead the whole run — curve={gen:?}"
    );
}
