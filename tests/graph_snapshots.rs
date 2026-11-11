use ratatui::{
    backend::TestBackend,
    Terminal,
    layout::Rect,
};
use std::collections::HashSet;
use insta::assert_snapshot;
use lsmtop::hist::Ring;
use lsmtop::engine::leaky::LeakyMax;
use lsmtop::ui::graph_widget::{Graph, GraphOpts};
use lsmtop::paint::modes::Mode as ColorMode;
use lsmtop::paint::ramps::Ramp;
use lsmtop::paint::glyphs::{Chars, Fill};

fn lcg_series(count: usize) -> Vec<f64> {
    let mut val = 42u32;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        val = val.wrapping_mul(1103515245).wrapping_add(12345) & 0x7FFFFFFF;
        out.push((val as f64) / (0x7FFFFFFF as f64) * 100.0);
    }
    out
}

fn render_to_string(opts: GraphOpts) -> String {
    let backend = TestBackend::new(20, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    
    let series = lcg_series(60);
    let mut ring = Ring::new(60);
    for v in series {
        ring.push(v);
    }
    
    let mut leaky = LeakyMax::new(0.997);
    
    terminal.draw(|f| {
        let widget = Graph {
            hist: &ring,
            opts,
            tick: 10,
            tick_ms: 2000,
            leaky: Some(&mut leaky),
            fixed_max: None,
                seam_back: None,
};
        f.render_widget(widget, Rect::new(0, 0, 20, 10));
    }).unwrap();
    
    let buffer = terminal.backend().buffer();
    
    let mut s = String::new();
    for y in 0..10 {
        for x in 0..20 {
            s.push_str(buffer.cell((x, y)).unwrap().symbol());
        }
        s.push('\n');
    }
    s
}

fn extract_colors(opts: GraphOpts) -> HashSet<ratatui::style::Color> {
    let backend = TestBackend::new(20, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    
    let series = lcg_series(60);
    let mut ring = Ring::new(60);
    for v in series {
        ring.push(v);
    }
    
    let mut leaky = LeakyMax::new(0.997);
    
    terminal.draw(|f| {
        let widget = Graph {
            hist: &ring,
            opts,
            tick: 10,
            tick_ms: 2000,
            leaky: Some(&mut leaky),
            fixed_max: None,
                seam_back: None,
};
        f.render_widget(widget, Rect::new(0, 0, 20, 10));
    }).unwrap();
    
    let buffer = terminal.backend().buffer();
    let mut colors = HashSet::new();
    for y in 0..10 {
        for x in 0..20 {
            colors.insert(buffer.cell((x, y)).unwrap().fg);
        }
    }
    colors
}

#[test]
fn test_graph_snapshots_chars() {
    let s_braille = render_to_string(GraphOpts {
        squish: false,
        chars: Chars::Braille(None),
        colors: ColorMode::Gradient,
        ramp: Ramp::Heat,
    });
    let s_block = render_to_string(GraphOpts {
        squish: false,
        chars: Chars::Block(Some(Fill::Bar)),
        colors: ColorMode::Gradient,
        ramp: Ramp::Heat,
    });
    let s_line = render_to_string(GraphOpts {
        squish: false,
        chars: Chars::Line(None),
        colors: ColorMode::Gradient,
        ramp: Ramp::Heat,
    });
    
    assert_ne!(s_braille, s_block);
    assert_ne!(s_block, s_line);
    assert_ne!(s_braille, s_line);
    
    assert_snapshot!("braille", s_braille);
    assert_snapshot!("block", s_block);
    assert_snapshot!("line", s_line);
}

// v0.2.0 lockdown: the STROKE identities are law too — quadrant block
// art, braille dots, and the line alphabet's vert/horiz rule fills.
#[test]
fn test_graph_snapshots_strokes() {
    let stroke = |chars: Chars| {
        render_to_string(GraphOpts {
            squish: false,
            chars,
            colors: ColorMode::Gradient,
            ramp: Ramp::Heat,
        })
    };
    let s_bstroke = stroke(Chars::Braille(None));
    let s_quad = stroke(Chars::Block(None));
    let s_line_off = stroke(Chars::Line(None));
    let s_line_v = stroke(Chars::Line(Some(Fill::Bar)));
    let s_line_h = stroke(Chars::Line(Some(Fill::Area)));
    // quadrant art must differ from the waterline fill AND carry quadrant glyphs
    assert_ne!(s_quad, stroke(Chars::Block(Some(Fill::Bar))));
    assert!(
        s_quad
            .chars()
            .any(|c| matches!(c, '▗' | '▖' | '▘' | '▝' | '▙' | '▟' | '▛' | '▜' | '▌' | '▐')),
        "block stroke drew no quadrant glyphs: {s_quad:?}"
    );
    // line fills are rules from the six-glyph alphabet, and differ
    assert_ne!(s_line_off, s_line_v);
    assert_ne!(s_line_v, s_line_h);
    assert!(s_line_v.contains('│') && s_line_h.contains('─'));
    assert_snapshot!("braille_stroke", s_bstroke);
    assert_snapshot!("block_quadrant", s_quad);
    assert_snapshot!("line_vert_rules", s_line_v);
    assert_snapshot!("line_horiz_rules", s_line_h);
}

#[test]
fn test_graph_colors_modes() {
    let c_grad = extract_colors(GraphOpts {
        squish: false,
        chars: Chars::Block(Some(Fill::Area)),
        colors: ColorMode::Gradient,
        ramp: Ramp::Heat,
    });
    let c_solid = extract_colors(GraphOpts {
        squish: false,
        chars: Chars::Block(Some(Fill::Area)),
        colors: ColorMode::Solid,
        ramp: Ramp::Heat,
    });
    let c_flow = extract_colors(GraphOpts {
        squish: false,
        chars: Chars::Block(Some(Fill::Area)),
        colors: ColorMode::Flow,
        ramp: Ramp::Heat,
    });
    
    assert_ne!(c_grad, c_solid);
    assert_ne!(c_solid, c_flow);
    assert_ne!(c_grad, c_flow);
}

fn render_spark_line(opts: GraphOpts) -> String {
    let series = lcg_series(60);
    let mut ring = Ring::new(60);
    for v in series {
        ring.push(v);
    }
    let backend = TestBackend::new(40, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let g = Graph {
                hist: &ring,
                opts,
                fixed_max: Some(100.0),
                leaky: None,
                tick: 7,
                tick_ms: 2000,
                    seam_back: None,
};
            let _ = g.render_spark(Rect::new(0, 0, 40, 1), f.buffer_mut());
        })
        .unwrap();
    let buf = terminal.backend().buffer();
    (0..buf.area.width)
        .map(|x| buf[(x, 0)].symbol().chars().next().unwrap_or(' '))
        .collect::<String>()
}

#[test]
fn test_graph_snapshots_sparks() {
    // single-line MINI GRAPHS (maintainer 2026-11-07 corrected law): braille
    // dot polyline, block eighths growing from the floor, line-mode
    // stroke-position ladder. The goldens are law documents.
    let mk = |chars: Chars| {
        render_spark_line(GraphOpts {
            squish: false,
            chars,
            colors: ColorMode::Gradient,
            ramp: Ramp::Heat,
        })
    };
    let spark_b = mk(Chars::Braille(None));
    let spark_k = mk(Chars::Block(None));
    let spark_l = mk(Chars::Line(None));
    let distinct = |s: &String| -> usize { s.chars().collect::<HashSet<_>>().len() };
    assert!(
        spark_k
            .chars()
            .any(|c| ('\u{2582}'..='\u{2588}').contains(&c)),
        "block spark stuck at the rail: {spark_k:?}"
    );
    assert!(distinct(&spark_k) >= 4, "block spark is flat: {spark_k:?}");
    assert!(
        spark_l.chars().all(|c| matches!(c, '\u{2500}' | '\u{23BD}' | '\u{23BC}' | '\u{203E}'))
            && distinct(&spark_l) >= 2,
        "line spark left the ladder or shows no shape: {spark_l:?}"
    );
    assert!(spark_b.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c)), "no dots: {spark_b:?}");
    assert_ne!(spark_b, spark_k);
    assert_ne!(spark_k, spark_l);
    assert_snapshot!("spark_braille", spark_b);
    assert_snapshot!("spark_block", spark_k);
    assert_snapshot!("spark_line", spark_l);
}
