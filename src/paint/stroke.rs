//! Row painters ported law-for-law from the reference styling engine.
//!
//! The law (why each rule exists — every one of these fixed a real
//! visual bug):
//! * A graph is a STROKE, not a dot patchwork: heights are interpolated
//!   per DOT column (2 per cell), and each dot paints the segment
//!   between its neighbouring dot heights — steep moves light a whole
//!   vertical run, so the line is connected across every cell boundary.
//! * `bar` fills both dot columns of a cell to that SAMPLE's value,
//!   quantized to whole dot rows (one visible column per sample).
//! * `area` fills from the baseline up to the stroke edge (the max of
//!   the two segments touching each dot column) — a solid body with a
//!   crisp interpolated edge, no row-quantized stair steps and no holes.
//! * Idle columns ride a rail: an empty bottom cell paints the rail
//!   dots (braille ⣀ / block ▄) in the dim rail colour; a painted cell
//!   whose value is idle (~0) keeps its pattern but rides the rail
//!   colour too.
//! * Quantize LAST: per-cell fill amounts are rounded after subtracting
//!   the cell's baseline, never from a column-level rounded total.
//! * 2x2 quadrant art (chars=line / block+line) is the solid-cell
//!   analogue of the braille stroke — one-pixel thickness, gapless at
//!   cell crossings, no wedging. Index order is UL*8|UR*4|LL*2|LR*1;
//!   any other order flips every glyph vertically.

/// Fill semantics shared by the braille and block painters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillMode {
    Line,
    Bar,
    Area,
}

/// One painted cell: dot pattern (braille bits) or quadrant pattern
/// (quad bits), the absolute dot/pixel row of the top lit dot (-1 when
/// nothing lit), and the column height used for colour + idle law.
#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub pat: u32,
    pub top: i32,
    pub col_h: f64,
}

const DOT_L: [u32; 4] = [0x01, 0x02, 0x04, 0x40]; // dots 1,2,3,7 (left col)
const DOT_R: [u32; 4] = [0x08, 0x10, 0x20, 0x80]; // dots 4,5,6,8 (right col)

/// braille ⣀ (dots 7+8) — the rail under idle columns.
pub const RAIL_DOT: char = '\u{28C0}';
/// ▄ — the rail for the quadrant painter.
pub const RAIL_QUAD: char = '\u{2584}';
/// ▁..█ eighth bars.
const BAR8: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}',
    '\u{2587}', '\u{2588}',
];

/// 2x2 quadrant glyphs, index = UL*8 | UR*4 | LL*2 | LR*1 (v7 shipped a
/// swapped order and every glyph rendered vertically flipped).
pub const QG: [char; 16] = [
    ' ', '\u{2597}', '\u{2596}', '\u{2584}', //     ▗      ▖      ▄
    '\u{259D}', '\u{2590}', '\u{259E}', '\u{259F}', // ▝      ▐      ▞      ▟
    '\u{2598}', '\u{259A}', '\u{258C}', '\u{2599}', // ▘      ▚      ▌      ▙
    '\u{2580}', '\u{259C}', '\u{259B}', '\u{2588}', // ▀      ▜      ▛      █
];

/// Interpolated dot-column heights (0..hdots, clamped just BELOW the top
/// so a vmaxed value never quantizes one past the grid — that bug left
/// the top row empty for a maxed graph). `hdots` is h*4 for braille,
/// h*2 for the quadrant painter.
pub fn heights(cols: &[f64], w: usize, hdots: usize) -> Vec<f64> {
    let h = hdots as f64;
    if cols.is_empty() {
        return vec![0.0; 2 * w];
    }
    let last = cols.len() - 1;
    (0..2 * w)
        .map(|px| {
            let smp = (px as f64 / 2.0 - 0.5).clamp(0.0, last as f64);
            let i0 = smp as usize;
            let i1 = (i0 + 1).min(last);
            let v = cols[i0] + (cols[i1] - cols[i0]) * (smp - i0 as f64);
            v.clamp(0.0, 1.0) * (h - 0.01)
        })
        .collect()
}

