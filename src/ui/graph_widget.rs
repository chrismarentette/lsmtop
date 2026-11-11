use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::Widget,
    style::{Color, Style},
};
use crate::hist::{seam_column, Ring, squish_frame, to_columns, Norm};



use crate::engine::leaky::{LeakyMax, flow_phase};
use crate::paint::modes::{Mode as ColorMode, column_color};
use crate::paint::ramps::Ramp;
use crate::paint::glyphs::{Chars, Fill};
use crate::paint::stroke::{self, FillMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphOpts { 
    pub chars: Chars,
    pub colors: ColorMode,
    pub ramp: Ramp,
    /// timebase: false = scroll (1:1 newest samples, DEFAULT),
    /// true = squish (stepped third-by-third warp). The option existed
    /// in the chips but was never wired — every graph squished (maintainer:
    /// "graph views and options don't seem to be reliable").
    pub squish: bool,
}

pub struct Graph<'a> {
    pub hist: &'a Ring,
    pub opts: GraphOpts,
    pub tick: u64,
    /// scrape interval in ms (metrics.tick_ms; 0 = 2000 reference) — the
    /// squish layout quantum is WALL time, so it scales with this.
    pub tick_ms: u64,
    pub leaky: Option<&'a mut LeakyMax>,
    pub fixed_max: Option<f64>,
    /// lane-focus seam (experiment): samples back from newest where the
    /// curve switched lanes; a dim vertical rides that column over rail
    /// cells only — it never cuts ink, and the ruler (painted later) wins.
    pub seam_back: Option<usize>,
}

/// What the painter decided, so the overlay (nvtop ticks + value cursor,
/// ) can ride the SAME normalized cells the curve used.
pub struct GraphStats {
    pub vmax: f64,
    /// per-column normalized heights, 0..1
    pub columns: Vec<f64>,
    /// newest raw sample (value-cursor target)
    pub last: f64,
    pub n: usize,
}

impl<'a> Graph<'a> {
    pub fn render_stats(mut self, area: Rect, buf: &mut Buffer) -> GraphStats {
        if area.width == 0 || area.height == 0 {
            return GraphStats {
                vmax: 1.0,
                columns: Vec::new(),
                last: 0.0,
                n: 0,
            };
        }

        let w = area.width;
        let h = area.height;

        let raw = self.hist.values();
        let last = raw.last().copied().unwrap_or(0.0);
        let n = raw.len();
        let squished = if self.opts.squish {
            squish_frame(&raw, w as usize, self.tick_ms)
        } else {
            // scroll: last w samples 1:1, left-padded
            let take = (w as usize).min(raw.len());
            let mut out = vec![0.0; w as usize - take];
            out.extend_from_slice(&raw[raw.len() - take..]);
            out
        };

        let (columns, vmax) = if let Some(max) = self.fixed_max {
            to_columns(&squished, w, h, Norm::Fixed(max))
        } else if let Some(leaky) = self.leaky.as_deref_mut() {
            to_columns(&squished, w, h, Norm::Leaky(leaky, 60))
        } else {
            to_columns(&squished, w, h, Norm::Fixed(1.0))
        };
        let stats = GraphStats {
            vmax,
            columns: columns.clone(),
            last,
            n,
        };

        match self.opts.chars {
            Chars::Braille(fill) => {
                // R4: the braille painter, law-for-law. Line = a
                // connected one-dot stroke (segment math); bar = solid
                // dot columns per SAMPLE; area = fill up to the stroke
                // EDGE. Idle columns ride the ⣀ rail in dim colour;
                // empty non-idle cells carry the DEFAULT style (never a
                // coloured space). The old hack ORed whole dot rows over
                // the stroke using a column-rounded top — that produced
                // the notched "area fill" that never matched the edge.
                let mode = match fill {
                    None => FillMode::Line,
                    Some(Fill::Bar) => FillMode::Bar,
                    Some(Fill::Area) => FillMode::Area,
                };
                let (wi, hi) = (w as usize, h as usize);
                let hd = (hi * 4) as f64;
                let dh = stroke::heights(&columns, wi, hi * 4);
                for y in 0..hi {
                    let cells = stroke::braille_row(&columns, &dh, mode, wi, hi, y);
                    let base = ((hi - 1 - y) * 4) as f64;
                    let screen_y = area.y + y as u16;
                    for (x, c) in cells.iter().enumerate() {
                        let val = columns[x];
                        let (ch, style) = if c.pat == 0 {
                            if base == 0.0 && c.col_h < 0.5 {
                                (stroke::RAIL_DOT, Self::rail())
                            } else {
                                (' ', Style::default())
                            }
                        } else if c.col_h < 0.5 && base == 0.0 {
                            (stroke::dot_glyph(c.pat), Self::rail())
                        } else {
                            let gfrac = if mode == FillMode::Line {
                                (c.top + 1) as f64 / hd
                            } else {
                                (base + 4.0) / hd
                            };
                            let color = self.cell_color(gfrac, c.col_h / hd, val);
                            (stroke::dot_glyph(c.pat), Style::default().fg(color))
                        };
                        if let Some(cell) = buf.cell_mut((area.x + x as u16, screen_y)) {
                            cell.set_char(ch).set_style(style);
                        }
                    }
                }
            }
            Chars::Block(fill) => {
                let mode = match fill {
                    None => FillMode::Line,
                    Some(Fill::Bar) => FillMode::Bar,
                    Some(Fill::Area) => FillMode::Area,
                };
                if mode == FillMode::Line {
                    // BLOCK stroke = 2x2 QUADRANT pixel art (restored
                    // law — two detours failed: lone eighth markers left
                    // gaps (maintainer's screenshot 2026-11-07 night: "the
                    // lines in block are not connected"), and the
                    // eighth-band ribbon was fat, not braille-pretty).
                    // The quadrant walk uses the SAME segment math as
                    // the braille stroke: every sub-cell the polyline
                    // crosses gets ink (▗▄▟▝▖▌▛▙...), so the line is
                    // thin and gapless at any slope — the look that was
                    // correct before the eighth-marker rewrite.
                    self.paint_quad(area, buf, &columns);
                } else {
                    // bar/area: eighth-cell fill bottom-up, 1-2-1
                    // smoothing for area, quantized AFTER subtracting
                    // each cell's baseline (quantizing a column-level
                    // total is what made waterlines read one eighth off).
                    let (wi, hi) = (w as usize, h as usize);
                    let levels = (hi * 8) as f64;
                    let ls = stroke::block_levels(&columns, wi, hi, mode == FillMode::Area);
                    for y in 0..hi {
                        let cells = stroke::block_row(&ls, wi, hi, y);
                        let b = hi - y;
                        let base = ((b - 1) * 8) as f64;
                        let screen_y = area.y + y as u16;
                        for (x, c) in cells.iter().enumerate() {
                            let val = columns[x];
                            let f = val.clamp(0.0, 1.0);
                            let (ch, style) = if c.pat == 0 {
                                if b == 1 {
                                    let st = if c.col_h < 0.5 {
                                        Self::rail()
                                    } else {
                                        let g = (1.0 / levels).min(1.0);
                                        Style::default().fg(self.cell_color(g, f, val))
                                    };
                                    (stroke::bar_glyph(1), st)
                                } else {
                                    (' ', Style::default())
                                }
                            } else {
                                let fill_h = (c.col_h - base).clamp(0.0, 8.0);
                                let g = ((base + fill_h) / levels).min(1.0);
                                let color = self.cell_color(g, f, val);
                                (stroke::bar_glyph(c.pat), Style::default().fg(color))
                            };
                            if let Some(cell) = buf.cell_mut((area.x + x as u16, screen_y)) {
                                cell.set_char(ch).set_style(style);
                            }
                        }
                    }
                }
            }
            Chars::Line(fill) => {
                // nvtop SINGLE-LINE law (maintainer 2026-11-07): SIX glyphs —
                // flat, vertical, four corner arcs — and stair-steps.
                // NO diagonals; corners hand off at mid-edges, diagonal
                // ends do not. Half-cell (2H) quantization.
                // chars=line CAN underfill (maintainer): the underfill is
                // built from the same line alphabet — bar = vertical
                // U+2502 runs from the floor up to the trace, area =
                // horizontal U+2500 rules stacked under it — so every
                // fill cell carries the ramp gradient by its row height
                // (a vertical colour ramp through the fill).
                match fill {
                    Some(Fill::Bar) => self.paint_line_fill(area, buf, &columns, true),
                    Some(Fill::Area) => self.paint_line_fill(area, buf, &columns, false),
                    _ => {}
                }
                self.paint_glyph_line(area, buf, &columns);
            }
        }
        self.paint_seam(area, buf, n, w);
        stats
    }

