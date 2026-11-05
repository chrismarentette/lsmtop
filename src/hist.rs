use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct Ring {
    pub cap: usize,
    pub data: VecDeque<f64>,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            data: VecDeque::with_capacity(cap),
        }
    }

    pub fn push(&mut self, val: f64) {
        if self.cap == 0 {
            return;
        }
        if self.data.len() == self.cap {
            self.data.pop_front();
        }
        self.data.push_back(val);
    }
    
    pub fn values(&self) -> Vec<f64> {
        self.data.iter().copied().collect()
    }
}

/// Squish timebase doctrine (house history):
/// - v9.8: interval MAXIMA, not averages or point-samples. Point-sampling and re-averaging made peaks blink in/out.
/// - v9.9: stepped third-by-third warp. The timeline visibly compresses but peaks persist.
///   Peaks must never blink in and out of the graph — a tall value must survive every squished frame.
pub fn squish_frame(raw: &[f64], cols: usize, _tick: u64) -> Vec<f64> {
    if raw.is_empty() {
        return vec![0.0; cols];
    }
    let w = cols;
    if raw.len() <= w {
        let mut out = vec![0.0; w - raw.len()];
        out.extend_from_slice(raw);
        return out;
    }
    
    let n = raw.len();
    let mut spans = Vec::with_capacity(w);
    // port: app.py:1947
    for i in 0..w {
        let x = (i as f64 + 0.5) / w as f64;
        let span = if x >= 2.0 / 3.0 {
            1.0
        } else if x >= 1.0 / 3.0 {
            2.0
        } else {
            3.0
        };
        spans.push(span);
    }
    
    // port: app.py:1951
    let sum_spans: f64 = spans.iter().sum();
    let mult = n as f64 / sum_spans;
    let mut edges = vec![0.0; w + 1];
    edges[w] = -0.5; // newest sample inclusive
    for i in (0..w).rev() {
        edges[i] = edges[i + 1] + spans[i] * mult;
    }
    
    // port: app.py:1956
    let mut out = Vec::with_capacity(w);
    for i in 0..w {
        let hi = edges[i];
        let lo = edges[i + 1];
        let kmin = (lo + 1e-9).floor() as isize + 1;
        let kmax = ((n - 1) as isize).min((hi + 1e-9).floor() as isize);
        
        if kmin <= kmax {
            let mut vmax = f64::NEG_INFINITY;
            let start = (n as isize - 1 - kmax).max(0) as usize;
            let end = (n as isize - kmin).max(0) as usize;
            for val in &raw[start..end] {
                if *val > vmax {
                    vmax = *val;
                }
            }
            if vmax == f64::NEG_INFINITY {
                vmax = 0.0;
            }
            out.push(vmax);
        } else {
            let mid = (lo + hi) / 2.0;
            let fi = ((n as f64 - 1.0 - mid).max(0.0)).min(n as f64 - 1.0);
            let i0_ = fi as usize;
            let i1_ = (i0_ + 1).min(n - 1);
            let fr = fi - (i0_ as f64);
            out.push(raw[i0_] * (1.0 - fr) + raw[i1_] * fr);
        }
    }
    out
}

use crate::engine::leaky::LeakyMax;

pub enum Norm<'a> {
    /// per-graph session leaky max for flow-type callers
    /// v9.12b lesson: normalization must stay per-graph (the v9.12 shared-group
    /// experiment pinned GEN to the ramp floor and was reverted in v9.14).
    /// Using `&mut LeakyMax` enforces this — Rust's borrow checker makes sharing impossible.
    Leaky(&'a mut LeakyMax, usize),
    /// explicit fixed_max for pct graphs
    Fixed(f64),
}

pub fn to_columns(series: &[f64], _w: u16, _h: u16, norm: Norm) -> (Vec<f64>, f64) {
    let vmax = match norm {
        Norm::Fixed(max) => max,
        Norm::Leaky(leaky, lookback) => {
            let recent = if lookback > 0 && lookback < series.len() {
                &series[series.len() - lookback..]
            } else {
                series
            };
            let mut peak = 0.0_f64;
            for &v in recent {
                if v > peak {
                    peak = v;
                }
            }
            leaky.observe(peak);
            
            // _scale_max quantize logic from app.py:1886
            let t = peak * 1.05 + 0.5;
            if t <= 0.0 {
                1.0
            } else {
                let eighth = peak / 8.0;
                let e = if eighth > 0.0 {
                    10.0_f64.powf(eighth.log10().floor())
                } else {
                    1.0
                };
                let mults = [1.0, 1.25, 1.5, 2.0, 2.5, 5.0, 10.0];
                let mut best_m = 10.0;
                let mut best_score = (1, 10.0_f64);
                for &m in &mults {
                    let s = if m * e >= eighth - 1e-12 { 0 } else { 1 };
                    let score = (s, m);
                    if score < best_score {
                        best_score = score;
                        best_m = m;
                    }
                }
                let step = (best_m * e).max(1e-9);
                (t / step).ceil() * step
            }
        }
    };
    
    let out = series.iter().map(|&v| {
        if vmax > 0.0 {
            v / vmax
        } else {
            0.0
        }
    }).collect();
    (out, vmax)
}
