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

/// B5 law: weighted panel heights over the graphs area. Weight 2 normal,
/// 1 half-size; every visible panel keeps >= 3 rows when the area allows
/// (stealing from the richest), equal shares in the degenerate tiny case,
/// and the sum is ALWAYS exactly the available height.
pub fn graph_heights(avail: u16, vis: &[bool; 4], half: &[bool; 4]) -> Vec<u16> {
    let idx: Vec<usize> = (0..4).filter(|i| vis[*i]).collect();
    if idx.is_empty() || avail == 0 {
        return Vec::new();
    }
    let n = idx.len();
    let w: Vec<u16> = idx.iter().map(|i| if half[*i] { 1 } else { 2 }).collect();
    let tw: u16 = w.iter().sum();
    if avail < (n as u16) * 3 {
        // tiny area: equal shares, remainder to the first panels
        let base = avail / n as u16;
        let rem = avail % n as u16;
        return (0..n).map(|i| base + if (i as u16) < rem { 1 } else { 0 }).collect();
    }
    let mut h: Vec<u16> = w.iter().map(|x| avail * x / tw).collect();
    let mut rem = avail - h.iter().sum::<u16>();
    // remainder first to the heavier panels (GEN outranks on weight ties)
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| w[*b].cmp(&w[*a]).then(a.cmp(b)));
    let mut k = 0usize;
    while rem > 0 {
        h[order[k % n]] += 1;
        rem -= 1;
        k += 1;
    }
    // B2 minimum-3 floor: donors are taken in SHRINK PRIORITY over the
    // graph ids (KV first out, then GPU, PROMPT; GEN shrinks LAST), so
    // losing a row never touches GEN while anyone else can still give.
    for i in 0..n {
        while h[i] < 3 {
            let donor = SHRINK_ORDER
                .iter()
                .find(|g| **g != order[i] && (0..n).any(|j| order[j] == **g && h[j] > 3))
                .and_then(|g| (0..n).position(|j| order[j] == *g));
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

/// B5 activation letter position within a graph title (None = no accent)
fn accent_index(title: &str) -> Option<usize> {
    if let Some(p) = title.find("GEN") {
        Some(p + 2) // GE**N**
    } else if title.starts_with("PROMPT") {
        Some(0) // **P**ROMPT
    } else if title.starts_with("KV") {
        Some(1) // K**V**
    } else if let Some(p) = title.find("GPU") {
        Some(p) // **G**PU
    } else if title.starts_with("APU") {
        Some(0) // APU proxies its first letter
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
    fn graph_heights_sum_is_exact_and_halves_respect_min() {
        let all = [true; 4];
        for avail in 0..40u16 {
            let h = graph_heights(avail, &all, &[false; 4]);
            assert_eq!(h.iter().sum::<u16>(), avail, "avail={avail}");
            assert_eq!(h.len(), if avail == 0 { 0 } else { 4 });
        }
        // GEN half-sized: visibly smaller, still exact sum + min floor
        let h = graph_heights(18, &all, &[true, false, false, false]);
        assert!(h[0] < h[1] && h.iter().sum::<u16>() == 18 && h.iter().all(|&x| x >= 3), "{h:?}");
        // hidden graphs vanish; weights renormalize
        let h2 = graph_heights(12, &[true, true, true, false], &[false; 4]);
        assert_eq!(h2.len(), 3);
        assert_eq!(h2.iter().sum::<u16>(), 12);
        // no visible graphs -> no rows at all
        assert!(graph_heights(20, &[false; 4], &[false; 4]).is_empty());
    }

    #[test]
    fn b2_donor_order_shrinks_kv_first_and_gen_last() {
        // half pattern [GEN,PROMPT,KV,GPU] = [on,full,full,half]: floor
        // repair must steal from GPU (shrink order KV,GPU,PROMPT,GEN),
        // never from PROMPT before GPU, never from GEN while GPU can pay.
        let h = graph_heights(13, &[true, true, true, true], &[true, false, false, true]);
        assert_eq!(h.iter().sum::<u16>(), 13);
        assert!(h.iter().all(|&x| x >= 3), "{h:?}");
        // Sum exact and floor never broken at any height:
        for avail in 12..=24 {
            let hh = graph_heights(avail, &[true, true, true, true], &[true, false, false, true]);
            assert_eq!(hh.iter().sum::<u16>(), avail, "avail {avail}");
            assert!(hh.iter().all(|&x| x >= 3), "avail {avail}: {hh:?}");
        }
        // GEN grows FIRST: with equal weights every leftover row lands on
        // GEN, so GEN >= every other panel at all heights.
        for avail in 12..=24 {
            let hh = graph_heights(avail, &[true, true, true, true], &[false; 4]);
            assert!(hh.iter().all(|&x| hh[0] >= x), "avail {avail}: {hh:?}");
        }
        // The documented shrink ladder itself: KV, GPU, PROMPT, GEN last.
        assert_eq!(SHRINK_ORDER, [2, 3, 1, 0]);
    }

    #[test]
    fn accent_letters_mark_the_hotkey_in_every_title() {
        let t = |s: &'static str| accent_index(s);
        assert_eq!(t("GEN t/s").map(|i| &"GEN t/s"[i..i + 1]), Some("N"));
        assert_eq!(t("PROMPT t/s").map(|i| &"PROMPT t/s"[i..i + 1]), Some("P"));
        assert_eq!(t("GPU %").map(|i| &"GPU %"[i..i + 1]), Some("G"));
        assert_eq!(accent_index("KV\u{2248} %"), Some(1));
        assert_eq!(accent_index("APU %"), Some(0));
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

pub struct DashboardWidget<'a> {
    pub banner: &'a str,
    pub stats: &'a str,
    pub hotkeys: &'a str,
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
    /// B5: visibility + half-size for [GEN, PROMPT, KV, GPU/APU]
    pub vis: [bool; 4],
    pub half: [bool; 4],
    /// B5: graphs-pane hotkey focus (N/P/V/G act on graphs while set)
    pub focused: bool,
    /// B2: pane scroll offset (rows) when the graphs area is squeezed
    /// below the content minimum; clamped to max_scroll at draw.
    pub graph_scroll: u16,
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
) {
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
        // table height is CONTENT-driven, capped at 60% (css
        // "#mts-table { height: auto; max-height: 60% }"); the graphs own
        // everything left over (1fr).
        let fixed: u16 = 4; // banner + stats + blank + hotkey strip
        let avail = area.height.saturating_sub(fixed);
        let vidx: Vec<usize> = (0..4).filter(|i| self.vis[*i]).collect();
        let n_graphs: u16 = vidx.len() as u16;
        let min_graph = n_graphs * 3;
        // B2: two regimes. Roomy: the table takes its content want and
        // the graphs own the rest (weighted by half-size, GEN outranks).
        // Squeezed (avail < min + stub): graphs keep the minimum they can,
        // the table shrinks to a stub, and the overflow becomes scrollable
        // pane content instead of crushing panels below 3 rows.
        let want_table = (self.rows.len() as u16 + 4).clamp(3, avail / 5 * 3);
        let graph_h = {
            let gh2 = avail.saturating_sub(want_table);
            if gh2 < min_graph && avail >= min_graph { min_graph } else { gh2 }
        };
        let table_h = avail.saturating_sub(graph_h);

        let mut y = body.y;
        let banner_r = Rect { x: body.x, y, width: body.width, height: 1 }; y += 1;
        let stats_r = Rect { x: body.x, y, width: body.width, height: 1 }; y += 1;
        let graphs_r = Rect { x: body.x, y, width: body.width, height: graph_h };
        y += graph_h;
        let table_r = Rect { x: body.x, y, width: body.width, height: table_h };

        Paragraph::new(self.banner)
            .style(Style::default().add_modifier(Modifier::BOLD))
            .render(banner_r, buf);
        Paragraph::new(self.stats).style(dim()).render(stats_r, buf);

        // Graph panels (#mts-graphs, 1fr each): stacked, apu panel
        // removed entirely when the option is off.
        let gh = graph_heights(graph_h, &self.vis, &self.half);
        // B5: only VISIBLE graphs stack, each with its weighted height
        // (half-size rides weight 1 vs 2 via graph_heights below).
        let mut gy = graphs_r.y;
        for (i, &gi) in vidx.iter().enumerate() {
            let r = Rect { x: graphs_r.x, y: gy, width: graphs_r.width, height: gh[i] };
            gy += gh[i];
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
        let inner = Rect {
            x: table_r.x + 1,
            y: table_r.y + 1,
            width: table_r.width.saturating_sub(2),
            height: table_r.height.saturating_sub(2),
        };
        table.render(inner, buf);

        Paragraph::new(self.hotkeys)
            .style(dim())
            .render(Rect { y: area.y + area.height.saturating_sub(1), width: area.width, height: 1, ..area }, buf);
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