    /// Lane-switch seam: one dim vertical column at the warp-mapped
    /// position, riding ONLY rail/blank cells (rail law: ink untouched).
    fn paint_seam(&self, area: Rect, buf: &mut Buffer, n: usize, w: u16) {
        let Some(back) = self.seam_back else { return };
        // The seam is the BOUNDARY: it rides the column just left of the
        // new stint's first sample. It crosses ink (maintainer 2026-11-10: the
        // seam exists precisely where the old lane was mid-flow — hiding
        // behind that ink hid the break itself). Rulers and borders win:
        // only the plot area ever reaches here.
        let Some(col) = seam_column(n, back + 1, w as usize, self.tick_ms) else {
            return;
        };
        let x = area.x + col as u16;
        for y in 0..area.height {
            let cell = match buf.cell_mut((x, area.y + y)) {
                Some(c) => c,
                None => continue,
            };
            cell.set_symbol("│");
            cell.set_style(Style::default().fg(Color::DarkGray));
        }
    }

    /// Underfill for chars=line, drawn BEFORE the polyline so the trace
    /// overwrites the shared row. bar: every column drops a vertical
    /// rule from just under the trace to the floor; area: the same
    /// region with horizontal rules. Colour follows the row's gradient
    /// fraction, so the fill deepens toward the top exactly like the
    /// braille/block underfills do. Idle columns stay pure rail.
    fn paint_line_fill(&self, area: Rect, buf: &mut Buffer, columns: &[f64], vertical: bool) {
        let (wi, hi) = (area.width as usize, area.height as usize);
        if wi == 0 || hi < 2 {
            return;
        }
        let two_h = (hi * 2) as f64;
        let idle_floor = 0.5 / two_h;
        let glyph = if vertical { '\u{2502}' } else { '\u{2500}' };
        for (x, &v) in columns.iter().enumerate().take(wi) {
            let x = x as u16;
            if v <= idle_floor {
                continue;
            }
            let hy = (v.clamp(0.0, 1.0) * (two_h - 1.0)).round() as usize;
            let line_r = hi - 1 - hy / 2;
            for r in (line_r + 1)..hi {
                let gfrac = ((2.0 * (hi - r) as f64) - 1.0) / two_h;
                let style = Style::default().fg(self.cell_color(gfrac, gfrac, v));
                if let Some(cell) = buf.cell_mut((area.x + x, area.y + r as u16)) {
                    cell.set_char(glyph).set_style(style);
                }
            }
        }
    }

