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

    
    /// Replace an entry counting back from the newest (`from_back = 1` is
    /// the previous sample). Task-6 gap law: the rail pushed for an
    /// evidence-less tick is a PLACEHOLDER, corrected by measurement once
    /// evidence returns — never by a held-over value (maintainer 2026-11-09).
    pub fn replace_from_back(&mut self, from_back: usize, val: f64) {
        if let Some(i) = self.data.len().checked_sub(from_back + 1) {
            self.data[i] = val;
        }
    }
    
    pub fn values(&self) -> Vec<f64> {
        self.data.iter().copied().collect()
    }
}

/// Near-field width of the squish warp: the last K columns are EXACTLY
/// 1:1 (indistinguishable from the scroll view). K is one third of the
/// width at most, and ~30 s of wall time at the current tick, floor 4.
pub fn squish_near_field(cols: usize, tick_ms: u64) -> usize {
    let t = if tick_ms == 0 { 2000 } else { tick_ms };
    let k_wall = (30_000u64 / t).clamp(4, 4096) as usize;
    (cols / 3).min(k_wall).max(1)
}

/// Squish timebase v4 (maintainer live 2026-11-07, refines v3): v3 fixed the
/// delay and the 1:1 right edge, but its geometric ratio had to cover the
/// whole ring, so the middle of the pane was already ~7:1 — "too wide,
/// slow to compress; the middle should be half time, not much tighter".
/// Law (recomputed EVERY frame, so a resize is honored instantly):
/// - Near field: last K columns exactly 1:1 (squish_near_field: ~30 s
///   wall time, max a third of the width).
/// - Tightening law: column j left of the near field spans
///   w_j = 1 + t_j^2 * (C - 1), t = (j-1)/(m-1) — a smooth curve that
///   starts hugging the near field. C is solved from the history budget
///   but CAPPED at 5: the visual centre of the pane is then ~2 samples
///   per column = half time, and the left edge stays at 5:1.
/// - Overflow honesty: when the ring holds more history than the capped
///   curve can tile, the OLDEST column absorbs the remainder (max of
///   span — a true peak still shows, never a fake one).
/// - Right-anchored and continuous (v3 law unchanged): every scrape
///   slides the picture; the newest sample IS the rightmost column.
/// - Peaks survive: bin = MAX of span, never average.
pub fn squish_frame(raw: &[f64], cols: usize, tick_ms: u64) -> Vec<f64> {
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
    let k = squish_near_field(w, tick_ms).min(n - 1);
    let edges = squish_edges(n, w, k);
    let m = edges.len() - 1; // compressible columns tiling raw[0 .. n-k)
    let mut out = Vec::with_capacity(w);
    for i in 0..m {
        let x0 = edges[i];
        let x1 = edges[i + 1];
        if x1 > x0 {
            let mut vmax = f64::NEG_INFINITY;
            for v in &raw[x0..x1] {
                if *v > vmax {
                    vmax = *v;
                }
            }
            out.push(if vmax == f64::NEG_INFINITY { 0.0 } else { vmax });
        } else {
            out.push(raw[x0.min(n - 1)]);
        }
    }
    out.extend_from_slice(&raw[n - k..]);
    out
}

/// squish v4 edge law, shared by the frame AND the seam mapping (one
/// source of truth: the seam must land in the bin the warp actually used).
fn squish_edges(n: usize, w: usize, k: usize) -> Vec<usize> {
    let m = w - k; // compressible columns tiling raw[0 .. n-k)
    let need = n - k;
    let mut edges = vec![0usize; m + 1];
    if m == 1 {
        edges[1] = need;
        return edges;
    }
    let mut s2 = 0.0f64;
    for j in 1..=m {
        let t = (j - 1) as f64 / (m - 1) as f64;
        s2 += t * t;
    }
    // solve exact tiling: m + (C-1)*S2 = need, cap C at 5 (left edge)
    let c = (1.0 + (need - m) as f64 / s2).clamp(1.0, 5.0);
    let mut pos = (need) as f64; // anchored at the near-field boundary
    edges[m] = need;
    for j in 1..=m {
        // bin j counts from the near field; leftmost (j=m) is widest
        let t = (j - 1) as f64 / (m - 1) as f64;
        let width = 1.0 + t * t * (c - 1.0);
        pos -= width;
        edges[m - j] = if pos <= 0.0 { 0 } else { (pos.round() as usize).min(need) };
    }
    edges[0] = 0; // overflow: the oldest column absorbs the remainder
    for i in 1..=m {
        if edges[i] < edges[i - 1] {
            edges[i] = edges[i - 1];
        }
    }
    edges
}

/// Seam mapping (lane-focus experiment, maintainer 2026-11-10): the vertical
/// line marking WHERE the curve switched lanes must land in the same
/// column the warp painted that sample into. `seam_back` counts samples
/// back from the newest (0 = newest). Returns the column (0 = leftmost of
/// the paint rect) or None when the sample is off-screen.
pub fn seam_column(raw_len: usize, seam_back: usize, cols: usize, tick_ms: u64) -> Option<usize> {
    let (w, n) = (cols, raw_len);
    if w == 0 || n == 0 || seam_back >= n {
        return None;
    }
    if n <= w {
        // left-padded 1:1 (both the padding case and plain scroll)
        return Some(w - 1 - seam_back);
    }
    let k = squish_near_field(w, tick_ms).min(n - 1);
    if seam_back < k {
        return Some(w - 1 - seam_back); // near field is 1:1
    }
    let sidx = n - 1 - seam_back; // sample index from the oldest
    let edges = squish_edges(n, w, k);
    // first bin whose end passes the sample
    edges.windows(2).position(|e| sidx >= e[0] && sidx < e[1])
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
