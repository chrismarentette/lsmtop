use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::Widget,
    style::{Color, Style},
};
use crate::hist::{Ring, squish_frame, to_columns, Norm};
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
    pub leaky: Option<&'a mut LeakyMax>,
    pub fixed_max: Option<f64>,
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
            squish_frame(&raw, w as usize, self.tick)
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
                    // block + line: 2x2 QUADRANT pixel art (same segment
                    // math as the braille stroke) — thin, gapless, no
                    // wedging. 1/8 bars cannot draw a thin line.
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
            Chars::Line => {
                // R4: old-school chars are the SAME quadrant pixel art
                // (thin, gapless across cell boundaries, ▄ rail under
                // idle columns). The old painter drew one '-' per column
                // at a rounded row — every steep move broke into
                // disconnected dashes, the exact "line mode
                // connectivity" complaint.
                self.paint_quad(area, buf, &columns);
            }
        }
        stats
    }

    fn rail() -> Style {
        Style::default().fg(Color::DarkGray) // bright_black, btop's rail
    }

    /// One colour call for every painter: `gfrac` is the gradient/flow
    /// height fraction for THIS cell, `solid_frac` the whole-column
    /// height fraction (solid colours the full interpolated gamut per
    /// column), `val` the column's normalized value (flow keys off the
    /// VALUE against the view max, not the row).
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

    /// The 2x2 quadrant painter (chars=line, block+line) for the whole area.
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
            squish_frame(&raw, w as usize, self.tick)
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
        // same stroke + rail + colour laws as the big panels, h=1
        let (wi, hi) = (w as usize, 1usize);
        let dh = stroke::heights(&columns, wi, hi * 4);
        let cells = stroke::braille_row(&columns, &dh, FillMode::Line, wi, hi, 0);
        for (x, c) in cells.iter().enumerate() {
            let val = columns[x];
            let (ch, style) = if c.pat == 0 {
                if c.col_h < 0.5 {
                    (stroke::RAIL_DOT, Self::rail())
                } else {
                    (' ', Style::default())
                }
            } else if c.col_h < 0.5 {
                (stroke::dot_glyph(c.pat), Self::rail())
            } else {
                let g = (c.top + 1) as f64 / 4.0;
                let color = self.cell_color(g, c.col_h / 4.0, val);
                (stroke::dot_glyph(c.pat), Style::default().fg(color))
            };
            if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y)) {
                cell.set_char(ch).set_style(style);
            }
        }
        GraphStats { vmax, columns, last, n }
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
            };
            f.render_widget(g, f.area());
        }).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.width)
            .map(|x| buf[(x, 2)].symbol().to_string())
            .collect::<String>()
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
    fn block_bar_fills_bottom_up_but_stroke_does_not() {
        // 90% of a 3-row box: a bar floors the baseline with a FULL cell;
        // a stroke (quadrant pixel art) rides through the baseline with
        // pixels but NEVER a full cell. If these ever render identically
        // the fill chip is dead again (maintainer round 4).
        let vals = [0.9, 0.0, 0.0, 0.0, 0.0, 0.0];
        let bar = render_line(opts(Chars::Block(Some(Fill::Bar)), false), &vals);
        let stroke = render_line(opts(Chars::Block(None), false), &vals);
        // the spike sits at the padded newest column; find it in the bar
        let col = bar
            .chars()
            .position(|c| c == '\u{2588}')
            .expect("no full bar cell anywhere");
        // the stroke never floors a full block cell at the spike
        assert_ne!(
            stroke.chars().nth(col).unwrap(),
            '\u{2588}',
            "stroke painted a full bar cell at col {col}: {stroke:?}"
        );
        assert!(
            !stroke.contains('\u{2588}'),
            "a thin stroke rendered a full block somewhere: {stroke:?}"
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
    fn r4_idle_columns_ride_the_rail() {
        // silence paints a rail, never a blank void: braille ⣀, quadrant
        // ▄, block-bar ▁ across the bottom row; every row above is blank.
        let zero = vec![0.0; 8];
        let br = render_rows(Chars::Braille(None), ColorMode::Gradient, &zero, 8, 2);
        assert!(br[1].chars().all(|c| c == '\u{28C0}'), "braille rail missing: {br:?}");
        assert!(br[0].chars().all(|c| c == ' '), "braille rows above rail: {br:?}");
        let qd = render_rows(Chars::Line, ColorMode::Gradient, &zero, 8, 2);
        assert!(qd[1].chars().all(|c| c == '\u{2584}'), "quad rail missing: {qd:?}");
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
        let rows = render_rows(Chars::Line, ColorMode::Gradient, &vals, 8, 3);
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
            Chars::Line,
        ] {
            for colors in [ColorMode::Gradient, ColorMode::Solid, ColorMode::Flow] {
                let rows = render_rows(chars, colors, &vals, 10, 3);
                let inked = rows.iter().any(|r| r.chars().any(|c| c != ' '));
                assert!(inked, "combo {chars:?}/{colors:?} painted nothing");
            }
        }
    }
}
