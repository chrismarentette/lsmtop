use lsmtop::engine::*;
use serde_json::json;

#[test]
fn test_delta_semantics() {
    assert_eq!(delta(Some(100), None), None);
    assert_eq!(delta(None, Some(100)), None);
    assert_eq!(delta(Some(100), Some(95)), Some(0)); // flagging reset
    assert_eq!(delta(Some(100), Some(180)), Some(80));
}

#[test]
fn test_slot_phase() {
    assert_eq!(slot_phase(&json!({"is_processing": false}), 100, 0), Phase::Idle);
    assert_eq!(slot_phase(&json!({"is_processing": true}), 65, 0), Phase::Prefill);
    assert_eq!(slot_phase(&json!({"is_processing": true}), 0, 1), Phase::Decode);
    assert_eq!(slot_phase(&json!({"is_processing": true}), 100, 1), Phase::Decode);
    assert_eq!(slot_phase(&json!({"is_processing": true}), 64, 0), Phase::Idle);
    
    let prefill_slot = json!({
        "id": 0,
        "is_processing": true,
        "n_prompt_tokens": 128,
        "n_generated_tokens": 0
    });
    assert_eq!(slot_phase(&prefill_slot, 128, 0), Phase::Prefill);
    
    let decode_slot = json!({
        "id": 1,
        "is_processing": true,
        "n_prompt_tokens": 128,
        "n_generated_tokens": 10
    });
    assert_eq!(slot_phase(&decode_slot, 0, 1), Phase::Decode);
}

#[test]
fn test_rates_and_maturity() {
    let r = rates(0.0, 0.4, 0, 36);
    assert_eq!(r.gen_tps, 90.0);
    assert!(!label_mature(0.4)); // This false maturity prevents rendering 90.0
    assert!(label_mature(1.5));
}

#[test]
fn test_cache_pct() {
    assert_eq!(cache_pct(Some(50.0)), Some(50.0));
    assert_eq!(cache_pct(Some(120.0)), Some(100.0));
    assert_eq!(cache_pct(Some(-10.0)), Some(0.0));
    assert_eq!(cache_pct(None), None);
}

#[test]
fn test_leaky_max_and_flow_phase() {
    let mut lm = lsmtop::engine::leaky::LeakyMax::new(lsmtop::engine::leaky::DEFAULT_DECAY);
    lm.observe(100.0);
    for _ in 0..1000 {
        lm.observe(0.0);
    }
    let expected = 100.0 * 0.997f64.powi(1000);
    assert!((lm.v - expected).abs() < 1e-6);
    
    let phase1 = lsmtop::engine::leaky::flow_phase(1, 0.5);
    let phase2 = lsmtop::engine::leaky::flow_phase(2, 0.5);
    assert!(phase2 > phase1);
    
    let phase_ratio_0 = lsmtop::engine::leaky::flow_phase(10, 0.0);
    let phase_ratio_1 = lsmtop::engine::leaky::flow_phase(10, 1.0);
    assert!(phase_ratio_1 > phase_ratio_0);
}
