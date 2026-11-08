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
use crate::ui::graph_widget::{Graph, GraphOpts};

/// nvtop corner legend for one graph (): dim title,
/// then name:value pairs, value colored by _heat_style(val / 100).
/// One label part on a graph's label row (maintainer 2026-11-07: no `now` —
/// the value rides the right edge; the row carries only what cannot be
/// read elsewhere).
#[derive(Clone)]
pub enum Part<'a> {
    /// heat-colored plain number
    Num(&'a str, f64),
    /// `{v:.0}%`, steady white
    Pct(&'a str, f64),
    /// pre-formatted, steady white (clk MHz etc.)
    Raw(&'a str, String),
}

#[derive(Clone)]
pub struct Legend<'a> {
    pub title: &'a str,
    pub parts: Vec<Part<'a>>,
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
/// `show_table = false` (lanes mode off) hands every row to the graphs.
/// Roomy regime (ideal >= min): table gets its content want, graphs own
/// the rest, pane content == window (no scroll). Squeezed regime: the
/// table drops toward a stub so graphs hold their 3-row minimum; below
/// that the graphs window is smaller than the min content and the pane
/// becomes wheel-scrollable instead of crushing panels. Invariants:
/// table_h + window_h == avail, and window_h >= min iff avail >= min.
pub fn graph_layout(avail: u16, rows: u16, min_graph: u16, show_table: bool) -> (u16, u16, u16) {
    // lanes table OFF (L cycle / chip): the pane is gone, graphs own it all
    if !show_table {
        return (avail, 0, avail);
    }
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
    // tight spacing (maintainer): exactly one space everywhere, no padding
    x += 1;
    for part in &lg.parts {
        let (label, text, val) = match part {
            Part::Num(n, v) => (*n, format!("{v:.1}"), *v),
            Part::Pct(n, v) => (*n, format!("{v:.0}%"), *v),
            Part::Raw(n, t) => (*n, t.clone(), 0.0),
        };
        put(buf, x, area.y, right, label, dim());
        x += label.chars().count() as u16 + 1;
        let st = match part {
            Part::Num(..) => Style::default().fg(heat(val / 100.0)),
            _ => Style::default().fg(Color::White),
        };
        put(buf, x, area.y, right, &text, st);
        x += text.chars().count() as u16 + 1;
    }
    x - area.x
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
fn scale_ticks(h: u16, _fixed_scale: bool) -> Vec<f64> {
    // maintainer 2026-11-07 v3: EVEN spacing above all — choose the interval
    // count whose fractions rasterize to uniform rows: quartiles first,
    // then fifths (the 0-100 graphs absorb those as clean whole percents
    // 20/40/60/80), then thirds, then the lone half. Uneven sets banned.
    let span = h.saturating_sub(1);
    if span < 2 {
        return Vec::new(); // only the floor "0" fits below two rows
    }
    for k in [4u16, 5, 3, 2] {
        let fr: Vec<f64> = (1..k).map(|i| 1.0 - f64::from(i) / f64::from(k)).collect();
        if uniform_rows(h, &fr).is_some() {
            return fr;
        }
    }
    if span >= 2 {
        vec![0.5]
    } else {
        Vec::new()
    }
}

/// do the fractions rasterize to uniformly spaced rows (floor included)?
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

/// left-edge label text for a fraction of the scale: k-figures for the
/// legend number, THREE chars maximum (maintainer): thousands compress to k
/// and get a tilde when rounding takes over (2500 -> ~2k); the 0-100
/// ratio graphs speak quartile glyphs or whole percents.
fn fmt3(v: f64) -> String {
    let a = v.abs();
    if a >= 999.5 {
        let ks = a / 1000.0;
        if ks >= 9.5 {
            format!("{ks:.0}k")
        } else {
            format!("~{}k", ks.floor().max(1.0))
        }
    } else if a >= 9.5 {
        format!("{a:.0}")
    } else if a == 0.0 {
        "0".to_string()
    } else {
        format!("{a:.1}")
    }
}

fn scale_label(vmax: f64, f: f64, fixed_pct: bool) -> String {
    if fixed_pct {
        if (f - 0.75).abs() < 1e-9 {
            return "¾".to_string();
        }
        if (f - 0.5).abs() < 1e-9 {
            return "½".to_string();
        }
        if (f - 0.25).abs() < 1e-9 {
            return "¼".to_string();
        }
        return format!("{:.0}", 100.0 * f);
    }
    fmt3(vmax * f)
}


#[cfg(test)]
mod overlay_tests {
    //! maintainer: a value near the ceiling used to paint its digits on top of
    //! the legend/ceiling row ("add a number to the number"). Row 0 is off
    //! limits for the value cursor.
    use super::*;

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
        assert_eq!(graph_layout(30, 8, 12, true), (19, 11, 19)); // R6: border+header+rows exactly
        // squeezed: table falls toward a stub, graphs hold the minimum
        assert_eq!(graph_layout(14, 8, 12, true), (12, 2, 12));
        // tiny: window < content -> the pane is wheel-scrollable by 2
        assert_eq!(graph_layout(10, 8, 12, true), (12, 0, 10));
        assert_eq!(graph_layout(30, 8, 12, false), (30, 0, 30)); // lanes off
        // invariants across the range: exact split, window >= min when avail >= min
        for avail in 0..40u16 {
            let (c, t, g) = graph_layout(avail, 8, 12, true);
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
        assert_eq!(t("KV≈ %").map(|i| &"KV≈ %"[i..i + 1]), Some("V"));
        assert_eq!(t("APU %").map(|i| &"APU %"[i..i + 1]), Some("U"));
        assert_eq!(accent_index("other"), None);
    }

    #[test]
    fn strip_layout_columns_match_the_painted_letters() {
        // G2 drift trap: the widget paints each WORD with the hotkey
        // letter at token.hot; the hit map uses strip_layout columns.
        // One law — the recorded hot column MUST be the hot letter.
        let toks = metrics_strip();
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
        // btop style: NO separate letter prefixes — the strip is words,
        // and the words are EXACTLY `Journal Lanes Settings Quit` in order
        // (nothing else may creep back onto the main chrome)
        let want: Vec<(&str, char)> =
            vec![("Journal", 'j'), ("Lanes", 'L'), ("Settings", 's'), ("Quit", 'q')];
        let got: Vec<(&str, char)> = toks.iter().map(|t| (t.label, t.key)).collect();
        assert_eq!(got, want, "strip drifted");
        assert!(text.contains("Journal") && text.contains("Lanes"));
        assert!(text.contains("Settings") && text.contains("Quit"));
        for gone in ["All/Active", "HF-CLI", "Llama", "Quit+Stop", "GEN", "KV", "GPU", "PROMPT"] {
            assert!(!text.contains(gone), "strip must not carry {gone}");
        }
    }

    #[test]
    fn scale_pos_parse_cols_and_live_row() {
        assert_eq!(ScalePos::parse("left"), ScalePos::Left);
        assert_eq!(ScalePos::parse("off"), ScalePos::Off);
        assert_eq!(ScalePos::parse("both"), ScalePos::Both);
        assert_eq!(ScalePos::parse(""), ScalePos::Right); // default law
        assert_eq!(ScalePos::Both.cols(80), (3, 3));
        assert_eq!(ScalePos::Both.cols(13), (0, 3)); // 13-6=7 < MIN_PLOT_W: RIGHT survives
        assert_eq!(ScalePos::Both.cols(12), (0, 3)); // 12-3=9 still fits the plot
        assert_eq!(ScalePos::Both.cols(10), (0, 0)); // 10-3=7 < 8: ruler loses to the graph
        assert_eq!(ScalePos::Right.cols(11), (0, 3));
        assert_eq!(ScalePos::Right.cols(10), (0, 0));
        assert_eq!(ScalePos::Left.cols(80), (3, 0));
        assert_eq!(ScalePos::Off.cols(80), (0, 0));
        assert_eq!(ScalePos::live_row(1.0, 9), 0);
        assert_eq!(ScalePos::live_row(0.0, 9), 8);
        assert_eq!(ScalePos::live_row(0.5, 9), 4);
        assert_eq!(ScalePos::live_row(2.0, 9), 0); // clamped
    }

    #[test]
    fn scale_is_even_always() {
        // maintainer 2026-11-07 v3: EVEN spacing above all. Quartiles where
        // they rasterize evenly (span % 4 == 0), fifths next (the 0-100
        // graphs absorb those as whole percents), thirds, then half.
        let approx = |a: &[f64], b: &[f64]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3)
        };
        assert!(approx(&scale_ticks(5, false), &[0.75, 0.5, 0.25])); // span 4
        assert!(approx(&scale_ticks(9, false), &[0.75, 0.5, 0.25])); // span 8
        assert!(approx(&scale_ticks(6, false), &[0.8, 0.6, 0.4, 0.2])); // span 5
        assert!(approx(&scale_ticks(7, false), &[2.0 / 3.0, 1.0 / 3.0])); // span 6
        assert!(approx(&scale_ticks(3, false), &[0.5]));
        assert!(scale_ticks(2, false).is_empty());
        // THE law: whatever height, the chosen set rasterizes uniform
        for h in 3..24u16 {
            let fr = scale_ticks(h, false);
            if fr.len() > 1 {
                assert!(uniform_rows(h, &fr).is_some(), "h={h} got uneven set {fr:?}");
            }
        }
        // three-char labels (maintainer): glyph quartiles / whole percents /
        // k-compressed approximate values — never 25.0, never 2500
        assert_eq!(scale_label(100.0, 0.75, true), "\u{00be}");
        assert_eq!(scale_label(100.0, 0.8, true), "80");
        assert_eq!(scale_label(5000.0, 0.5, false), "~2k"); // maintainer's example
        assert_eq!(scale_label(46.8, 0.5, false), "23");
        assert_eq!(scale_label(46000.0, 0.5, false), "23k");
        for h in [5.0f64, 7.0, 9.0, 13.0] {
            for f in scale_ticks(h as u16, false) {
                for fixed in [true, false] {
                    assert!(
                        scale_label(9_999.0, f, fixed).chars().count() <= 3,
                        "label >3 chars: {} f={f}",
                        scale_label(9_999.0, f, fixed)
                    );
                }
            }
        }
    }

    #[test]
    fn value_pins_above_the_trace_or_takes_the_label_row() {
        // the glance law: one column (the right edge), y rides just above
        // the latest sample; a top-out pushes the number into the label
        // row (area.y), replacing the ceiling — computed by draw_graph via
        // this exact formula (kept here so the rule is TESTED, not vibes)
        let h: u16 = 6;
        let (data_y, label_y) = (11u16, 10u16);
        let row_of = |ln: f64| -> (u16, bool) {
            let ly = ((1.0 - ln) * f64::from(h - 1)).round() as u16;
            let y = if ly <= 1 { label_y } else { data_y + ly - 1 };
            (y, y == label_y)
        };
        assert_eq!(row_of(1.0), (label_y, true), "saturated -> label row");
        assert_eq!(row_of(0.9), (label_y, true));
        assert_eq!(row_of(0.5), (data_y + 2, false), "mid trace: above it");
        assert_eq!(row_of(0.05), (data_y + 4, false));
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

/// Journal sizing law (tiers: four / ten) with scaling-priority: FIXED
/// display-row counts (wrap-aware — `render_tail` folds long entries and
/// the count covers the wrapped result), so a new log line NEVER changes
/// the pane height and the dashboard flow never jumps. The pane truncates
/// to whatever fits and hides below two honest rows. The journal flexes
/// and hides BEFORE graphs lose their minimums. The pane hides below a
/// 24-row screen (small-window law).
pub fn journal_pane_h(screen_h: u16, mode: &str, graphs_min: u16) -> u16 {
    if screen_h < 24 || mode == "off" {
        return 0;
    }
    // 2 chrome rows (stats/hotkeys) + 3 table stub must survive
    let cap = screen_h.saturating_sub(5 + graphs_min);
    let (want, min_ok) = match mode {
        "four" => (4u16, 2u16),
        "ten" => (10u16, 2),
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

/// G2 law, maintainer pass 2026-11-07: the main strip carries exactly the page
/// words `Journal  Lanes  Settings  Quit` — graph letters, HF-CLI and the
/// Llama page are off the strip (undocumented keys are cataloged in
/// help.md, not painted on the chrome). The letter still rides INSIDE the
/// word; the whole word is the mouse target; clicking it IS pressing it.
pub fn metrics_strip() -> Vec<StripTok> {
    vec![
        StripTok { label: "Journal", hot: 0, key: 'j' },
        StripTok { label: "Lanes", hot: 0, key: 'L' },
        StripTok { label: "Settings", hot: 0, key: 's' },
        StripTok { label: "Quit", hot: 0, key: 'q' },
    ]
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
    /// scrape interval ms — forwarded to every panel (squish quantum law)
    pub tick_ms: u64,
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
    /// lanes table visibility (metrics.lanes != off); false = no table drawn
    pub show_lanes: bool,
    /// B4: journal lower pane (None = hidden). Lines come from the pane's
    /// own LogState; scroll is independent of the full-screen journal.
    pub journal: Option<JournalPane<'a>>,
    /// pane height already computed against the live screen (0 hides)
    pub journal_h: u16,
    pub rows: Vec<DashRow>,
    /// where the scale ruler lives (metrics.scale)
    pub scale: ScalePos,
}

/// where the three-column scale ruler lives (metrics.scale, maintainer
/// 2026-11-07). RIGHT is the house default; the ruler and the live value
/// that rides it are one object — `off` removes both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalePos {
    Right,
    Left,
    Both,
    Off,
}

impl ScalePos {
    pub fn parse(s: &str) -> Self {
        match s {
            "left" => Self::Left,
            "both" => Self::Both,
            "off" => Self::Off,
            _ => Self::Right,
        }
    }
    /// (left, right) gutter columns; each gutter is 3 wide and refuses to
    /// squeeze the plot below MIN_PLOT_W — a legible graph outranks its
    /// ruler. When only one side fits, the DEFAULT (right) survives first.
    pub fn cols(self, pane_w: u16) -> (u16, u16) {
        const G: u16 = 3;
        const MIN_PLOT_W: u16 = 8;
        let (l, r) = match self {
            Self::Left => (G, 0),
            Self::Right => (0, G),
            Self::Both => (G, G),
            Self::Off => (0, 0),
        };
        if pane_w.saturating_sub(l + r) < MIN_PLOT_W {
            if r > 0 && pane_w.saturating_sub(r) >= MIN_PLOT_W {
                (0, r)
            } else if l > 0 && pane_w.saturating_sub(l) >= MIN_PLOT_W {
                (l, 0)
            } else {
                (0, 0)
            }
        } else {
            (l, r)
        }
    }
    /// row of the live value inside the gutter: fraction 1.0 sits on the
    /// TOP data row, 0.0 on the bottom (mirrors the tick row math)
    pub fn live_row(f: f64, height: u16) -> u16 {
        let h = height.max(1);
        ((1.0 - f.clamp(0.0, 1.0)) * f64::from(h - 1)).round() as u16
    }
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
    tick_ms: u64,
    legend: &Legend,
    single: bool,
    scale: ScalePos,
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
                tick_ms,
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
    // dedicated label row (maintainer 2026-11-07): NAME ... rule ... ceiling.
    // The graph body keeps the rest of the box; data can never cover the
    // numbers because it now lives strictly BELOW the label row.
    let data = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    let (lc, rc) = scale.cols(area.width);
    // ink discipline (maintainer 2026-11-07): the plot never reaches a gutter
    // column — the ruler owns its three columns outright.
    let plot = Rect {
        x: area.x + lc,
        width: area.width.saturating_sub(lc + rc).max(1),
        ..data
    };
    let st = Graph {
        hist,
        opts: opts.clone(),
        tick,
        tick_ms,
        leaky,
        fixed_max,
    }
    .render_stats(plot, buf);
    let lw = draw_legend(buf, area, legend);

    // GUTTERS (maintainer 2026-11-07 v4): the static three-column ruler of v3,
    // now POSITIONABLE via metrics.scale (right default). The border rule
    // lives on the gutter column ADJACENT to the plot (left ruler: area.x+2,
    // right ruler: area.x+w-3); tick digits align to it and BREAK it at
    // three chars (no fourth column is spent on the line). Even vertical
    // spacing comes from scale_ticks. Data never enters a gutter: the plot
    // rect fed render_stats above is inset by exactly these columns.
    let fr = scale_ticks(data.height, fixed_max.is_some());
    let mut ticks: Vec<(u16, String)> = Vec::new();
    let mut seen_rows: Vec<u16> = Vec::new();
    for f in fr {
        let r =
            data.y + ((1.0 - f) * f64::from(data.height.saturating_sub(1))).round() as u16;
        if seen_rows.contains(&r) {
            continue; // dedupe: one label per row, never collide
        }
        seen_rows.push(r);
        ticks.push((r, scale_label(st.vmax, f, fixed_max.is_some())));
    }

    // live value (maintainer 2026-11-07 v4): RETIRED from riding the trace — it
    // now lives IN the ruler and moves up and down the three-column space
    // with the current value, formatted by the very scale_label the ticks
    // use (same three-char law), coloured by heat like the old cursor.
    // It draws LAST, so it wins its row over any tick digit outright;
    // `both` shows it on both rulers; `off` removes ruler and value alike.
    let live = if st.last > 0.0 {
        let fl = (st.last / st.vmax.max(f64::EPSILON)).clamp(0.0, 1.0);
        Some((
            data.y + ScalePos::live_row(fl, data.height),
            scale_label(st.vmax, fl, fixed_max.is_some()),
        ))
    } else {
        None
    };
    for side_left in [true, false] {
        let active = (side_left && lc > 0) || (!side_left && rc > 0);
        if !active {
            continue;
        }
        let bx = if side_left {
            area.x + 2
        } else {
            area.x + area.width.saturating_sub(3)
        };
        if area.width >= 4 {
            // vertical border down the ruler column (row 0 belongs to the title)
            for y in (area.y + 1)..data.bottom() {
                if let Some(c) = buf.cell_mut((bx, y)) {
                    c.set_char('\u{2502}').set_style(dim());
                }
            }
        }
        // digits align to the border and break it at three chars; the
        // right ruler mirrors (digits run OUTWARD from the border)
        for (r, t) in &ticks {
            let w = t.chars().count() as u16;
            if side_left {
                put(buf, bx + 1 - w, *r, bx + 1, t, dim());
            } else {
                put(buf, bx, *r, bx + w, t, dim());
            }
        }
        if side_left {
            put(buf, bx, data.bottom().saturating_sub(1), bx + 1, "0", dim());
        } else {
            put(buf, bx, data.bottom().saturating_sub(1), bx + 2, "0", dim());
        }
        if let Some((y, t)) = &live {
            let w = t.chars().count() as u16;
            let ln = st.columns.last().copied().unwrap_or(0.0);
            let style = Style::default().fg(heat(ln));
            if side_left {
                put(buf, bx + 1 - w, *y, bx + 1, t, style);
            } else {
                put(buf, bx, *y, bx + w, t, style);
            }
        }
    }

    // the label-row ceiling number stays (row 0, above the gutter region);
    // nothing can top out into it any more — the live value is the ruler's
    // job now, so it is permanently dim furniture.
    let ct = fmt_v(st.vmax);
    let cw = ct.chars().count() as u16;
    let mut rule_x1 = area.x;
    if area.width >= cw + 4 {
        put(
            buf,
            area.x + area.width - cw - 1,
            area.y,
            area.x + area.width - 1,
            &ct,
            dim(),
        );
        rule_x1 = area.x + area.width - cw - 2;
    }

    // continuous rule from the legend toward the ceiling number
    let x0 = area.x + lw + 2;
    if rule_x1 > x0 {
        put(
            buf,
            x0,
            area.y,
            rule_x1,
            &"\u{2500}".repeat((rule_x1 - x0) as usize),
            dim(),
        );
    }
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
        let (content_h, table_h, graph_h) =
            graph_layout(avail, self.rows.len() as u16, min_graph, self.show_lanes);
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
                    self.tick_ms,
                    &self.gen_legend,
                    single,
                    self.scale,
                ),
                1 => draw_graph(
                    buf,
                    r,
                    self.prompt,
                    Some(self.prompt_leaky),
                    None,
                    &self.opts,
                    self.tick,
                    self.tick_ms,
                    &self.prompt_legend,
                    single,
                    self.scale,
                ),
                2 => draw_graph(
                    buf,
                    r,
                    self.kv,
                    None,
                    Some(100.0),
                    &self.opts,
                    self.tick,
                    self.tick_ms,
                    &self.kv_legend,
                    single,
                    self.scale,
                ),
                _ => draw_graph(
                    buf,
                    r,
                    self.apu,
                    None,
                    Some(100.0),
                    &self.opts,
                    self.tick,
                    self.tick_ms,
                    &self.apu_legend,
                    single,
                    self.scale,
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
        if self.show_lanes && table_h > 0 {
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .render(table_r, buf);
        }
        // Journal pane: bare wrapped rows (the banner line was cut —
        // every pane row shows log now; the strip word marks the pane).
        // Fixed-height tail: the four/ten tiers show exactly journal_h
        // wrapped display rows (R1 soft-wrap, styles ride the fold).
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
        if self.show_lanes && table_h > 0 {
            let inner = Rect {
                x: table_r.x + 1,
                y: table_r.y + 1,
                width: table_r.width.saturating_sub(2),
                height: table_r.height.saturating_sub(2),
            };
            table.render(inner, buf);
        }

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
