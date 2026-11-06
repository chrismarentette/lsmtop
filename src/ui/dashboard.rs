//! v9.15-faithful metrics shell (): banner, stats line,
//! FOUR stacked history graphs (GEN leaky / PROMPT leaky / KV≈ fixed 100 /
//! GPU% fixed 100, backend-titled) each with an nvtop corner legend
//! (title dim + run|now / avg / max heat-tinted) AND the nvtop overlay
//! (quartile ticks on the left edge, ceiling top-right, live value cursor
//! dodging the trace), the rounded-box 8-column lane table, and the hotkey
//! strip. M6-era two-graph skeleton replaced 2026-11-04 (maintainer caught the
//! single metrics shell; shell fidelity IS the product). The
//! pty-vs-pty audit the same day caught the missing overlay/box/header —
//! parity is measured against the screen, never our own snapshot.
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Widget},
};
use crate::engine::leaky::LeakyMax;
use crate::hist::Ring;
use crate::ui::graph_widget::{Graph, GraphOpts, GraphStats};

/// nvtop corner legend for one graph (): dim title,
/// then name:value pairs, value colored by _heat_style(val / 100).
#[derive(Clone, Copy)]
pub struct Legend<'a> {
    pub title: &'a str,
    pub head: (&'a str, f64),
    pub avg: f64,
    pub max: f64,
}

/// G1: per-pane display size. Full = weight 2, Half = weight 1, Single =
/// exactly ONE row: the nvtop legend line plus a trailing braille sparkline
/// (graphs) or a one-line ticker of the newest entry (journal).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaneSize {
    Full,
    Half,
    Single,
}

impl PaneSize {
    pub fn from_mode(s: &str) -> Self {
        match s {
            "half" => PaneSize::Half,
            "single" => PaneSize::Single,
            _ => PaneSize::Full,
        }
    }
    fn weight(self) -> u16 {
        match self {
            PaneSize::Full => 2,
            _ => 1,
        }
    }
}

/// G1: weighted panel heights over the graphs area. Full weighs 2, half
/// weighs 1 and GROWS with the leftover (the weights are floors the whole
/// area is shared by); single weighs nothing and sits at exactly one row.
/// Every non-single panel keeps >= 3 rows when the area allows (stealing
/// from the richest, B2 donor ladder), and the sum is ALWAYS exactly the
/// available height unless every visible pane is single (then the block is
/// as tall as its panes and the remainder is the table's to want).
pub fn graph_heights(avail: u16, vis: &[bool; 4], sizes: &[PaneSize; 4]) -> Vec<u16> {
    let idx: Vec<usize> = (0..4).filter(|i| vis[*i]).collect();
    if idx.is_empty() || avail == 0 {
        return Vec::new();
    }
    let n = idx.len();
    if avail < n as u16 {
        // tiny area: first come, one row each
        return (0..n).map(|i| if (i as u16) < avail { 1 } else { 0 }).collect();
    }
    let single: Vec<bool> = idx.iter().map(|i| sizes[*i] == PaneSize::Single).collect();
    let n_single = single.iter().filter(|s| **s).count() as u16;
    let rest = avail - n_single;
    let w: Vec<u16> = idx
        .iter()
        .map(|i| if sizes[*i] == PaneSize::Single { 0 } else { sizes[*i].weight() })
        .collect();
    let tw: u16 = w.iter().sum();
    let mut h: Vec<u16> = if tw == 0 {
        vec![0; n]
    } else {
        let hv: Vec<u16> = w.iter().map(|x| rest * x / tw).collect();
        let mut rem = rest - hv.iter().sum::<u16>();
        // remainder first to the heavier panels (GEN outranks on weight ties)
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|a, b| w[*b].cmp(&w[*a]).then(a.cmp(b)));
        let order: Vec<usize> = order.into_iter().filter(|i| w[*i] > 0).collect();
        let mut k = 0usize;
        let mut hv = hv;
        while rem > 0 && !order.is_empty() {
            hv[order[k % order.len()]] += 1;
            rem -= 1;
            k += 1;
        }
        hv
    };
    for i in 0..n {
        if single[i] {
            h[i] = 1;
        }
    }
    // B2 minimum-3 floor for non-single panels: donors are taken in SHRINK
    // PRIORITY over the graph ids (KV first out, then GPU, PROMPT; GEN
    // shrinks LAST); single panels are exempt and never donate.
    for i in 0..n {
        if single[i] {
            continue;
        }
        while h[i] < 3 {
            let donor = SHRINK_ORDER
                .iter()
                .find(|g| **g != idx[i] && (0..n).any(|j| idx[j] == **g && !single[j] && h[j] > 3))
                .and_then(|g| (0..n).position(|j| idx[j] == *g && !single[j]));
            match donor {
                Some(d) => {
                    h[d] -= 1;
                    h[i] += 1;
                }
                None => break,
            }
        }
    }
    h
}

/// G1: the minimum honest height of the whole graphs block — 3 rows per
/// full/half panel, 1 per single. Drives the graphs/table split AND the
/// journal pane's shrink ladder (scaling-priority law: the journal flexes
/// before graphs lose their minimum).
pub fn graphs_min(vis: &[bool; 4], sizes: &[PaneSize; 4]) -> u16 {
    (0..4)
        .filter(|i| vis[*i])
        .map(|i| if sizes[i] == PaneSize::Single { 1 } else { 3 })
        .sum()
}