    /// Six-glyph stair-stair polyline: horizontal U+2500, vertical U+2502,
    /// corner arcs U+256D/U+256E/U+256F/U+2570 and nothing else (nvtop
    /// law). Every arm attaches at a cell MID-EDGE, so consecutive cells
    /// share the seam exactly — continuous at any slope, provable by
    /// construction: a rise draws LEFT-in foot (up+left arm), a vertical
    /// run, then the cap whose arms point DOWN (into its own run) and
    /// RIGHT (into the next column at the new row); falls mirror it. A
    /// one-row step stacks foot+cap in the same column: the tight hop
    /// nvtop graphs show.
    fn paint_glyph_line(&self, area: Rect, buf: &mut Buffer, columns: &[f64]) {
        let (wi, hi) = (area.width as usize, area.height as usize);
        if wi == 0 || hi == 0 {
            return;
        }
        let two_h = (hi * 2) as f64;
        let hy: Vec<usize> = columns
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0) * (two_h - 1.0)).round() as usize)
            .collect();
        let idle_floor = 0.5 / two_h;
        for x in 0..wi {
            let y0 = hy[x];
            let y1 = hy.get(x + 1).copied().unwrap_or(y0);
            let r0 = hi - 1 - y0 / 2;
            let r1 = hi - 1 - y1 / 2;
            let val = columns[x];
            let rail = y0 == 0 && y1 == 0 && val <= idle_floor;
            let cells: Vec<(usize, char)> = if r0 == r1 {
                vec![(r0, '\u{2500}')]
            } else if r1 < r0 {
                // rising: foot = LEFT+UP arm, cap = DOWN+RIGHT arm so it
                // connects DOWN into its own run and RIGHT onward
                let mut v = vec![(r0, '\u{256F}')];
                for r in (r1 + 1)..r0 {
                    v.push((r, '\u{2502}'));
                }
                v.push((r1, '\u{256D}'));
                v
            } else {
                let mut v = vec![(r0, '\u{256E}')];
                for r in (r0 + 1)..r1 {
                    v.push((r, '\u{2502}'));
                }
                v.push((r1, '\u{2570}'));
                v
            };
            for (r, ch) in cells {
                let gfrac = ((2.0 * (hi - r) as f64) - 1.0) / two_h;
                let style = if rail {
                    Self::rail()
                } else {
                    Style::default().fg(self.cell_color(gfrac, val.clamp(0.0, 1.0), val))
                };
                if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + r as u16)) {
                    cell.set_char(ch).set_style(style);
                }
            }
        }
    }

    fn rail() -> Style {
        Style::default().fg(Color::DarkGray) // bright_black, btop's rail
    }

    /// One colour call for every painter: `gfrac` is the gradient/flow
    /// height fraction for THIS cell, `solid_frac` the whole-column
    /// height fraction (solid colours the full interpolated gamut per
    /// column), `val` the column's normalized value (flow keys off the
    /// VALUE against the view max, not the row).
    /// Single-line colour law: HORIZONTAL (the row has no height axis).
    /// gradient ramps along the graph, hottest at the right edge; flow is
    /// a wash moving RIGHT to LEFT as time passes (newest stays hottest,
    /// ripples trail leftward); solid colours by the column's own value.
    fn spark_color(&self, x: usize, w: usize, f: f64) -> Color {
        match self.opts.colors {
            ColorMode::Gradient => {
                let xf = if w > 1 { (x + 1) as f64 / w as f64 } else { 1.0 };
                column_color(ColorMode::Gradient, self.opts.ramp, xf, 0.0, 1.0)
            }
            ColorMode::Solid => column_color(ColorMode::Solid, self.opts.ramp, f, 0.0, 1.0),
            ColorMode::Flow => {
                let phase = flow_phase(self.tick, 1.0);
                let wash = spark_flow_wash(x, w, phase);
                column_color(ColorMode::Flow, self.opts.ramp, wash, phase, 1.0)
            }
        }
    }

    fn cell_color(&self, gfrac: f64, solid_frac: f64, val: f64) -> Color {
        match self.opts.colors {
            ColorMode::Gradient => column_color(ColorMode::Gradient, self.opts.ramp, gfrac, 0.0, 1.0),
            ColorMode::Solid => column_color(ColorMode::Solid, self.opts.ramp, solid_frac, 0.0, 1.0),
            ColorMode::Flow => {
                let phase = flow_phase(self.tick, val);
                column_color(ColorMode::Flow, self.opts.ramp, val, phase, 1.0)
            }
        }
    }

    fn paint_quad(&self, area: Rect, buf: &mut Buffer, columns: &[f64]) {
        let (wi, hi) = (area.width as usize, area.height as usize);
        let h2f = (hi * 2) as f64;
        let dh = stroke::heights(columns, wi, hi * 2);
        for y in 0..hi {
            let cells = stroke::quad_row(&dh, wi, hi, y);
            let base = ((hi - 1 - y) * 2) as f64;
            let screen_y = area.y + y as u16;
            for (x, c) in cells.iter().enumerate() {
                let val = columns[x];
                let (ch, style) = if c.pat == 0 {
                    if base == 0.0 && c.col_h < 0.5 {
                        (stroke::RAIL_QUAD, Self::rail())
                    } else {
                        (' ', Style::default())
                    }
                } else if c.col_h < 0.5 && base == 0.0 {
                    (stroke::QG[c.pat as usize], Self::rail())
                } else {
                    let g = (c.top + 1) as f64 / h2f;
                    let color = self.cell_color(g, c.col_h / h2f, val);
                    (stroke::QG[c.pat as usize], Style::default().fg(color))
                };
                if let Some(cell) = buf.cell_mut((area.x + x as u16, screen_y)) {
                    cell.set_char(ch).set_style(style);
                }
            }
        }
    }

    /// G1 single mode: ONE braille row carrying the same timebase/fill/
    /// colour law as the big panels (squish frames or 1:1 scroll), drawn
    /// inside `area` (its extra rows, if any, stay blank).
    pub fn render_spark(mut self, area: Rect, buf: &mut Buffer) -> GraphStats {
        let w = area.width;
        let raw = self.hist.values();
        let last = raw.last().copied().unwrap_or(0.0);
        let n = raw.len();
        if w == 0 || area.height == 0 {
            return GraphStats { vmax: 0.0, columns: Vec::new(), last, n };
        }
        let squished = if self.opts.squish {
            squish_frame(&raw, w as usize, self.tick_ms)
        } else {
            let take = (w as usize).min(raw.len());
            let mut out = vec![0.0; w as usize - take];
            out.extend_from_slice(&raw[raw.len() - take..]);
            out
        };
        let (columns, vmax) = if let Some(max) = self.fixed_max {
            to_columns(&squished, w, 1, Norm::Fixed(max))
        } else if let Some(leaky) = self.leaky.as_deref_mut() {
            to_columns(&squished, w, 1, Norm::Leaky(leaky, 60))
        } else {
            to_columns(&squished, w, 1, Norm::Fixed(1.0))
        };
        // single-line law (maintainer 2026-11-07, corrected): sparks are MINI
        // GRAPHS honouring the chars choice — braille dot polyline (btop
        // style), block eighths ▁..█ growing from the cell floor, or the
        // line-mode stroke-position ladder (⎽ ⎼ ─ ‾: the shape rides in
        // the glyph's vertical position — a flat rule cannot show shape
        // in one row). One colour per cell means the ramp rides
        // HORIZONTALLY: gradient traversed LEFT->RIGHT across the whole
        // spark, flow pulses RIGHT->LEFT with time (newest enters at the
        // right edge), solid colours each glyph by its own value. The
        // `fill` option does NOT apply to sparks — the mini-graph shape
        // is fixed per style.
        let wi = w as usize;
        let hi = 1usize;
        let dh = stroke::heights(&columns, wi, 4);
        let brow = stroke::braille_row(&columns, &dh, FillMode::Line, wi, hi, 0);
        for x in 0..wi {
            let val = columns[x];
            // columns are ALREADY normalized (to_columns returns v/vmax);
            // dividing by vmax AGAIN flattened every block spark to the
            // rail — the "does not grow vertically" bug (maintainer 2026-11-07).
            let f = val.clamp(0.0, 1.0);
            let (ch, style) = match self.opts.chars {
                Chars::Braille(_) => {
                    let c = &brow[x];
                    if c.pat == 0 {
                        if c.col_h < 0.5 {
                            (stroke::RAIL_DOT, Self::rail())
                        } else {
                            (' ', Style::default())
                        }
                    } else if c.col_h < 0.5 {
                        (stroke::dot_glyph(c.pat), Self::rail())
                    } else {
                        (stroke::dot_glyph(c.pat), Style::default().fg(self.spark_color(x, wi, f)))
                    }
                }
                Chars::Block(_) => {
                    let lev = (f * 8.0).round().max(if val > 0.0 { 1.0 } else { 0.0 }) as u32;
                    if lev == 0 {
                        ('\u{2581}', Self::rail())
                    } else {
                        let ch = std::char::from_u32(0x2580 + lev).unwrap_or('\u{2588}');
                        (ch, Style::default().fg(self.spark_color(x, wi, f)))
                    }
                }
                Chars::Line(_) => {
                    if val <= 0.0 {
                        ('\u{2500}', Self::rail())
                    } else {
                        (
                            line_spark_glyph(f, false),
                            Style::default().fg(self.spark_color(x, wi, f)),
                        )
                    }
                }
            };
            if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y)) {
                cell.set_char(ch).set_style(style);
            }
        }
        GraphStats { vmax, columns, last, n }
    }
}

