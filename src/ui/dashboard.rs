//! v9.15-faithful metrics shell (app.py MetricsScreen): banner, stats line,
//! FOUR stacked history graphs (GEN leaky / PROMPT leaky / KV≈ fixed 100 /
//! GPU% fixed 100, backend-titled) each with an nvtop corner legend
//! (title dim + run|now / avg / max heat-tinted) AND the nvtop overlay
//! (quartile ticks on the left edge, ceiling top-right, live value cursor
//! dodging the trace), the rounded-box 8-column lane table, and the hotkey
//! strip. M6-era two-graph skeleton replaced 2026-11-04 (maintainer caught the
//! dashboard running BEHIND Python; shell parity IS the product). The
//! pty-vs-pty audit the same day caught the missing overlay/box/header —
//! parity is measured against the PYTHON screen, never our own snapshot.
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Widget},
};
use crate::engine::leaky::LeakyMax;
use crate::hist::Ring;
use crate::ui::graph_widget::{Graph, GraphOpts, GraphStats};

/// nvtop corner legend for one graph (app.py _legend_segs): dim title,
/// then name:value pairs, value colored by _heat_style(val / 100).
#[derive(Clone, Copy)]
pub struct Legend<'a> {
    pub title: &'a str,
    pub head: (&'a str, f64),
    pub avg: f64,
    pub max: f64,
}

