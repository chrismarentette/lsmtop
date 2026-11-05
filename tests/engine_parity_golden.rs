//! M9.2: offline engine parity — replay the committed scenario through the
//! Rust Sampler and match the Python-oracle golden (tools/paint_ref.py drove
//! the REAL MetricsState; this test is the anti-drift contract, 1e-9).
use lsmtop::app::SampleData;
use lsmtop::net::{LaneSample, RouterSample};
use lsmtop::sampling::Sampler;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

fn fixture(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&p).expect(name)).expect(name)
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn engine_golden_parity() {
    let scenario = fixture("engine_scenario.json");
    let golden = fixture("engine_golden.json");
    let ticks = scenario.as_array().unwrap();
    let expect = golden.as_array().unwrap();
    assert_eq!(ticks.len(), expect.len());

    let mut sampler = Sampler::new();
    for (i, (tick, want)) in ticks.iter().zip(expect).enumerate() {
        let ts = tick["ts"].as_f64().unwrap();
        let mut lanes = HashMap::new();
        let lm = tick["lanes"].as_object().unwrap();
        for (port, l) in lm {
            let metrics = l["metrics"].as_object().map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_f64().unwrap()))
                    .collect::<HashMap<String, f64>>()
            });
            lanes.insert(
                port.clone(),
                LaneSample {
                    slots: if l["slots"].is_null() {
                        None
                    } else {
                        Some(l["slots"].clone())
                    },
                    metrics,
                },
            );
        }
        let data = SampleData::Router(RouterSample {
            models: tick["models"].clone(),
            lanes,
        });
        sampler.ingest(ts, &data);

        // ---- table rows ----
        let wrows = want["rows"].as_array().unwrap();
        assert_eq!(sampler.rows.len(), wrows.len(), "T{i} row count");
        for (got, w) in sampler.rows.iter().zip(wrows) {
            assert_eq!(got.model, w["model"].as_str().unwrap(), "T{i} lane id");
            assert!(
                near(got.gen_ts, w["gen_ts"].as_f64().unwrap()),
                "T{i} {} gen_ts: rust {} vs python {}",
                got.model,
                got.gen_ts,
                w["gen_ts"]
            );
            assert!(
                near(got.prompt_ts, w["prompt_ts"].as_f64().unwrap()),
                "T{i} {} prompt_ts: rust {} vs python {}",
                got.model,
                got.prompt_ts,
                w["prompt_ts"]
            );
            let wcache = &w["cache_pct"];
            match (got.cache_pct, wcache.as_f64()) {
                (Some(g), Some(wv)) => assert!(near(g, wv), "T{i} cache: {g} vs {wv}"),
                (None, None) => {}
                (g, wv) => panic!("T{i} cache_pct mismatch: rust {g:?} python {wv:?}"),
            }
            assert!(near(got.kv, w["kv"].as_f64().unwrap()), "T{i} kv mismatch");
            assert_eq!(got.active, w["active"].as_bool().unwrap(), "T{i} active");
        }

        // ---- curves ----
        let wcurve = want["curve"].as_array().unwrap();
        let g = *sampler.gen.values().last().unwrap();
        let p = *sampler.prompt.values().last().unwrap();
        assert!(near(g, wcurve[0].as_f64().unwrap()), "T{i} curve gen: rust {g} vs python {}", wcurve[0]);
        assert!(near(p, wcurve[1].as_f64().unwrap()), "T{i} curve prompt: rust {p} vs python {}", wcurve[1]);
    }
}