/// flow wash for the horizontal single-line law (maintainer 2026-11-07):
/// a colour feature at column x moves LEFTWARD as phase grows — pulses
/// enter at the RIGHT edge and travel with time. wash(y, p + 1/w) ==
/// wash(y + 1, p) pins the direction.
fn spark_flow_wash(x: usize, w: usize, phase: f64) -> f64 {
    if w == 0 {
        return 0.0;
    }
    (((x as f64 + phase * w as f64).rem_euclid(w as f64)) / (w as f64)).min(1.0)
}

/// line-mode sparkline glyph (maintainer 2026-11-07): the six-glyph rule
/// cannot show shape in one row, so the value rides the glyph's
/// VERTICAL POSITION — a stroke ladder from the low scan line to the
/// high overline. Zero stays the dim baseline rule.
fn line_spark_glyph(f: f64, idle: bool) -> char {
    if idle {
        return '\u{2500}';
    }
    match f {
        v if v < 0.25 => '\u{23BD}', // ⎽
        v if v < 0.50 => '\u{23BC}', // ⎼
        v if v < 0.75 => '\u{2500}', // ─
        _ => '\u{203E}',             // ‾
    }
}

impl<'a> Widget for Graph<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let _ = self.render_stats(area, buf);
    }
}

#[cfg(test)]
mod opts_wiring_tests {
    //! the chips actually change the paint now (maintainer: "stuck on braille
    //! line mode, I can change the setting but it is buggy")
    use super::*;
    use ratatui::backend::TestBackend;

