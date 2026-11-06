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

/// Squish timebase v2 (B6): CONTINUOUS inverse-exponential warp with a
/// FROZEN bin layout.
/// - Density law: column span s(x) = R^(1-x), x = position left→right.
///   Old columns coarsen up to 6:1, the newest columns stay 1:1 — a
///   smooth curve replaces the old stepped third-by-third warp (whose
///   hard 1/3 boundaries produced visible ripple jumps in earlier
///   lineage).
/// - Anti-shimmer: bins are anchored at the OLDEST sample and only
///   re-laid-out every G samples (layout quantum). Between quanta the
///   frozen bins do not move at all — only the newest bin takes the live
///   tail — so old history can never shimmer as ticks arrive.
/// - Idle honesty: an idle span just renders flat zeros in stable bins;
///   nothing about the idle fill shifts the axis (the old right-anchored
///   edges shifted EVERY column on every tick — the "scale expands while
///   idle" artifact).
/// - Peaks survive (v9.8 doctrine unchanged): a bin shows the MAX of its
///   span, never an average, so a tall sample cannot blink out.
pub fn squish_frame(raw: &[f64], cols: usize, _tick: u64) -> Vec<f64> {
    const R: f64 = 6.0; // oldest column's coarseness
    const G: usize = 8; // layout quantum: bins freeze between steps
    if raw.is_empty() {
        return vec![0.0; cols];
    }
    let w = cols;
    if w == 0 {
        return Vec::new();
    }
    let n = raw.len();
    if n <= w {
        let mut out = vec![0.0; w - n];
        out.extend_from_slice(raw);
        return out;
    }
    let e_q = ((n - w) / G) * G; // frozen extra samples beyond 1:1
    let n_eff = w + e_q; // bins tile [0, n_eff)
    let mut spans = Vec::with_capacity(w);
    let mut sum = 0.0;
    for i in 0..w {
        let x = (i as f64 + 0.5) / w as f64;
        let s = R.powf(1.0 - x);
        sum += s;
        spans.push(s);
    }
    let mult = n_eff as f64 / sum;
    // left-anchored cumulative positions
    let mut pos = vec![0.0f64; w + 1];
    for i in 0..w {
        pos[i + 1] = pos[i] + spans[i] * mult;
    }
    pos[w] = n_eff as f64;
    let mut out = Vec::with_capacity(w);
    for i in 0..w {
        let a = if i == 0 { 0 } else { pos[i].ceil() as usize }.min(n_eff);
        let mut b = if i + 1 == w { n_eff } else { pos[i + 1].ceil() as usize }.min(n_eff);
        if b < a {
            b = a;
        }
        if i + 1 == w {
            // live tail (samples past the frozen layout) rides the newest bin
            b = n;
        }
        if a < b {
            let mut vmax = f64::NEG_INFINITY;
            for v in &raw[a..b] {
                if *v > vmax {
                    vmax = *v;
                }
            }
            out.push(if vmax == f64::NEG_INFINITY { 0.0 } else { vmax });
        } else {
            // bin narrower than one sample: interpolate at the column center
            let f = pos[i].min((n - 1) as f64);
            let i0 = f as usize;
            let i1 = (i0 + 1).min(n - 1);
            out.push(raw[i0] * (1.0 - (f - i0 as f64)) + raw[i1] * (f - i0 as f64));
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
            
            // _scale_max quantize logic from
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
