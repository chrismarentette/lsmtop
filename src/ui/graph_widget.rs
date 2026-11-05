use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::Widget,
    style::Style,
};
use crate::hist::{Ring, squish_frame, to_columns, Norm};
use crate::engine::leaky::{LeakyMax, flow_phase};
use crate::paint::modes::{Mode as ColorMode, column_color};
use crate::paint::ramps::Ramp;
use crate::paint::glyphs::{Chars, Fill, eighth_block};
use crate::paint::braille;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphOpts { 
    pub chars: Chars,
    pub colors: ColorMode,
    pub ramp: Ramp,
    /// timebase: false = scroll (1:1 newest samples, python DEFAULT),
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
/// app.py _overlay) can ride the SAME normalized cells the curve used.
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
            // scroll: last w samples 1:1, left-padded (python default)
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
            Chars::Braille(_) => {
                let braille_strings = braille::encode(&columns, w, h);
                for (y, row_str) in braille_strings.into_iter().enumerate() {
                    let screen_y = area.y + y as u16;
                    for (x, ch) in row_str.chars().enumerate() {
                        let screen_x = area.x + x as u16;
                        let val = columns[x];
                        
                        let ratio = if self.opts.colors == ColorMode::Gradient {
                            if h > 1 {
                                (h - 1 - y as u16) as f64 / (h - 1) as f64
                            } else {
                                1.0
                            }
                        } else {
                            val
                        };
                        
                        let phase = flow_phase(self.tick, val);
                        let color = column_color(
                            self.opts.colors, 
                            self.opts.ramp, 
                            ratio, 
                            phase, 
                            vmax
                        );
                        
                        if let Some(cell) = buf.cell_mut((screen_x, screen_y)) {
                            cell.set_char(ch).set_style(Style::default().fg(color));
                        }
                    }
                }
            }
            Chars::Block(fill) => {
                // python _render_blocks: fill=line draws a stroke (only the
                // waterline glyph); bar fills bottom-up in eighth-cells;
                // area = bar with the 1-2-1 smoothing. fill was previously
                // ignored entirely (maintainer: "stuck on braille line mode").
                let levels = f64::from(h) * 8.0;
                let totals: Vec<u16> = match fill {
                    Some(Fill::Area) if w > 2 => {
                        let l: Vec<f64> = columns
                            .iter()
                            .map(|v| v.clamp(0.0, 1.0) * (levels - 0.01))
                            .collect();
                        let n = l.len().max(1);
                        (0..columns.len())
                            .map(|x| {
                                ((l[x.saturating_sub(1)]
                                    + 2.0 * l[x]
                                    + l[(x + 1).min(n - 1)])
                                    / 4.0)
                                    .round()
                                    .max(0.0) as u16
                            })
                            .collect()
                    }
                    _ => columns
                        .iter()
                        .map(|v| (v.clamp(0.0, 1.0) * levels).round() as u16)
                        .collect(),
                };
                for x in 0..w {
                    let val = columns[x as usize];
                    let screen_x = area.x + x;
                    let phase = flow_phase(self.tick, val);

                    let total_eighths = totals.get(x as usize).copied().unwrap_or(0);
                    let stroke = fill.is_none();
                    
                    for y in 0..h {
                        let screen_y = area.y + (h - 1 - y);
                        
                        let ratio = if self.opts.colors == ColorMode::Gradient {
                            if h > 1 {
                                y as f64 / (h - 1) as f64
                            } else {
                                1.0
                            }
                        } else {
                            val
                        };
                        
                        let color = column_color(
                            self.opts.colors,
                            self.opts.ramp,
                            ratio,
                            phase,
                            vmax
                        );
                        let style = Style::default().fg(color);
                        
                        let cell_bottom_eighth = y * 8;
                        let cell_top_eighth = cell_bottom_eighth + 8;
                        
                        let ch = if total_eighths >= cell_top_eighth {
                            if stroke { ' ' } else { eighth_block(1.0) }
                        } else if total_eighths > cell_bottom_eighth {
                            let frac = (total_eighths - cell_bottom_eighth) as f64 / 8.0;
                            eighth_block(frac)
                        } else {
                            if y == 0 { eighth_block(0.0) } else { ' ' }
                        };
                        
                        if let Some(cell) = buf.cell_mut((screen_x, screen_y)) {
                            cell.set_char(ch).set_style(style);
                        }
                    }
                }
            }
            Chars::Line => {
                for x in 0..w {
                    let val = columns[x as usize];
                    let phase = flow_phase(self.tick, val);
                    let h_val = (val * (h as f64 - 1.0)).round() as u16;
                    
                    for y in 0..h {
                        let screen_y = area.y + (h - 1 - y);
                        
                        let ratio = if self.opts.colors == ColorMode::Gradient {
                            if h > 1 {
                                y as f64 / (h - 1) as f64
                            } else {
                                1.0
                            }
                        } else {
                            val
                        };
                        
                        let color = column_color(
                            self.opts.colors,
                            self.opts.ramp,
                            ratio,
                            phase,
                            vmax
                        );
                        
                        let ch = if y == h_val { '-' } else if y == 0 { '_' } else { ' ' };
                        if let Some(cell) = buf.cell_mut((area.x + x, screen_y)) {
                            cell.set_char(ch).set_style(Style::default().fg(color));
                        }
                    }
                }
            }
        }
        stats
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
    fn block_bar_fills_bottom_up_but_stroke_does_not() {
        // 90% of a 3-row box: a bar floors the baseline with a FULL cell;
        // a stroke keeps the baseline empty (waterline only). If these ever
        // render identically the fill chip is dead again (maintainer round 4).
        let vals = [0.9, 0.0, 0.0, 0.0, 0.0, 0.0];
        let bar = render_line(opts(Chars::Block(Some(Fill::Bar)), false), &vals);
        let stroke = render_line(opts(Chars::Block(None), false), &vals);
        // the spike sits at the padded newest column; find it in the bar
        let col = bar
            .chars()
            .position(|c| c == '\u{2588}')
            .expect("no full bar cell anywhere");
        // stroke at the same column keeps the baseline empty (waterline only)
        assert_eq!(
            stroke.chars().nth(col).unwrap(),
            ' ',
            "stroke painted the baseline at col {col}: {stroke:?}"
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
}
