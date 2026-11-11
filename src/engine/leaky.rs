
/// max
pub const DEFAULT_DECAY: f64 = 0.997;

#[derive(Debug, Clone)]
pub struct LeakyMax {
    pub v: f64,
    pub decay: f64,
}

impl LeakyMax {
    pub fn new(decay: f64) -> Self {
        Self { v: 0.0, decay }
    }

    pub fn observe(&mut self, peak: f64) -> f64 {
        self.v = peak.max(self.v * self.decay);
        self.v
    }
}

/// exact constants, cite
pub fn flow_phase(hist_tick: u64, ratio: f64) -> f64 {
    hist_tick as f64 * 0.028 + ratio * 1.7
}