    fn render_line(opts: GraphOpts, vals: &[f64]) -> String {
        let mut r = Ring::new(64);
        for v in vals {
            r.push(*v);
        }
        let backend = TestBackend::new(10, 3);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            let g = Graph {
                hist: &r,
                opts,
                fixed_max: Some(1.0),
                leaky: None,
                tick: 0,
                tick_ms: 0,
                    seam_back: None,
};
            f.render_widget(g, f.area());
        }).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.width)
            .map(|x| buf[(x, 2)].symbol().to_string())
            .collect::<String>()
    }

    /// one-row render of render_spark; returns (glyph, fg) per column.
    /// `fixed_max` pins the scale so expected levels are deterministic.
    fn render_spark_cells(opts: GraphOpts, vals: &[f64], tick: u64) -> Vec<(char, Option<ratatui::style::Color>)> {
        let mut r = Ring::new(vals.len() + 8);
        for v in vals {
            r.push(*v);
        }
        let backend = TestBackend::new(vals.len() as u16, 1);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            let g = Graph {
                hist: &r,
                opts,
                fixed_max: Some(100.0),
                leaky: None,
                tick,
                tick_ms: 2000,
                    seam_back: None,
};
            let _ = g.render_spark(f.area(), f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.width)
            .map(|x| {
                let c = &buf[(x, 0)];
                (
                    c.symbol().chars().next().unwrap_or(' '),
                    c.style().fg,
                )
            })
            .collect()
    }

    #[test]
    fn spark_block_grows_vertically() {
        // maintainer regression 2026-11-07: render_spark divided the ALREADY
        // normalized columns by vmax AGAIN -> every eighth rounded to 0 ->
        // a flat ▁ rail for chars:block. Values spanning 0..100 must ride
        // real eighth levels; peak column full height.
        let cells = render_spark_cells(
            GraphOpts {
                squish: false,
                chars: Chars::Block(None),
                colors: ColorMode::Solid,
                ramp: Ramp::Heat,
            },
            &[0.0, 12.0, 25.0, 50.0, 75.0, 100.0, 60.0, 30.0],
            0,
        );
        let g: Vec<char> = cells.iter().map(|(c, _)| *c).collect();
        assert_eq!(g.len(), 8);
        assert_eq!(g[0], '\u{2581}', "zero column is the rail: {g:?}");
        assert_eq!(g[3], '\u{2584}', "50/100 is the half block: {g:?}");
        assert_eq!(g[5], '\u{2588}', "100/100 is full height: {g:?}");
        assert!(
            g.iter().collect::<std::collections::HashSet<_>>().len() >= 4,
            "flat spark: {g:?}"
        );
    }

    #[test]
    fn spark_flow_travels_right_to_left() {
        // the wash value a column shows AFTER advancing phase by 1/w is
        // the wash its RIGHT neighbour showed before -> the colour moved
        // one column LEFT. New pulses enter at the right edge.
        let w = 8usize;
        let p = 0.3;
        for y in 0..w {
            let later = spark_flow_wash(y, w, p + 1.0 / w as f64);
            let earlier = spark_flow_wash((y + 1) % w, w, p);
            let d = (later - earlier).abs();
            assert!(d < 1e-9 || (d - 1.0).abs() < 1e-9, "y={y}: {later} vs {earlier}");
        }
    }

    #[test]
    fn spark_gradient_is_left_to_right() {
        // gradient traverses the ramp LEFT->RIGHT: column x carries
        // ramp sample (x+1)/w — left edge near the ramp start, right
        // edge at its end.
        let cells = render_spark_cells(
            GraphOpts {
                squish: false,
                chars: Chars::Block(None),
                colors: ColorMode::Gradient,
                ramp: Ramp::Heat,
            },
            &[50.0; 8],
            0,
        );
        for (x, (_, fg)) in cells.iter().enumerate() {
            let want = column_color(ColorMode::Gradient, Ramp::Heat, (x as f64 + 1.0) / 8.0, 0.0, 1.0);
            assert_eq!(*fg, Some(want), "column {x} off the left->right ramp");
        }
    }

    #[test]
    fn spark_line_ladder_uses_four_positions() {
        assert_eq!(line_spark_glyph(0.1, false), '\u{23BD}');
        assert_eq!(line_spark_glyph(0.4, false), '\u{23BC}');
        assert_eq!(line_spark_glyph(0.6, false), '\u{2500}');
        assert_eq!(line_spark_glyph(0.9, false), '\u{203E}');
        assert_eq!(line_spark_glyph(0.0, true), '\u{2500}');
        // and the spark itself paints the ladder, not a flat rule
        let cells = render_spark_cells(
            GraphOpts {
                squish: false,
                chars: Chars::Line(None),
                colors: ColorMode::Solid,
                ramp: Ramp::Heat,
            },
            &[10.0, 40.0, 60.0, 90.0, 0.0],
            0,
        );
        let g: Vec<char> = cells.iter().map(|(c, _)| *c).collect();
        assert_eq!(g[0], '\u{23BD}');
        assert_eq!(g[1], '\u{23BC}');
        assert_eq!(g[2], '\u{2500}');
        assert_eq!(g[3], '\u{203E}');
        assert_eq!(g[4], '\u{2500}', "zero stays the dim baseline");
    }

    #[test]
    fn spark_solid_colours_by_value() {
        // solid: each glyph coloured by its OWN value (needs the real
        // fill fraction, not the double-divided one)
        let opts = GraphOpts {
            squish: false,
            chars: Chars::Block(None),
            colors: ColorMode::Solid,
            ramp: Ramp::Heat,
        };
        let low = render_spark_cells(opts.clone(), &[10.0, 10.0, 10.0, 10.0], 0);
        assert!(low.windows(2).all(|p| p[0].1 == p[1].1), "equal values, equal colours");
        let mix = render_spark_cells(opts, &[10.0, 90.0, 10.0, 90.0], 0);
        assert_ne!(mix[0].1, mix[1].1, "10 vs 90 must colour apart");
    }

    fn opts(chars: Chars, squish: bool) -> GraphOpts {
        GraphOpts { chars, colors: ColorMode::Gradient, ramp: Ramp::Heat, squish }
    }

    #[test]
    fn braille_area_fills_below_the_line() {
        // BR-AREA law: braille keeps its dot stroke, but bar/area fill
        // every dot BELOW the line .
        let vals = vec![1.0; 10];
        let stroke = render_line(opts(Chars::Braille(None), false), &vals);
        let area = render_line(opts(Chars::Braille(Some(Fill::Area)), false), &vals);
        assert!(
            stroke.chars().all(|c| c == ' ' || c == '\u{2800}'),
            "stroke leaves the bottom row empty: {stroke:?}"
        );
        assert!(
            area.chars().all(|c| c == '\u{28ff}'),
            "area fills the bottom row solid: {area:?}"
        );
    }

                #[test]
    fn block_stroke_is_thin_quadrant_and_gapless() {
        // restored pre-1b89bf6 law: block+fill=line is quadrant pixel
        // art — thin (no full block cell where the line is not really
        // four quadrants deep), and every column between the first and
        // last ink carries ink (the segment walk cannot skip a column).
        let spike = render_rows(Chars::Block(None), ColorMode::Gradient, &[0.0, 1.0, 0.0], 3, 3);
        assert!(
            !spike.iter().any(|r| r.contains('\u{2588}')),
            "thin stroke painted a full block: {spike:?}"
        );
        // every column inked somewhere
        for x in 0..3 {
            assert!(
                spike.iter().any(|r| r.chars().nth(x).unwrap() != ' '),
                "stroke dropped col {x}: {spike:?}"
            );
        }
        // bar fill still paints the full block at the spike (contrast)
        let bar = render_rows(Chars::Block(Some(Fill::Bar)), ColorMode::Gradient, &[0.0, 1.0, 0.0], 3, 3);
        assert!(bar.iter().any(|r| r.contains('\u{2588}')), "bar lost the spike: {bar:?}");
        // the ink of adjacent columns touches: no column pair has a
        // vertical jump larger than one cell while both stay mid-graph
        let ramp = render_rows(Chars::Block(None), ColorMode::Gradient, &[0.1, 0.9], 2, 3);
        let top_of = |x: usize| {
            ramp.iter()
                .position(|r| r.chars().nth(x).unwrap() != ' ')
                .unwrap()
        };
        assert!(
            top_of(1).abs_diff(top_of(0)) <= 2,
            "ramp columns leapt apart: {ramp:?}"
        );
    }


    #[test]
    fn timebase_scroll_and_squish_differ_on_long_history() {
        // one old spike, then silence: scroll forgets it, squish keeps it
        let mut vals = vec![1.0];
        vals.extend(std::iter::repeat(0.0).take(40));
        let scroll = render_line(opts(Chars::Block(Some(Fill::Bar)), false), &vals);
        let squish = render_line(opts(Chars::Block(Some(Fill::Bar)), true), &vals);
        assert!(scroll.trim_end_matches([' ', '\u{2581}']).is_empty(), "scroll kept noise: {scroll:?}");
        assert!(squish.contains('\u{2585}') || squish.contains('\u{2588}') || squish.contains('\u{2587}') || squish.contains('\u{2586}'),
            "squish lost the old peak: {squish:?}");
    }

    // ---- R4: style laws ported from the reference engine ----

    /// bottom-row helper: last row of a full-area render
    fn render_rows(chars: Chars, colors: ColorMode, vals: &[f64], w: u16, h: u16) -> Vec<String> {
        let mut r = Ring::new(64);
        for v in vals {
            r.push(*v);
        }
        let mut term = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let g = Graph {
                hist: &r,
                opts: GraphOpts { chars, colors, ramp: Ramp::Heat, squish: false },
                fixed_max: Some(1.0),
                leaky: None,
                tick: 0,
                tick_ms: 0,
                seam_back: None,
            };
            f.render_widget(g, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

            #[test]
    fn line_underfill_is_strokes_not_blocks() {
        // chars=line + fill: the underfill is built from the line
        // alphabet itself (maintainer 2026-11-07): bar = vertical rules under
        // the trace, area = horizontal rules; plain line = nothing under.
        // plateau in the middle: the PLAIN line paints only the top rule
        // there, so anything below it can only come from the fill
        let cols: Vec<f64> = vec![0.0, 1.0, 1.0, 1.0, 0.0];
        let bare = render_rows(Chars::Line(None), ColorMode::Gradient, &cols, 5, 4);
        let bar = render_rows(Chars::Line(Some(Fill::Bar)), ColorMode::Gradient, &cols, 5, 4);
        let area = render_rows(Chars::Line(Some(Fill::Area)), ColorMode::Gradient, &cols, 5, 4);
        // plateau column 2: trace rides row 0; rows 2..3 sit under it —
        // bare paints nothing there, bar fills with U+2502 runs.
        for r in 2..4 {
            let c_bare = bare[r].chars().nth(2).unwrap();
            let c_bar = bar[r].chars().nth(2).unwrap();
            assert_eq!(c_bare, ' ', "bare line leaked at row {r}: {bare:?}");
            assert_eq!(c_bar, '\u{2502}', "bar underfill missing at {r}: {bar:?}");
            let c_area = area[r].chars().nth(2).unwrap();
            assert_eq!(c_area, '\u{2500}', "area underfill missing at {r}: {area:?}");
        }
        // the truly idle tail (col 4, flat zero) never takes fill — only
        // the bottom rail; col 0 is the step's rise, that ink is the line
        for r in 1..3 {
            // rows 1..h-1 only: the bottom row belongs to the rail and
            // the line's own outgoing flat, never to the fill
            assert!(
                [bar[r].chars().nth(4).unwrap(), area[r].chars().nth(4).unwrap()]
                    .iter()
                    .all(|c| *c == ' '),
                "fill leaked into idle col 4 at row {r}: bar={bar:?} area={area:?}"
            );
        }
    }

    #[test]
    fn spark_honours_the_chars_choice() {
        // single-line graphs respect chars (maintainer): block draws eighths,
        // line draws the flat rule, braille draws dots — three visibly
        // different rows from the SAME data.
        let mut r = Ring::new(16);
        for i in 0..16 {
            r.push(((i % 4) as f64 + 1.0) / 4.0);
        }
        let mk = |chars: Chars| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 16, 1));
            Graph {
                hist: &r,
                opts: GraphOpts {
                    chars,
                    colors: ColorMode::Solid,
                    ramp: Ramp::from_name("green"),
                    squish: false,
                },
                tick: 0,
                tick_ms: 2000,
                leaky: None,
                fixed_max: None,
                seam_back: None,
            }
            .render_spark(Rect::new(0, 0, 16, 1), &mut buf);
            (0..16)
                .map(|x| buf.content()[x].symbol().chars().next().unwrap_or(' '))
                .collect::<String>()
        };
        let block = mk(Chars::Block(None));
        assert!(
            block.chars().all(|c| ('\u{2581}'..='\u{2588}').contains(&c)),
            "block spark left the eighths band: {block:?}"
        );
        let line = mk(Chars::Line(None));
        assert!(
            line.chars()
                .all(|c| matches!(c, '\u{2500}' | '\u{23BD}' | '\u{23BC}' | '\u{203E}')),
            "line spark left the stroke ladder: {line:?}"
        );
        assert!(
            line.chars().collect::<std::collections::HashSet<_>>().len() >= 2,
            "line ladder shows no shape variety: {line:?}"
        );
        let braille = mk(Chars::Braille(None));
        assert!(
            braille.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
            "braille spark lost its dots: {braille:?}"
        );
    }

