pub mod leaky;

use serde_json::Value;

/// M3.1 Counter-reset-safe delta
pub fn delta(prev: Option<u64>, now: Option<u64>) -> Option<u64> {
    match (prev, now) {
        (Some(p), Some(n)) => {
            if n < p {
                // Reset detected: returning Some(0)
                Some(0)
            } else {
                Some(n - p)
            }
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Prefill,
    Decode,
}

/// M3.2 Slot phase decision
pub fn slot_phase(slot: &Value, d_pp: u64, d_gen: u64) -> Phase {
    if !slot.get("is_processing").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Phase::Idle;
    }
    // prefill can NEVER read as GEN, e2e95 salted threshold
    if d_pp > 64 && d_gen == 0 {
        return Phase::Prefill;
    }
    if d_gen > 0 {
        return Phase::Decode;
    }
    // processing-but-quiet stays Idle, prevented GEN latching during pending queue
    Phase::Idle
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub pp_tps: f64,
    pub gen_tps: f64,
}

/// M3.3 Rates + label maturity
pub fn rates(prev_scrape_ts_secs: f64, now_scrape_ts_secs: f64, d_pp: u64, d_gen: u64) -> Rates {
    let mut dt = now_scrape_ts_secs - prev_scrape_ts_secs;
    if dt < 1e-6 {
        dt = 1e-6; // epsilon
    }
    Rates {
        pp_tps: d_pp as f64 / dt,
        gen_tps: d_gen as f64 / dt,
    }
}

pub fn label_mature(window_secs: f64) -> bool {
    window_secs >= 1.5
}

/// M3.4 CACHE %
pub fn cache_pct(kv_shared: Option<f64>) -> Option<f64> {
    kv_shared.map(|v| v.clamp(0.0, 100.0))
}
