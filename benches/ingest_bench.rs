//! Ingest throughput of the sampler path: parse → Sampler::ingest → rings.
//!
//! Groups read by scripts/perf-check.sh: 'parse' (models, slots, metrics)
//! and 'ingest' (tick). Gate target ≥ 50 000 ticks/s single-core.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use lsmtop::app::SampleData;
use lsmtop::net::{RouterSample, child_ports};
use lsmtop::sampling::Sampler;
use serde_json::Value;
use std::collections::HashMap;

// ──────────── fixture helpers ────────────

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

// ──────────── parse benches (group: 'parse') ────────────

fn bench_parse(c: &mut Criterion) {
    let mut g = c.benchmark_group("parse");
    g.bench_function(BenchmarkId::from_parameter("models"), |b| {
        b.iter(|| {
            black_box(serde_json::from_str::<Value>(
                include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/models.json"))
            ).unwrap());
        });
    });
    g.bench_function(BenchmarkId::from_parameter("slots"), |b| {
        b.iter(|| {
            black_box(serde_json::from_str::<Value>(
                include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/slots.json"))
            ).unwrap());
        });
    });
    g.bench_function(BenchmarkId::from_parameter("metrics"), |b| {
        b.iter(|| {
            let text = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/metrics.prom"));
            black_box(lsmtop::net::prom::parse_prom(text));
        });
    });
    g.finish();
}

// ──────────── ingest bench (group: 'ingest') ────────────

fn build_one_tick(models: &Value, slots_json: Value, metrics_map: &HashMap<String, f64>) -> SampleData {
    let ports = child_ports(models);
    let mut lanes = HashMap::new();
    for (model_id, _port) in ports {
        lanes.insert(model_id.clone(), lsmtop::net::LaneSample {
            slots: Some(slots_json.clone()),
            metrics: Some(metrics_map.clone()),
        });
    }
    SampleData::Router(RouterSample { models: models.clone(), lanes })
}

fn bench_ingest_tick(c: &mut Criterion) {
    let models = models_fixture();
    let slots = slots_fixture();
    let metrics = metrics_fixture();

    // Steady-state per-tick cost: ONE sampler across iterations, monotonic
    // clock and monotonic counters (a constant ts pins dt<0.2 resync and a
    // regressing counter reads as an engine restart — neither is the real
    // per-tick path the gate is supposed to price).
    let mut sampler = Sampler::new();
    let mut g = c.benchmark_group("ingest");
    g.bench_function(BenchmarkId::from_parameter("tick"), |b| {
        let mut n: u64 = 0;
        b.iter(|| {
            n += 1;
            let ts = 1_000.0 + n as f64 * 0.5;
            let mut perturbed = metrics.clone();
            perturbed.insert("llamacpp:tokens_predicted_total".into(), (n * 20) as f64);
            perturbed.insert(
                "llamacpp:tokens_predicted_seconds_total".into(),
                (n as f64) * 0.5,
            );
            perturbed.insert("llamacpp:prompt_tokens_total".into(), (n * 250) as f64);
            perturbed.insert(
                "llamacpp:prompt_seconds_total".into(),
                (n as f64) * 0.4,
            );
            let tick_data = build_one_tick(&models, slots.clone(), &perturbed);
            sampler.ingest(ts, &tick_data);
            black_box(&sampler);
        });
    });
    g.finish();
}

// ─────────── criterion setup ───────────

criterion_group!(benches, bench_parse, bench_ingest_tick);
criterion_main!(benches);
