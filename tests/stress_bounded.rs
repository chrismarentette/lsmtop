//! 200,000 synthetic ingest ticks through the Sampler path.
//! Values vary with sin(index) so nothing optimizes away.
//! Asserts every exposed ring/hist structure stays at its fixed cap;
//! sampler.rows len bounded by lane count; HashMaps bounded by fixture lane count.

use lsmtop::app::{SampleData};
use lsmtop::net::{RouterSample, LaneSample, child_ports};
use lsmtop::sampling::{Sampler, HISTORY_CAP};
use lsmtop::hist::Ring;
use serde_json::Value;
use std::collections::HashMap;

fn models_fixture() -> Value {
    serde_json::from_str(
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/models.json"))
    ).unwrap()
}

fn slots_fixture() -> Value {
    serde_json::from_str(
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/slots.json"))
    ).unwrap()
}

fn metrics_fixture() -> HashMap<String, f64> {
    let text = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/metrics.prom"));
    lsmtop::net::prom::parse_prom(text)
}

/// Build one sample_data once; clone per tick (commented for clarity).
fn build_tick(models: &Value, slots_json: Value, metrics_map: &HashMap<String, f64>) -> SampleData {
    let ports = child_ports(models);
    let mut lanes = HashMap::new();
    for (model_id, _port) in ports {
        lanes.insert(model_id.clone(), LaneSample {
            slots: Some(slots_json.clone()),
            metrics: Some(metrics_map.clone()),
        });
    }
    SampleData::Router(RouterSample { models: models.clone(), lanes })
}

#[test]
fn two_hundred_k_ticks_all_structures_stay_bounded() {
    let models = models_fixture();
    let slots = slots_fixture();
    let metrics = metrics_fixture();
    let ports = child_ports(&models);
    let lane_count = ports.len();

    // Pre-parse once, then clone per tick — ~200k iterations stay well under 60s.
    let mut sampler = Sampler::new();

    for i in 0..200_000 {
        // Vary values with sin(index) so the sampler path never short-circuits.
        let ts = 1_000.0 + i as f64 * 0.5;
        let amp_gen = 100.0 * (i as f64).sin();
        let amp_prompt = 200.0 * (i as f64).cos();

        // Build the synthetic tick data by slightly perturbing metrics.
        let mut perturbed_metrics = metrics.clone();
        perturbed_metrics.insert(
            "llamacpp:tokens_predicted_total".to_string(),
            amp_gen.max(0.0)
        );
        perturbed_metrics.insert(
            "llamacpp:prompt_tokens_total".to_string(),
            amp_prompt.max(0.0)
        );

        let tick_data = build_tick(&models, slots.clone(), &perturbed_metrics);
        sampler.ingest(ts, &tick_data);

        // ── Ring bounds ──
        // gen and prompt: cap is HISTORY_CAP (600), len = min(cap, pushed).
        assert_eq!(
            sampler.gen.values().len(),
            sampler.gen.cap.min(i + 1),
            "gen ring len at tick {} exceeds cap",
            i
        );
        assert_eq!(
            sampler.prompt.values().len(),
            sampler.prompt.cap.min(i + 1),
            "prompt ring len at tick {} exceeds cap",
            i
        );
        assert_eq!(
            sampler.kv.values().len(),
            sampler.kv.cap.min(i + 1),
            "kv ring len at tick {} exceeds cap",
            i
        );
        assert_eq!(
            sampler.apu.values().len(),
            sampler.apu.cap.min(i + 1),
            "apu ring len at tick {} exceeds cap",
            i
        );

        // ── Rows bounded by lane count ──
        assert!(sampler.rows.len() <= lane_count,
            "rows grew to {} at tick {} (expected ≤ {} lanes)",
            sampler.rows.len(), i, lane_count);

        // ── HashMaps bounded by fixture lane count ──
        // last_live keyed by model id — at most lane_count entries.
        assert!(sampler.last_live.len() <= lane_count,
            "last_live grew to {} at tick {}",
            sampler.last_live.len(), i);

        // ── No Vec grows per tick (rows replaced in-place by ingest) ──
        // The Vec capacity is stable; we verify len stays bounded.
    }

    // Final steady-state assertions: every ring must be exactly at cap.
    assert_eq!(sampler.gen.values().len(), sampler.gen.cap, "gen ring not full");
    assert_eq!(sampler.prompt.values().len(), sampler.prompt.cap, "prompt ring not full");
    assert_eq!(sampler.kv.values().len(), sampler.kv.cap, "kv ring not full");
    assert_eq!(sampler.apu.values().len(), sampler.apu.cap, "apu ring not full");
}

#[test]
fn stress_ring_push_does_not_leak_beyond_cap() {
    // Direct Ring exercise: 10k pushes through a HISTORY_CAP-600 ring.
    let mut r = Ring::new(HISTORY_CAP);
    for i in 0..10_000 {
        r.push((i as f64).sin().abs() + 0.5);
        assert!(r.data.len() <= r.cap, "Ring grew beyond fixed cap at push {}", i);
    }
    assert_eq!(r.values().len(), r.cap);
}