/// B2: the graphs/table split. Returns (content_h, table_h, window_h).
/// Roomy regime (ideal >= min): table gets its content want, graphs own
/// the rest, pane content == window (no scroll). Squeezed regime: the
/// table drops toward a stub so graphs hold their 3-row minimum; below
/// that the graphs window is smaller than the min content and the pane
/// becomes wheel-scrollable instead of crushing panels. Invariants:
/// table_h + window_h == avail, and window_h >= min iff avail >= min.
pub fn graph_layout(avail: u16, rows: u16, min_graph: u16) -> (u16, u16, u16) {
    let want_table = (rows + 3).min((avail / 5 * 3).max(3)); // R6: exact border+header+rows, no blank row; never clamp(min>max)
    let ideal = avail.saturating_sub(want_table);
    if ideal >= min_graph {
        return (ideal, avail - ideal, ideal);
    }
    let table_h = avail.saturating_sub(min_graph);
    (min_graph, table_h, avail - table_h)
}

/// B2 resize scaling priority: graphs lose rows in this order and gain
/// them in reverse — GEN grows FIRST and shrinks LAST; KV shrinks FIRST
/// and grows last; GPU and PROMPT sit between, in that order.
pub const SHRINK_ORDER: [usize; 4] = [2, 3, 1, 0];