#[test]
    fn r4_line_uses_exactly_the_six_glyphs() {
        // nvtop law (maintainer 2026-11-07): chars=line is the six-glyph stair
        // set — no diagonals (corner-ended, can never join the mid-edge
        // handoff), no block/braille glyphs (those are the OTHER chars
        // options). Stair [0,1,0,1,0] at h=3 exercises every transition.
        let cols: Vec<f64> = vec![0.0, 1.0, 0.0, 1.0, 0.0];
        let rows = render_rows(Chars::Line(None), ColorMode::Gradient, &cols, 5, 3);
        let six = ['\u{2500}', '\u{2502}', '\u{256D}', '\u{256E}', '\u{256F}', '\u{2570}'];
        for ch in rows.concat().chars() {
            assert!(ch == ' ' || six.contains(&ch), "line mode leaked {:?}", ch);
        }
        let joined: String = rows.concat();
        for c in six {
            assert!(joined.contains(c), "six-glyph set missing {:?}", c);
        }
        // seam audit: a column's cap row is its right-edge handoff; the
        // NEXT column must carry ink on exactly that row.
        let ink_at = |r: usize, x: usize| rows[r].chars().nth(x).unwrap_or(' ') != ' ';
        for x in 0..4 {
            let rows_x: Vec<usize> = (0..3).filter(|r| ink_at(*r, x)).collect();
            let rows_n: Vec<usize> = (0..3).filter(|r| ink_at(*r, x + 1)).collect();
            assert!(!rows_x.is_empty() && !rows_n.is_empty(), "col {}/{} blank", x, x + 1);
            // flat+flat share row; step: next column's foot is on this
            // column's cap row. Either way ONE row must be common.
            assert!(
                rows_x.iter().any(|r| rows_n.contains(r)),
                "col {x} {:?} detached from col {} {:?}",
                rows_x,
                x + 1,
                rows_n
            );
        }
    }

