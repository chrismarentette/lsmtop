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
            leaky: Some(&mut leaky),
            fixed_max: None,
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
            leaky: Some(&mut leaky),
            fixed_max: None,
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
        chars: Chars::Line,
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