fn heat(frac: f64) -> Color {
    // : green -> amber -> red on value/100.
    let f = frac.clamp(0.0, 1.0);
    if f >= 0.8 {
        Color::Red
    } else if f >= 0.5 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// avg over the LAST 60 samples, max over the whole ring ().
pub fn ring_stats(ring: &Ring) -> (f64, f64) {
    let v = ring.values();
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let mx = v.iter().cloned().fold(f64::MIN, f64::max);
    let start = v.len().saturating_sub(60);
    let win = &v[start..];
    let avg = win.iter().sum::<f64>() / win.len() as f64;
    (avg, mx)
}

/// : k-suffix over 999.5, integer over 99.5,
/// one decimal above 9.95, else one decimal — and a bare "0" for zero
/// (idle rails must read "0", not "0.0").
pub fn fmt_v(v: f64) -> String {
    let v = if v == 0.0 { 0.0 } else { v }; // IEEE -0.0 -> "0"
    let a = v.abs();
    if a >= 999.5 {
        format!("{:.1}k", v / 1000.0)
    } else if a >= 99.5 {
        format!("{v:.0}")
    } else {
        // 9.95..99.5 and everything below print one decimal; zero is bare.
        if v == 0.0 {
            "0".to_string()
        } else {
            format!("{v:.1}")
        }
    }
}

fn put(buf: &mut Buffer, mut x: u16, y: u16, right: u16, text: &str, style: Style) {
    for ch in text.chars() {
        if x >= right {
            return;
        }
        buf[(x, y)].set_char(ch).set_style(style);
        x += 1;
    }
}

/// G2: the activation letter each graph title shows for its hotkey —
/// **G**EN, **P**ROMPT, K**V**, G**U**P**U**/A**U** (GPU moved to the letter
/// 'u' because 'g' now drives GEN). None = no accent.
fn accent_index(title: &str) -> Option<usize> {
    if let Some(p) = title.find("GEN") {
        Some(p) // **G**EN
    } else if title.starts_with("PROMPT") {
        Some(0) // **P**ROMPT
    } else if title.starts_with("KV") {
        Some(1) // K**V**
    } else if let Some(p) = title.find("GPU") {
        Some(p + 2) // GP**U**
    } else if title.starts_with("APU") {
        Some(2) // AP**U** — 'u' is the GPU/APU hotkey letter
    } else {
        None
    }
}

fn draw_legend(buf: &mut Buffer, area: Rect, lg: &Legend) -> u16 {
    if area.height == 0 {
        return 0;
    }
    let mut x = area.x + 1;
    let right = area.x + area.width;
    // B5: the activation letter of each graph title rides bold+yellow —
    // GE*N*, *P*ROMPT, K*V*, *G*PU (APU proxies its first letter).
    let acc = accent_index(lg.title);
    for (j, ch) in lg.title.chars().enumerate() {
        let st = if Some(j) == acc {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        put(buf, x, area.y, right, &ch.to_string(), st);
        x += 1;
    }
    x += 1;
    for (name, val) in [lg.head, ("avg", lg.avg), ("max", lg.max)] {
        // IEEE -0.0 prints "-0.0" on idle rails; sums never hit it.
        let val = if val == 0.0 { 0.0 } else { val };
        put(buf, x, area.y, right, name, dim());
        x += name.chars().count() as u16 + 1;
        let txt = format!("{val:5.1}");
        put(buf, x, area.y, right, &txt, Style::default().fg(heat(val / 100.0)));
        x += txt.chars().count() as u16 + 1;
    }
    x - area.x
}

/// // port: does the curve occupy `row`
/// within the last ncols cells (value-label dodge)? band height is 4 dots
/// for braille, 2 for block.
#[allow(clippy::too_many_arguments)]
fn row_occupied(
    columns: &[f64],
    vmax: f64,
    h: u16,
    row: u16,
    ncols: u16,
    braille: bool,
) -> bool {
    if vmax <= 0.0 || columns.is_empty() || row >= h {
        return false;
    }
    let step: f64 = if braille { 4.0 } else { 2.0 };
    let big_h = f64::from(h) * step;
    let band_lo = f64::from(h - 1 - row) * step;
    let band_hi = band_lo + step;
    let take = (2 * ncols.max(1) as usize).min(columns.len());
    for v in &columns[columns.len() - take..] {
        let d = (v / vmax).clamp(0.0, 1.0) * (big_h - 0.01);
        if (band_lo..band_hi).contains(&d) {
            return true;
        }
    }
    false
}

/// // port: nvtop wastes no columns: scale
/// quartiles sit into the LEFT edge, the ceiling top-RIGHT, and the live
/// value rides at the end of the line with a one-row dodge. Old-school
/// line style gets no overlay .
/// B3 (backlog): left-edge scale labels must sit on EVENLY SPACED rows —
/// the quartile set (25/50/75) rasterizes to uneven gaps whenever h-1 is
/// not divisible by 4 (h=6 -> rows 1/3/4, h=7 -> rows 2/3/5). Rule: pick
/// the interval count k whose fractions land on uniform rows — 5 bands
/// (20/40/60/80) or 3 bands (33/66) — quartiles (4) and a lone 50 (2) as
/// fallbacks; a fixed 0-100 scale (KV/GPU) prefers its meaningful 25s.
/// Returns fractions top-down; the 0-floor label is added by the caller.
fn scale_ticks(h: u16, fixed_scale: bool) -> Vec<f64> {
    let span = h.saturating_sub(1);
    if span == 0 {
        return Vec::new();
    }
    // interval counts to try: even-spacing first; fixed scales lead with 25s
    let order: [u16; 4] = if fixed_scale { [4, 5, 3, 2] } else { [5, 3, 4, 2] };
    for k in order {
        let fr: Vec<f64> = (1..k).map(|i| 1.0 - f64::from(i) / f64::from(k)).collect();
        if uniform_rows(h, &fr).is_some() {
            return fr;
        }
    }
    // nothing divides evenly: one honest label centered, never an uneven set
    if span >= 2 {
        vec![0.5]
    } else {
        Vec::new()
    }
}

/// do the fractions rasterize to uniformly spaced rows (floor included)?
/// duplicate rows collapse (the caller dedupes); any ragged gap rejects.
fn uniform_rows(h: u16, fr: &[f64]) -> Option<Vec<u16>> {
    let span = f64::from(h) - 1.0;
    let mut rows: Vec<u16> = Vec::new();
    for f in fr {
        let r = ((1.0 - f) * span).round() as u16;
        if r == 0 || r >= h - 1 {
            continue; // row 0 is the legend; h-1 is the floor label
        }
        if rows.last() != Some(&r) {
            rows.push(r);
        } else {
            return None; // two labels crammed on one row
        }
    }
    if rows.len() < 2 {
        return Some(rows); // 0 or 1 label is trivially uniform
    }
    let gap = rows[1] - rows[0];
    if rows.windows(2).any(|pw| pw[1] - pw[0] != gap) || rows[rows.len() - 1] + gap != h - 1 {
        return None;
    }
    Some(rows)
}

fn draw_overlay(
    buf: &mut Buffer,
    area: Rect,
    st: &GraphStats,
    legend_w: u16,
    braille: bool,
    line_chars: bool,
    fixed_scale: bool,
) {
    if line_chars || area.width < 4 || area.height == 0 {
        return;
    }
    let (w, h) = (area.width, area.height);
    if h >= 4 {
        // left-edge even-spaced scale ticks + floor; ceiling lives top-right
        let mut ticks: Vec<(u16, f64)> = Vec::new();
        for frac in scale_ticks(h, fixed_scale) {
            let row = (((1.0 - frac) * (f64::from(h) - 1.0)).round()) as u16;
            if row > 0 && row < h - 1 && !ticks.iter().any(|(r, _)| *r == row) {
                ticks.push((row, frac));
            }
        }
        ticks.push((h - 1, 0.0));
        for (row, frac) in ticks {
            let t = fmt_v(st.vmax * frac);
            if row == 0 && w <= legend_w + 2 {
                continue; // legend owns row 0 when it is tight
            }
            put(buf, area.x, area.y + row, area.x + w, &format!(" {t}"), dim());
        }
        let t = fmt_v(st.vmax);
        if w >= t.chars().count() as u16 + 2 {
            put(
                buf,
                area.x + w - t.chars().count() as u16,
                area.y,
                area.x + w,
                &format!("{t} "),
                dim(),
            );
        }
    }
    // live value cursor at the line end, dodging rows the trace owns
    if st.n > 1 && w >= 8 && st.vmax > 0.0 {
        let v = if st.last == 0.0 { 0.0 } else { st.last };
        let t = fmt_v(v);
        let h2 = f64::from(h) * 2.0; // H2 = h*2 for every painter
        let r = (v / st.vmax).clamp(0.0, 1.0) * (h2 - 0.01);
        let ly = h - 1 - (r as u32 / 2) as u16;
        if v > 0.0 {
            let lt = t.chars().count() as u16;
            for cand in [ly.wrapping_sub(1), ly + 1, ly.wrapping_sub(2), ly + 2, ly] {
                if cand >= h {
                    continue;
                }
                // row 0 belongs to the legend and the ceiling number; the
                // label always ends at column w-1, so painting it there
                // merges digits into those numbers (maintainer: "add a number
                // to the number") — dodge could land there too
                if cand == 0 {
                    continue;
                }
                if row_occupied(&st.columns, st.vmax, h, cand, lt + 2, braille) {
                    continue;
                }
                let x0 = area.x + w.saturating_sub(lt + 1);
                put(buf, x0 - 1, area.y + cand, area.x + w, " ", dim());
                put(
                    buf,
                    x0,
                    area.y + cand,
                    area.x + w,
                    &t,
                    Style::default().fg(heat(((r + 1.0) / h2).min(1.0))),
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod overlay_tests {
    //! maintainer: a value near the ceiling used to paint its digits on top of
    //! the legend/ceiling row ("add a number to the number"). Row 0 is off
    //! limits for the value cursor.
    use super::*;
    use crate::ui::graph_widget::GraphStats;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    /// B3 law: labels always sit on uniformly spaced rows, and the sets
    /// are exactly the spec's (20/40/60/80, 33/66, quartiles, lone 50)
    #[test]
    fn graph_heights_sum_is_exact_across_size_mixes() {
        use PaneSize::{Full, Half, Single};
        let all = [true; 4];
        let full = [Full; 4];
        for avail in 0..40u16 {
            let h = graph_heights(avail, &all, &full);
            assert_eq!(h.iter().sum::<u16>(), avail, "avail={avail}");
            assert_eq!(h.len(), if avail == 0 { 0 } else { 4 });
        }
        // GEN half-sized: visibly smaller, still exact sum + min floor
        let h = graph_heights(18, &all, &[Half, Full, Full, Full]);
        assert!(h[0] < h[1] && h.iter().sum::<u16>() == 18 && h.iter().all(|&x| x >= 3), "{h:?}");
        // singles ride ONE row; the rest share the remainder exactly
        let h = graph_heights(18, &all, &[Single, Full, Single, Full]);
        assert_eq!((h[0], h[2]), (1, 1), "{h:?}");
        assert_eq!(h.iter().sum::<u16>(), 18);
        assert!(h[1] >= 3 && h[3] >= 3 && h[1] + h[3] == 16, "{h:?}");
        // all single: block collapses to one row per pane (the leftover
        // is the table's to want — documented degradation, no blank lies)
        assert_eq!(graph_heights(30, &all, &[Single; 4]), vec![1; 4]);
        // exact-sum law holds for every single/non-single mix too
        let sizes = [Single, Half, Single, Full];
        for avail in 4..40u16 {
            let h = graph_heights(avail, &all, &sizes);
            assert_eq!(h.iter().sum::<u16>(), avail, "avail={avail}: {h:?}");
            assert_eq!((h[0], h[2]), (1, 1), "avail={avail}: {h:?}");
        }
        // hidden graphs vanish; weights renormalize
        let h2 = graph_heights(12, &[true, true, true, false], &full);
        assert_eq!(h2.len(), 3);
        assert_eq!(h2.iter().sum::<u16>(), 12);
        // no visible graphs -> no rows at all
        assert!(graph_heights(20, &[false; 4], &full).is_empty());
        // graphs_min drives the honest block floor: 3 per full/half, 1 per single
        assert_eq!(graphs_min(&all, &full), 12);
        assert_eq!(graphs_min(&[true, true, false, false], &[Single, Half, Full, Full]), 4);
    }

    #[test]
    fn b2_graph_layout_regimes() {
        // roomy: table gets its content want; content == window, no scroll
        assert_eq!(graph_layout(30, 8, 12), (19, 11, 19)); // R6: border+header+rows exactly
        // squeezed: table falls toward a stub, graphs hold the minimum
        assert_eq!(graph_layout(14, 8, 12), (12, 2, 12));
        // tiny: window < content -> the pane is wheel-scrollable by 2
        assert_eq!(graph_layout(10, 8, 12), (12, 0, 10));
        // invariants across the range: exact split, window >= min when avail >= min
        for avail in 0..40u16 {
            let (c, t, g) = graph_layout(avail, 8, 12);
            assert_eq!(t + g, avail, "avail {avail}");
            assert!(c >= g, "avail {avail}");
            if avail >= 12 {
                assert!(g >= 12, "avail {avail}: window lost the minimum");
            }
            if avail >= 20 {
                assert_eq!(c, g, "avail {avail}: roomy must not scroll");
            }
        }
    }

    #[test]
    fn b2_donor_order_shrinks_kv_first_and_gen_last() {
        // size mix [GEN,PROMPT,KV,GPU] = [half,full,full,half]: floor
        // repair must steal from KV (shrink order KV,GPU,PROMPT,GEN),
        // never from GEN while anyone else can pay; singles never donate.
        use PaneSize::{Full, Half, Single};
        let h = graph_heights(13, &[true; 4], &[Half, Full, Full, Half]);
        assert_eq!(h.iter().sum::<u16>(), 13);
        assert!(h.iter().all(|&x| x >= 3), "{h:?}");
        for avail in 12..=24 {
            let hh = graph_heights(avail, &[true; 4], &[Half, Full, Full, Half]);
            assert_eq!(hh.iter().sum::<u16>(), avail, "avail {avail}");
            assert!(hh.iter().all(|&x| x >= 3), "avail {avail}: {hh:?}");
        }
        // a single pane is exempt from the floor and never donates
        let hs = graph_heights(11, &[true; 4], &[Single, Full, Full, Full]);
        assert_eq!(hs[0], 1);
        assert_eq!(hs.iter().sum::<u16>(), 11);
        // GEN grows FIRST: with equal weights every leftover row lands on
        // GEN, so GEN >= every other panel at all heights.
        for avail in 12..=24 {
            let hh = graph_heights(avail, &[true; 4], &[Full; 4]);
            assert!(hh.iter().all(|&x| hh[0] >= x), "avail {avail}: {hh:?}");
        }
        // The documented shrink ladder itself: KV, GPU, PROMPT, GEN last.
        assert_eq!(SHRINK_ORDER, [2, 3, 1, 0]);
    }

    #[test]
    fn accent_letters_mark_the_hotkey_in_every_title() {
        // G2: GEN->g, PROMPT->p, KV->v, GPU/APU->u (u freed when 'g' moved)
        let t = |s: &'static str| accent_index(s);
        assert_eq!(t("GEN t/s").map(|i| &"GEN t/s"[i..i + 1]), Some("G"));
        assert_eq!(t("PROMPT t/s").map(|i| &"PROMPT t/s"[i..i + 1]), Some("P"));
        assert_eq!(t("GPU %").map(|i| &"GPU %"[i..i + 1]), Some("U"));
        assert_eq!(t("KV\u{2248} %").map(|i| &"KV\u{2248} %"[i..i + 1]), Some("V"));
        assert_eq!(t("APU %").map(|i| &"APU %"[i..i + 1]), Some("U"));
        assert_eq!(accent_index("other"), None);
    }

    #[test]
    fn strip_layout_columns_match_the_painted_letters() {
        // G2 drift trap: the widget paints each WORD with the hotkey
        // letter at token.hot; the hit map uses strip_layout columns.
        // One law — the recorded hot column MUST be the hot letter.
        let toks = metrics_strip(true);
        let (text, cols) = strip_layout(&toks);
        assert_eq!(cols.len(), toks.len());
        for (i, (start, hot, key)) in cols.iter().enumerate() {
            let t = &toks[i];
            // hot column lands on the token's hotkey letter inside the word
            let ch = text.chars().nth(*hot as usize).unwrap();
            assert_eq!(ch, t.label.chars().nth(t.hot).unwrap(), "tok {i} hot");
            assert_eq!(
                ch.to_ascii_lowercase(),
                key.to_ascii_lowercase(),
                "tok {i}: hot letter must BE the key"
            );
            assert_eq!(text.chars().nth(*start as usize).unwrap(), t.label.chars().next().unwrap());
            assert!(hot >= start && hot - start < t.label.chars().count() as u16);
        }
        // words ascend and never collide (two-space gaps)
        let mut last_end = 0u16;
        for (i, (start, _hot, _)) in cols.iter().enumerate() {
            assert!(*start >= last_end, "tok {i} overlaps");
            last_end = start + toks[i].label.chars().count() as u16 + 2;
        }
        // btop style: NO separate letter prefixes — the strip is words
        assert!(text.contains("GEN") && text.contains("KV") && text.contains("GPU"));
        assert!(text.contains("All/Active") && text.contains("Llama") && text.contains("Journal"));
        // router-only strips drop Quit+Stop but keep Quit
        let no_side = metrics_strip(false);
        assert!(!no_side.iter().any(|t| t.key == 'Q'));
        assert!(no_side.iter().any(|t| t.key == 'q'));
        // GPU/APU rides the letter U inside the word (hot index 2)
        let gpu = toks.iter().find(|t| t.key == 'u').unwrap();
        assert_eq!(gpu.label.chars().nth(gpu.hot), Some('U'));
    }

    #[test]
    fn scale_ticks_are_always_evenly_spaced() {
        for h in 4..=24 {
            let fr = scale_ticks(h, false);
            if fr.len() < 2 {
                continue; // 0 or 1 label is trivially even
            }
            let rows: Vec<u16> = fr
                .iter()
                .map(|f| ((1.0 - f) * (f64::from(h) - 1.0)).round() as u16)
                .collect();
            let gaps: Vec<u16> = rows.windows(2).map(|p| p[1] - p[0]).collect();
            assert!(gaps.windows(2).all(|g| g[0] == g[1]), "h={h} uneven: {rows:?}");
            // uniform with the floor row too (the 0 label sits on h-1)
            assert_eq!(
                rows[rows.len() - 1] + gaps[0],
                h - 1,
                "h={h} floor gap: {rows:?}"
            );
        }
        assert_eq!(scale_ticks(6, false).len(), 4); // 20/40/60/80
        assert_eq!(scale_ticks(7, false).len(), 2); // 33/66
        assert_eq!(scale_ticks(5, true), vec![0.75, 0.5, 0.25]); // KV quartiles
    }

    #[test]
    fn value_cursor_never_paints_row0() {
        let area = Rect { x: 0, y: 0, width: 40, height: 6 };
        let mut buf = Buffer::empty(area);
        // 85% of ceiling: the curve leaves row 0 free — exactly the case
        // where the old dodge parked the value label on the ceiling text
        let st = GraphStats { vmax: 100.0, columns: vec![85.0; 30], last: 85.0, n: 30 };
        draw_overlay(&mut buf, area, &st, 20, true, false, false);
        let row_text = |r: u16| -> String {
            (0..area.width)
                .map(|x| buf[(x, r)].symbol().to_string())
                .collect::<String>()
        };
        let needle = fmt_v(85.0); // "85.0"
        assert!(!row_text(0).contains(&needle), "value leaked onto legend row: {:?}", row_text(0));
        assert!(
            (1..area.height).any(|r| row_text(r).contains(&needle)),
            "value label vanished entirely"
        );
    }
}

#[derive(Clone)]
pub struct DashRow {
    /// LANES, ACT, PROMPT t/s, TOTAL, GEN t/s, KV≈%, CACHE, CTX, ACCEPT
    pub cells: [String; 9],
}

/// B4: journal pane render input; `lines`/`scroll` mirror the pane state.
pub struct JournalPane<'a> {
    pub lines: &'a std::collections::VecDeque<ratatui::text::Line<'static>>,
    pub scroll: usize,
}

/// Journal sizing law (tiers: single / third): single rides the newest
/// entry — `single_rows` is how many wrapped rows that line needs (1-2,
/// from `journal_single_rows`), so a long line is shown WHOLE, not
/// chopped. third = a third of the screen. Full-height journal is a
/// SCREEN, not a pane tier. Scaling-priority: the journal flexes and
/// hides BEFORE graphs lose their minimums. The pane hides below a
/// 24-row screen (small-window law) or when the tier can't be honest.
pub fn journal_pane_h(screen_h: u16, mode: &str, graphs_min: u16, single_rows: u16) -> u16 {
    if screen_h < 24 || mode == "off" {
        return 0;
    }
    // 2 chrome rows (stats/hotkeys) + 3 table stub must survive
    let cap = screen_h.saturating_sub(5 + graphs_min);
    if cap < 1 {
        return 0;
    }
    let (want, min_ok) = match mode {
        "single" => (single_rows.clamp(1, 2), 1u16),
        "third" => (screen_h / 3, 4u16),
        _ => (0, 0),
    };
    if want == 0 {
        return 0;
    }
    let want = want.min(cap);
    if want < min_ok {
        return 0;
    }
    want
}

/// How many wrapped rows the newest journal entry needs in the single
/// tier: one line, or two when it soft-wraps at the pane width — the
/// whole log line stays readable. ONE law: the widget paints exactly
/// this many rows and the height mirror sizes for it (drift = clipped
/// line).
pub fn journal_single_rows(newest: Option<&ratatui::text::Line<'static>>, width: usize) -> u16 {
    match newest {
        None => 1,
        Some(l) => crate::ui::logs::wrap_line(l, width.max(1))
            .len()
            .clamp(1, 2) as u16,
    }
}

/// Fixed chrome the dashboard always reserves: stats line + hotkey
/// strip. (The old title line became the header bar's banner — the
/// title bar was superfluous, so the stats line moved up into it and
/// the body gained a row.)
pub const CHROME_H: u16 = 2;

/// The graphs+table area after chrome + journal pane — ONE law shared by
/// the widget and the wheel/hit mirrors in main (drift = wheel pans the
/// wrong band; the B4 mirror trap).
pub fn dash_avail_h(body_h: u16, pane_h: u16) -> u16 {
    body_h.saturating_sub(CHROME_H + pane_h)
}

/// Graphs block rect inside the (already side-margin'd) body.
pub fn graphs_rect(body: Rect, h: u16) -> Rect {
    Rect {
        x: body.x,
        y: body.y + 1, // stats above (the banner rides the header bar)
        width: body.width,
        height: h,
    }
}

/// Journal pane rect: bottom-anchored above the 1-row hotkey strip; the
/// SAME function feeds the widget paint and the wheel/hit mirror in main
/// (drift = wheel pans the wrong band; the B4 mirror trap). None = hidden.
pub fn journal_pane_rect(area: Rect, pane_h: u16) -> Option<Rect> {
    if pane_h == 0 {
        return None;
    }
    Some(Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2).max(1),
        y: area.y + area.height.saturating_sub(1 + pane_h),
        height: pane_h,
    })
}

/// G2: a bottom-strip hotkey token in btop style — the hotkey letter
/// rides INSIDE the word (accented like the graph-title letters), so the
/// strip stays compact. The whole word is the mouse target; clicking it
/// IS pressing it. Lowercase graph letters cycle the pane size,
/// Shift/uppercase toggles visibility.
#[derive(Clone, Copy)]
pub struct StripTok {
    pub label: &'static str,
    pub hot: usize, // index of the hotkey char inside label
    pub key: char,
}

pub fn metrics_strip(has_side: bool) -> Vec<StripTok> {
    let mut t = vec![
        StripTok { label: "All/Active", hot: 0, key: 'A' },
        StripTok { label: "Journal", hot: 0, key: 'j' },
        StripTok { label: "PROMPT", hot: 0, key: 'p' },
        StripTok { label: "GEN", hot: 0, key: 'g' },
        StripTok { label: "KV", hot: 1, key: 'v' },
        StripTok { label: "GPU", hot: 2, key: 'u' },
        StripTok { label: "HF-CLI", hot: 0, key: 'h' },
        StripTok { label: "Settings", hot: 0, key: 's' },
        StripTok { label: "Llama", hot: 0, key: 'L' },
        StripTok { label: "Quit", hot: 0, key: 'q' },
    ];
    if has_side {
        t.push(StripTok { label: "Quit+Stop", hot: 0, key: 'Q' });
    }
    t
}

/// The strip text plus, per token, the word-start COLUMN, the hotkey
/// letter's column and its key — one source feeding both the widget
/// paint and the mouse hit map (drift = dead clicks).
pub fn strip_layout(toks: &[StripTok]) -> (String, Vec<(u16, u16, char)>) {
    let mut s = String::from(" ");
    let mut cols: Vec<(u16, u16, char)> = Vec::new();
    for t in toks {
        let start = s.chars().count() as u16;
        s.push_str(t.label);
        s.push_str("  ");
        cols.push((start, start + t.hot as u16, t.key));
    }
    (s, cols)
}

pub fn strip_text(toks: &[StripTok]) -> String {
    strip_layout(toks).0
}

pub struct DashboardWidget<'a> {
    pub stats: &'a str,
    pub hotkeys: &'a str,
    /// G2: styled hotkey tokens (letters carry the graph-title accent and
    /// are the mouse targets); empty = render `hotkeys` plain-dim.
    pub hotkey_toks: &'a [StripTok],
    pub opts: GraphOpts,
    pub tick: u64,
    pub gen: &'a Ring,
    pub gen_leaky: &'a mut LeakyMax,
    pub gen_legend: Legend<'a>,
    pub prompt: &'a Ring,
    pub prompt_leaky: &'a mut LeakyMax,
    pub prompt_legend: Legend<'a>,
    pub kv: &'a Ring,
    pub kv_legend: Legend<'a>,
    pub apu: &'a Ring,
    pub apu_legend: Legend<'a>,
    /// gpu_graph option: off removes the panel entirely ().
    /// visibility for [GEN, PROMPT, KV, GPU/APU]
    pub vis: [bool; 4],
    /// G1: per-graph display size (replaces the old B5 half booleans)
    pub sizes: [PaneSize; 4],
    /// B2: pane scroll offset (rows) when the graphs area is squeezed
    /// below the content minimum; clamped to max_scroll at draw.
    pub graph_scroll: u16,
    /// B4: journal lower pane (None = hidden). Lines come from the pane's
    /// own LogState; scroll is independent of the full-screen journal.
    pub journal: Option<JournalPane<'a>>,
    /// pane height already computed against the live screen (0 hides)
    pub journal_h: u16,
    pub rows: Vec<DashRow>,
}

// args mirror Graph's own field set — splitting further only obscures it.
#[allow(clippy::too_many_arguments)]
fn draw_graph(
    buf: &mut Buffer,
    area: Rect,
    hist: &Ring,
    leaky: Option<&mut LeakyMax>,
    fixed_max: Option<f64>,
    opts: &GraphOpts,
    tick: u64,
    legend: &Legend,
    single: bool,
) {
    if single || area.height <= 1 {
        // G1 single line: legend left, braille sparkline trailing right.
        let lw = draw_legend(buf, area, legend);
        let sw = area.width.saturating_sub(lw + 2);
        if sw >= 4 {
            let _ = Graph {
                hist,
                opts: opts.clone(),
                tick,
                leaky,
                fixed_max,
            }
            .render_spark(
                Rect {
                    x: area.x + lw + 1,
                    y: area.y,
                    width: sw,
                    height: 1,
                },
                buf,
            );
        }
        return;
    }
    let braille = matches!(opts.chars, crate::paint::glyphs::Chars::Braille(_));
    let line_chars = opts.chars == crate::paint::glyphs::Chars::Line;
    let lw = legend.title.chars().count() as u16 + 23; // "run 0.0 avg 0.0 max 0.0"
    let st = Graph {
        hist,
        opts: opts.clone(),
        tick,
        leaky,
        fixed_max,
    }
    .render_stats(area, buf);
    draw_legend(buf, area, legend);
    draw_overlay(buf, area, &st, lw, braille, line_chars, fixed_max.is_some());
}

impl Widget for DashboardWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // margins: banner/graphs/table sit inside a 1-col side
        // margin; the footer strip is full width.
        let body = Rect {
            x: area.x + 1,
            width: area.width.saturating_sub(2).max(1),
            ..area
        };
        // G1: table height is CONTENT-driven, capped at 60%; graphs own
        // the rest. One shared geometry law (dash_avail_h / graphs_rect /
        // journal_pane_rect) feeds the widget AND the wheel/hit mirrors.
        let pane_h = self
            .journal
            .as_ref()
            .map(|_| self.journal_h)
            .unwrap_or(0);
        let avail = dash_avail_h(area.height, pane_h);
        let vidx: Vec<usize> = (0..4).filter(|i| self.vis[*i]).collect();
        let min_graph = graphs_min(&self.vis, &self.sizes);
        // B2: two regimes. Roomy: the table takes its content want and
        // the graphs own the rest (weighted by pane size, GEN outranks).
        // Squeezed (avail < min + stub): graphs keep the minimum they can,
        // the table shrinks to a stub, and the overflow becomes scrollable
        // pane content instead of crushing panels below 3 rows.
        let (content_h, table_h, graph_h) = graph_layout(avail, self.rows.len() as u16, min_graph);
        let max_scroll = content_h.saturating_sub(graph_h);
        let scroll = self.graph_scroll.min(max_scroll);

        let mut y = body.y;
        let stats_r = Rect { x: body.x, y, width: body.width, height: 1 }; y += 1;
        let graphs_r = graphs_rect(body, graph_h);
        let _ = &mut y; // graphs/table tops derive from the shared fns now
        let table_r = Rect {
            x: body.x,
            y: graphs_r.y + graph_h,
            width: body.width,
            height: table_h,
        };
        let pane_r = journal_pane_rect(area, pane_h);

        Paragraph::new(self.stats).style(dim()).render(stats_r, buf);
        // (pane draws after the table; geometry below)

        // Graph panels (#mts-graphs, 1fr each): stacked, apu panel
        // removed entirely when the option is off.
        let gh = graph_heights(content_h, &self.vis, &self.sizes);
        // G1: only VISIBLE graphs stack, weighted by pane size (single =
        // one sparkline row; full/half share the rest 2:1).
        // B2 pane window: panels stack in content space; the window shows
        // a slice that the wheel scrolls (scroll==0 -> identical to the
        // classic layout, so roomy frames never change).
        let win_end = graphs_r.y + graph_h;
        let mut gy = graphs_r.y;
        for (i, &gi) in vidx.iter().enumerate() {
            let mut r = Rect { x: graphs_r.x, y: gy.saturating_sub(scroll), width: graphs_r.width, height: gh[i] };
            gy += gh[i];
            if r.y >= win_end {
                continue;
            }
            if r.y < graphs_r.y {
                let skip = graphs_r.y - r.y;
                r.y = graphs_r.y;
                r.height = r.height.saturating_sub(skip);
            }
            if r.height == 0 {
                continue;
            }
            let single = self.sizes[gi] == PaneSize::Single;
            match gi {
                0 => draw_graph(
                    buf,
                    r,
                    self.gen,
                    Some(self.gen_leaky),
                    None,
                    &self.opts,
                    self.tick,
                    &self.gen_legend,
                    single,
                ),
                1 => draw_graph(
                    buf,
                    r,
                    self.prompt,
                    Some(self.prompt_leaky),
                    None,
                    &self.opts,
                    self.tick,
                    &self.prompt_legend,
                    single,
                ),
                2 => draw_graph(
                    buf,
                    r,
                    self.kv,
                    None,
                    Some(100.0),
                    &self.opts,
                    self.tick,
                    &self.kv_legend,
                    single,
                ),
                _ => draw_graph(
                    buf,
                    r,
                    self.apu,
                    None,
                    Some(100.0),
                    &self.opts,
                    self.tick,
                    &self.apu_legend,
                    single,
                ),
            }
        }

        // Rounded-box table (#mts-table { border: round }; columns
        // LANES ACT PROMPT t/s TOTAL GEN t/s KV≈% CACHE CTX ACCEPT). LANES sizes
        // to content — fixed full-width stretch was the "bare" tell.
        let lane_w = self
            .rows
            .iter()
            .map(|r| r.cells[0].chars().count())
            .max()
            .unwrap_or(5)
            .max(5) as u16
            + 1;
        let inner_h = table_h.saturating_sub(2) as usize;
        let rows: Vec<Row> = self
            .rows
            .iter()
            .take(inner_h.saturating_sub(1))
            .map(|r| Row::new(r.cells.iter().cloned().map(Cell::from)))
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(lane_w),
                Constraint::Length(5),
                Constraint::Length(10),
                Constraint::Length(6),
                Constraint::Length(8),
                Constraint::Length(6),
                Constraint::Length(6),
                Constraint::Length(5),
                Constraint::Length(9),
            ],
        )
        .header(
            Row::new(vec![
                "LANES", "ACT", "PROMPT t/s", "TOTAL", "GEN t/s", "KV≈%", "CACHE", "CTX", "ACCEPT",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .column_spacing(2);
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .render(table_r, buf);
        // Journal pane: bare wrapped rows (the banner line was cut —
        // every pane row shows log now; the strip word marks the pane).
        // R1 soft-wrap; single tier = the newest line shown WHOLE
        // (1-2 wrapped rows, sized by journal_single_rows upstream).
        if let (Some(pr), Some(jp)) = (pane_r, self.journal.as_ref()) {
            if pr.height > 0 {
                crate::ui::logs::render_tail(
                    buf,
                    pr.x,
                    pr.y,
                    pr.width,
                    pr.height,
                    jp.lines,
                    jp.scroll,
                );
            }
        }
        let inner = Rect {
            x: table_r.x + 1,
            y: table_r.y + 1,
            width: table_r.width.saturating_sub(2),
            height: table_r.height.saturating_sub(2),
        };
        table.render(inner, buf);

        // G2 hotkey strip (btop style): the hotkey letter rides inside
        // each word in the graph-title yellow; the whole word is the
        // mouse target. Token columns match strip_layout so the hit map
        // in main lands exactly on the words.
        let strip_r = Rect {
            y: area.y + area.height.saturating_sub(1),
            width: area.width,
            height: 1,
            ..area
        };
        if self.hotkey_toks.is_empty() {
            Paragraph::new(self.hotkeys).style(dim()).render(strip_r, buf);
        } else {
            let right = strip_r.x + strip_r.width;
            let mut x = strip_r.x + 1;
            for t in self.hotkey_toks {
                let chars: Vec<char> = t.label.chars().collect();
                for (i, ch) in chars.iter().enumerate() {
                    let st = if i == t.hot {
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else {
                        dim()
                    };
                    put(buf, x, strip_r.y, right, &ch.to_string(), st);
                    x += 1;
                }
                x += 2; // two-space gap between tokens
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_v;

    // Golden values taken from 's own rules —
    // the boundaries are the interesting part (99.5/999.5 round UP into
    // the next format tier, and 0 is bare "0" not "0.0").
    #[test]
    fn fmt_tiers() {
        assert_eq!(fmt_v(0.0), "0");
        assert_eq!(fmt_v(-0.0), "0");
        assert_eq!(fmt_v(3.24), "3.2");
        assert_eq!(fmt_v(9.94), "9.9");
        assert_eq!(fmt_v(9.95), "9.9"); // 9.95 is 9.949999.. in binary — IEEE754 agrees
        assert_eq!(fmt_v(75.0), "75.0");
        assert_eq!(fmt_v(99.4), "99.4");
        assert_eq!(fmt_v(99.5), "100");
        assert_eq!(fmt_v(100.0), "100");
        assert_eq!(fmt_v(999.4), "999");
        assert_eq!(fmt_v(999.5), "1.0k");
        assert_eq!(fmt_v(1234.0), "1.2k");
    }
}