#[test]
    fn r4_idle_columns_ride_the_rail() {
        // silence paints a rail, never a blank void: braille ⣀, quadrant
        // ▄, block-bar ▁ across the bottom row; every row above is blank.
        let zero = vec![0.0; 8];
        let br = render_rows(Chars::Braille(None), ColorMode::Gradient, &zero, 8, 2);
        assert!(br[1].chars().all(|c| c == '\u{28C0}'), "braille rail missing: {br:?}");
        assert!(br[0].chars().all(|c| c == ' '), "braille rows above rail: {br:?}");
        let qd = render_rows(Chars::Line(None), ColorMode::Gradient, &zero, 8, 2);
        assert!(qd[1].chars().all(|c| c == '\u{2500}'), "line rail missing: {qd:?}");
        assert!(qd[0].chars().all(|c| c == ' '), "quad rows above rail: {qd:?}");
        let bb = render_rows(Chars::Block(Some(Fill::Bar)), ColorMode::Gradient, &zero, 8, 2);
        assert!(bb[1].chars().all(|c| c == '\u{2581}'), "bar rail missing: {bb:?}");
    }

    #[test]
    fn r4_area_fills_to_the_stroke_edge_not_rounded_rows() {
        // the area glitch: the old hack ORed whole dot rows using a
        // column-rounded top, leaving holes/notches under a ramp. The
        // ported law fills EVERY dot at or below the stroke edge, so on
        // a rising ramp each column's bottom row must carry the bottom
        // dot-row bits (0xC0) as soon as the value reaches >= 1 dot.
        let cols = [0.3, 0.5, 0.7, 0.9, 0.9, 0.7, 0.5, 0.3];
        let rows = render_rows(Chars::Braille(Some(Fill::Area)), ColorMode::Solid, &cols, 8, 2);
        let bottom = &rows[1];
        for (x, ch) in bottom.chars().enumerate() {
            let bits = ch as u32 - 0x2800;
            assert!(
                bits & 0xC0 == 0xC0,
                "col {x} bottom dots unlit under a 0.3+ value: {rows:?}"
            );
        }
        // and the edge stays crisp: the topmost row is NOT a solid
        // ⣿ band (the old row-OR smeared whole rows up top on a ramp)
        assert!(!rows[0].chars().all(|c| c == '\u{28FF}'), "top row smeared: {rows:?}");
    }

    #[test]
    fn r4_bar_and_area_are_different_pictures() {
        // bar = one column per SAMPLE (no interpolation); area = solid
        // body up to the interpolated stroke edge. Identical renders
        // would mean a dead fill chip.
        let vals = [0.9, 0.1, 0.8, 0.2, 0.9];
        let bar = render_rows(Chars::Braille(Some(Fill::Bar)), ColorMode::Solid, &vals, 8, 2);
        let area = render_rows(Chars::Braille(Some(Fill::Area)), ColorMode::Solid, &vals, 8, 2);
        assert_ne!(bar, area, "bar and area render identically");
    }

    #[test]
    fn r4_quadrant_glyph_bit_order_is_upright() {
        // v7 shipped UL=1|UR=2|LL=4|LR=8 and EVERY glyph rendered
        // vertically flipped (the 'inverted characters' complaint).
        use crate::paint::stroke::QG;
        assert_eq!(QG[0b0001], '\u{2597}'); // LR ▗ bottom-right
        assert_eq!(QG[0b0010], '\u{2596}'); // LL ▖ bottom-left
        assert_eq!(QG[0b0100], '\u{259D}'); // UR ▝ top-right
        assert_eq!(QG[0b1000], '\u{2598}'); // UL ▘ top-left
        assert_eq!(QG[0b1111], '\u{2588}');
    }

    #[test]
    fn r4_line_mode_is_connected_across_steep_moves() {
        // the complaint: '-' per rounded row broke into floating dashes
        // on steep moves. Quadrant art lights every cell the segment
        // crosses, so no column is blank between inked neighbours and
        // the ink count exceeds one-glyph-per-column.
        let vals = [0.0, 1.0, 0.0, 1.0, 0.0];
        let rows = render_rows(Chars::Line(None), ColorMode::Gradient, &vals, 8, 3);
        let ink: usize = rows
            .iter()
            .flat_map(|r| r.chars())
            .filter(|c| *c != ' ')
            .count();
        assert!(ink > 8, "dashed, disconnected line (ink={ink}): {rows:?}");
        // every column has something (stroke pixels or rail)
        for x in 0..8usize {
            assert!(
                rows.iter().any(|r| r.chars().nth(x).unwrap() != ' '),
                "col {x} fully blank: {rows:?}"
            );
        }
    }

    #[test]
    fn r4_all_style_combos_paint() {
        // every chars x fill x colors combo renders ink without panic
        let vals = [0.2, 0.6, 1.0, 0.4, 0.1, 0.7];
        for chars in [
            Chars::Braille(None),
            Chars::Braille(Some(Fill::Bar)),
            Chars::Braille(Some(Fill::Area)),
            Chars::Block(None),
            Chars::Block(Some(Fill::Bar)),
            Chars::Block(Some(Fill::Area)),
            Chars::Line(None),
        ] {
            for colors in [ColorMode::Gradient, ColorMode::Solid, ColorMode::Flow] {
                let rows = render_rows(chars, colors, &vals, 10, 3);
                let inked = rows.iter().any(|r| r.chars().any(|c| c != ' '));
                assert!(inked, "combo {chars:?}/{colors:?} painted nothing");
            }
        }
    }
}