/// One braille row (y counted from the TOP, 0..h) for the given fill
/// mode. `dh` comes from `heights(cols, w, h*4)`.
pub fn braille_row(
    cols: &[f64],
    dh: &[f64],
    mode: FillMode,
    w: usize,
    h: usize,
    y: usize,
) -> Vec<Cell> {
    let hd = (h * 4) as f64;
    let base = ((h - 1 - y) * 4) as i32;
    let mut out = Vec::with_capacity(w);
    let mut prev_h = dh.first().copied().unwrap_or(0.0);
    for x in 0..w {
        let (mut left_h, mut right_h) = (dh[2 * x], dh[2 * x + 1]);
        let hb = (cols[x.min(cols.len() - 1)].clamp(0.0, 1.0)) * (hd - 0.01);
        if mode == FillMode::Bar {
            left_h = hb;
            right_h = hb;
            prev_h = hb;
        }
        let mut pat = 0u32;
        let mut top = -1i32;
        for r in 0..4 {
            let a = base + (3 - r) as i32;
            let (lo_l, hi_l) = if prev_h < left_h { (prev_h, left_h) } else { (left_h, prev_h) };
            let (lo_r, hi_r) = if left_h < right_h { (left_h, right_h) } else { (right_h, left_h) };
            let lit_l = match mode {
                // stroke: the segment between neighbouring heights (int-
                // truncated bounds — the thickening that keeps the line
                // gapless)
                FillMode::Line => lo_l as i32 <= a && a <= hi_l as i32,
                // bar: solid up to this sample's own dot height
                FillMode::Bar => a <= left_h as i32,
                // area: fill from the baseline to the stroke edge
                FillMode::Area => a <= hi_l as i32,
            };
            let lit_r = match mode {
                FillMode::Line => lo_r as i32 <= a && a <= hi_r as i32,
                FillMode::Bar => a <= right_h as i32,
                FillMode::Area => a <= hi_r as i32,
            };
            if lit_l {
                pat |= DOT_L[r];
            }
            if lit_r {
                pat |= DOT_R[r];
            }
            if (lit_l || lit_r) && top < 0 {
                top = a;
            }
        }
        prev_h = if mode == FillMode::Bar { hb } else { dh[2 * x + 1] };
        let col_h = if mode == FillMode::Bar { hb } else { left_h.max(right_h) };
        out.push(Cell { pat, top, col_h });
    }
    out
}

/// One row of the 2x2 quadrant painter (chars=line, block+line): the
/// same segment law as braille but each cell is 2 pixels tall.
pub fn quad_row(dh: &[f64], w: usize, h: usize, y: usize) -> Vec<Cell> {
    let base = ((h - 1 - y) * 2) as i32;
    let mut out = Vec::with_capacity(w);
    let mut prev_h = dh.first().copied().unwrap_or(0.0);
    for x in 0..w {
        let (left_h, right_h) = (dh[2 * x], dh[2 * x + 1]);
        let mut px = [false; 4]; // UL UR LL LR
        let mut top = -1i32;
        for py in 0..2 {
            let a = base + (1 - py) as i32;
            let (lo_l, hi_l) = if prev_h < left_h { (prev_h, left_h) } else { (left_h, prev_h) };
            let (lo_r, hi_r) = if left_h < right_h { (left_h, right_h) } else { (right_h, left_h) };
            let lit_l = lo_l as i32 <= a && a <= hi_l as i32;
            let lit_r = lo_r as i32 <= a && a <= hi_r as i32;
            if lit_l {
                px[py * 2] = true;
            }
            if lit_r {
                px[py * 2 + 1] = true;
            }
            if (lit_l || lit_r) && top < 0 {
                top = a;
            }
        }
        prev_h = right_h;
        let pat = (px[0] as u32) * 8
            + (px[1] as u32) * 4
            + (px[2] as u32) * 2
            + px[3] as u32;
        let col_h = left_h.max(right_h);
        out.push(Cell { pat, top, col_h });
    }
    out
}

/// One row of the solid-block fill painter (block + bar/area). `ls` are
/// the per-column smoothed levels from `block_levels`; `pat` carries the
/// quantized level 0..8 for the cell.
pub fn block_row(ls: &[f64], w: usize, h: usize, y: usize) -> Vec<Cell> {
    let b = h - y; // 1..h from the bottom
    let base = ((b - 1) * 8) as f64;
    let mut out = Vec::with_capacity(w);
    for &l in ls.iter().take(w) {
        let fill = (l - base).clamp(0.0, 8.0);
        let lv = fill.round() as u32; // quantize LAST, after base
        out.push(Cell { pat: lv, top: -1, col_h: l });
    }
    out
}

/// Per-column eighth-levels for block bar/area: bar quantizes per
/// sample; area applies the 1-2-1 smoothing (w>2) BEFORE rounding.
pub fn block_levels(cols: &[f64], w: usize, h: usize, area: bool) -> Vec<f64> {
    let levels = (h * 8) as f64;
    let raw: Vec<f64> = (0..w)
        .map(|x| cols[x.min(cols.len().saturating_sub(1))].clamp(0.0, 1.0) * (levels - 0.01))
        .collect();
    if !area || w <= 2 {
        return raw;
    }
    (0..w)
        .map(|x| {
            (raw[x.saturating_sub(1)]
                + 2.0 * raw[x]
                + raw[(x + 1).min(w - 1)])
                / 4.0
        })
        .collect()
}

/// braille glyph for a pattern.
pub fn dot_glyph(pat: u32) -> char {
    char::from_u32(0x2800 + pat).unwrap_or(' ')
}

/// eighth bar for a quantized level 1..8 (0 = caller handles the rail).
pub fn bar_glyph(lv: u32) -> char {
    BAR8[(lv.clamp(1, 8) - 1) as usize]
}