fn heat(frac: f64) -> Color {
    // app.py _heat_style: green -> amber -> red on value/100.
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

/// avg over the LAST 60 samples, max over the whole ring (app.py _graph).
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

/// app.py HistoryGraph._fmt: k-suffix over 999.5, integer over 99.5,
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

fn draw_legend(buf: &mut Buffer, area: Rect, lg: &Legend) -> u16 {
    if area.height == 0 {
        return 0;
    }
    let mut x = area.x + 1;
    let right = area.x + area.width;
    put(buf, x, area.y, right, lg.title, dim());
    x += lg.title.chars().count() as u16 + 1;
    for (name, val) in [lg.head, ("avg", lg.avg), ("max", lg.max)] {
        // IEEE -0.0 prints "-0.0" on idle rails; Python sums never hit it.
        let val = if val == 0.0 { 0.0 } else { val };
        put(buf, x, area.y, right, name, dim());
        x += name.chars().count() as u16 + 1;
        let txt = format!("{val:5.1}");
        put(buf, x, area.y, right, &txt, Style::default().fg(heat(val / 100.0)));
        x += txt.chars().count() as u16 + 1;
    }
    x - area.x
}

/// // port: app.py _cellv_row_occupied — does the curve occupy `row`
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

/// // port: app.py HistoryGraph._overlay — nvtop wastes no columns: scale
/// quartiles sit into the LEFT edge, the ceiling top-RIGHT, and the live
/// value rides at the end of the line with a one-row dodge. Old-school
/// line style gets no overlay (python returns before reaching here).
fn draw_overlay(
    buf: &mut Buffer,
    area: Rect,
    st: &GraphStats,
    legend_w: u16,
    braille: bool,
    line_chars: bool,
) {
    if line_chars || area.width < 4 || area.height == 0 {
        return;
    }
    let (w, h) = (area.width, area.height);
    if h >= 4 {
        // left-edge quartile ticks + floor; the ceiling lives top-right
        let mut ticks: Vec<(u16, f64)> = Vec::new();
        for frac in [0.75_f64, 0.5, 0.25] {
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
        let h2 = f64::from(h) * 2.0; // python H2 = h*2 for every painter
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
                // to the number") — python's dodge could land there too
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

    #[test]
    fn value_cursor_never_paints_row0() {
        let area = Rect { x: 0, y: 0, width: 40, height: 6 };
        let mut buf = Buffer::empty(area);
        // 85% of ceiling: the curve leaves row 0 free — exactly the case
        // where the old dodge parked the value label on the ceiling text
        let st = GraphStats { vmax: 100.0, columns: vec![85.0; 30], last: 85.0, n: 30 };
        draw_overlay(&mut buf, area, &st, 20, true, false);
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
    /// LANES, ACT, PROMPT t/s, GEN t/s, KV≈%, CACHE, CTX, ACCEPT
    pub cells: [String; 8],
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
    /// gpu_graph option: off removes the panel entirely (app.py set_gpu).
    pub apu_on: bool,
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
    draw_overlay(buf, area, &st, lw, braille, line_chars);
}

impl Widget for DashboardWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // python margins: banner/graphs/table sit inside a 1-col side
        // margin; the footer strip is full width.
        let body = Rect {
            x: area.x + 1,
            width: area.width.saturating_sub(2).max(1),
            ..area
        };
        // table height is CONTENT-driven, capped at 60% (app.py
        // "#mts-table { height: auto; max-height: 60% }"); the graphs own
        // everything left over (1fr).
        let fixed: u16 = 4; // banner + stats + blank + hotkey strip
        let avail = area.height.saturating_sub(fixed);
        let n_graphs: u16 = if self.apu_on { 4 } else { 3 };
        let want_table = (self.rows.len() as u16 + 4).clamp(3, avail / 5 * 3);
        let mut graph_h = avail.saturating_sub(want_table);
        let min_graph = n_graphs * 3;
        if graph_h < min_graph && avail >= min_graph {
            graph_h = min_graph;
        }
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

        // Graph panels (app.py #mts-graphs, 1fr each): stacked, apu panel
        // removed entirely when the option is off.
        let gc = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Ratio(1, n_graphs as u32); n_graphs as usize])
            .split(graphs_r);

        draw_graph(buf, gc[0], self.gen, Some(self.gen_leaky), None, &self.opts, self.tick, &self.gen_legend);
        draw_graph(
            buf,
            gc[1],
            self.prompt,
            Some(self.prompt_leaky),
            None,
            &self.opts,
            self.tick,
            &self.prompt_legend,
        );
        draw_graph(buf, gc[2], self.kv, None, Some(100.0), &self.opts, self.tick, &self.kv_legend);
        if self.apu_on {
            draw_graph(buf, gc[3], self.apu, None, Some(100.0), &self.opts, self.tick, &self.apu_legend);
        }

        // Rounded-box table (app.py #mts-table { border: round }; columns
        // LANES ACT PROMPT t/s GEN t/s KV≈% CACHE CTX ACCEPT). LANES sizes
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
                Constraint::Length(8),
                Constraint::Length(6),
                Constraint::Length(6),
                Constraint::Length(5),
                Constraint::Length(9),
            ],
        )
        .header(
            Row::new(vec![
                "LANES", "ACT", "PROMPT t/s", "GEN t/s", "KV≈%", "CACHE", "CTX", "ACCEPT",
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

    // Golden values taken from app.py HistoryGraph._fmt's own rules —
    // the boundaries are the interesting part (99.5/999.5 round UP into
    // the next format tier, and 0 is bare "0" not "0.0").
    #[test]
    fn fmt_matches_python_tiers() {
        assert_eq!(fmt_v(0.0), "0");
        assert_eq!(fmt_v(-0.0), "0");
        assert_eq!(fmt_v(3.24), "3.2");
        assert_eq!(fmt_v(9.94), "9.9");
        assert_eq!(fmt_v(9.95), "9.9"); // 9.95 is 9.949999.. in binary — python agrees
        assert_eq!(fmt_v(75.0), "75.0");
        assert_eq!(fmt_v(99.4), "99.4");
        assert_eq!(fmt_v(99.5), "100");
        assert_eq!(fmt_v(100.0), "100");
        assert_eq!(fmt_v(999.4), "999");
        assert_eq!(fmt_v(999.5), "1.0k");
        assert_eq!(fmt_v(1234.0), "1.2k");
    }
}