#[cfg(test)]
mod seam_paint_tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn seam_paints_dim_vertical_straight_through_ink() {
        // ring: 6 rail samples, then the focused lane's live data
        let mut r = Ring::new(64);
        for _ in 0..6 {
            r.push(0.0);
        }
        r.push(40.0);
        r.push(40.0);
        let mut leaky = LeakyMax::new(1.0);
        let mut term = ratatui::Terminal::new(TestBackend::new(20, 5)).unwrap();
        term.draw(|f| {
            let g = Graph {
                hist: &r,
                opts: GraphOpts {
                    chars: Chars::Braille(None),
                    colors: ColorMode::Solid,
                    ramp: Ramp::Heat,
                    squish: false,
                },
                tick: 0,
                tick_ms: 2000,
                leaky: Some(&mut leaky),
                fixed_max: Some(80.0),
                seam_back: Some(2), // switch was 2 samples back
            };
            f.render_widget(g, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let seam_col = 20 - 1 - 3; // w-1-(back+1)
        let mut hits = 0;
        for y in 0..5 {
            if buf[(seam_col, y)].symbol() == "\u{2502}" {
                hits += 1;
            }
        }
        assert_eq!(hits, 5, "seam must ride every data row");
        // ink columns are untouched: the live column keeps a braille dot
        // and it must NOT hide behind ink: seam_back = Some(0) puts the
    // boundary ON a live ink column — it still paints top to bottom.
    let mut term2 = ratatui::Terminal::new(TestBackend::new(20, 5)).unwrap();
    term2.draw(|f| {
        let g = Graph {
            hist: &r,
            opts: GraphOpts {
                chars: Chars::Braille(None),
                colors: ColorMode::Solid,
                ramp: Ramp::Heat,
                squish: false,
            },
            tick: 0,
            tick_ms: 2000,
            leaky: Some(&mut leaky),
            fixed_max: Some(80.0),
            seam_back: Some(0),
        };
        f.render_widget(g, f.area());
    })
    .unwrap();
    let buf2 = term2.backend().buffer().clone();
    let seam_col2 = 20 - 1 - 1; // w-1-(back+1) — an ink column
    let hits2 = (0..5)
        .filter(|y| buf2[(seam_col2, *y)].symbol() == "\u{2502}")
        .count();
    assert_eq!(hits2, 5, "seam must cross the old lane's ink, not hide");
    let live_col = 19;
        let dots = (0..5)
            .filter(|y| {
                buf[(live_col, *y)]
                    .symbol()
                    .chars()
                    .next()
                    .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
            })
            .count();
        assert!(dots >= 1, "live ink column was wiped: {:?}",
            (0..5).map(|y| buf[(live_col, y)].symbol()).collect::<Vec<_>>());
    }
}
