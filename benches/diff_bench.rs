//! Terminal-diff efficiency: ratatui suppresses Crossterm writes by
//! diffing buffers between frames. If an UNCHANGED dashboard state ever
//! diffs to nonzero cells, every tick would repaint the screen — flicker
//! and a write storm. This bench proves it doesn't and prices the worst
//! case (everything changed). Gate reads `diff/unchanged_cells` (a COUNT
//! bench, must be 0) and `diff/frame` (µs budget).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use lsmtop::engine::leaky::LeakyMax;
use lsmtop::hist::Ring;
use lsmtop::paint::glyphs::Chars;
use lsmtop::paint::modes::Mode;
use lsmtop::paint::ramps::Ramp;
use lsmtop::ui::dashboard::{
    metrics_strip, ring_stats, strip_text, DashRow, DashboardWidget, Legend, PaneSize, Part,
    ScalePos, StripTok,
};
use lsmtop::ui::graph_widget::GraphOpts;
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

fn fed(cap: usize, ticks: usize, amp: f64) -> Ring {
    let mut r = Ring::new(cap);
    for i in 0..ticks {
        r.push((amp * (i as f64 / 37.0).sin()).abs() + 0.5);
    }
    r
}

#[allow(clippy::too_many_arguments)]
fn widget<'a>(
    rings: &'a [Ring; 4],
    gl: &'a mut LeakyMax,
    pl: &'a mut LeakyMax,
    rows: &'a [DashRow],
    toks: &'a [StripTok],
    stext: &'a str,
) -> DashboardWidget<'a> {
    let mk = |i: usize, title: &'a str| {
        let (avg, max) = ring_stats(&rings[i]);
        Legend {
            title,
            parts: vec![Part::Num("avg", avg), Part::Num("max", max)],
        }
    };
    DashboardWidget {
        stats: "GPU  42% · VRAM 5.2/8.0 GiB",
        hotkeys: stext,
        hotkey_toks: toks,
        show_lanes: true,
        opts: GraphOpts {
            squish: true,
            chars: Chars::Braille(None),
            colors: Mode::Gradient,
            ramp: Ramp::from_name("heat"),
        },
        tick: 7,
        tick_ms: 500,
        gen: &rings[0],
        gen_leaky: gl,
        gen_legend: mk(0, "GEN t/s"),
        prompt: &rings[1],
        prompt_leaky: pl,
        prompt_legend: mk(1, "PROMPT t/s"),
        kv: &rings[2],
        kv_legend: mk(2, "CACHE"),
        apu: &rings[3],
        apu_legend: mk(3, "GPU %"),
        vis: [true, true, true, true],
        sizes: [PaneSize::Full; 4],
        graph_scroll: 0,
        journal: None,
        journal_h: 0,
        rows: rows.to_vec(),
        scale: ScalePos::Right,
    }
}

fn paint(rings: &[Ring; 4], rows: &[DashRow], toks: &[StripTok], stext: &str) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
    terminal
        .draw(|f| {
            let mut gl = LeakyMax::new(0.997);
            let mut pl = LeakyMax::new(0.997);
            let w = widget(rings, &mut gl, &mut pl, rows, toks, stext);
            f.render_widget(w, f.area());
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn bench_diff(c: &mut Criterion) {
    let toks = metrics_strip();
    let stext = strip_text(&toks);
    let rings_a: [Ring; 4] = [fed(10_000, 10_000, 60.0), fed(10_000, 10_000, 600.0), fed(200, 200, 90.0), fed(200, 200, 100.0)];
    let rings_b: [Ring; 4] = [fed(10_000, 10_001, 60.0), fed(10_000, 10_001, 600.0), fed(200, 201, 90.0), fed(200, 201, 100.0)];
    let rows: Vec<DashRow> = (0..4)
        .map(|i| DashRow {
            cells: [
                format!("lane-{i}"),
                "—".into(),
                "610t/s".into(),
                "12.3k".into(),
                "45.2t/s".into(),
                "33.0%".into(),
                "12%".into(),
                "24.5k".into(),
                "0.88".into(),
            ],
        })
        .collect();

    // The LAW proof (runs every `cargo bench` invocation, cheap): identical
    // state must diff to ZERO cells — nothing to rewrite, no flicker.
    let a1 = paint(&rings_a, &rows, &toks, &stext);
    let a2 = paint(&rings_a, &rows, &toks, &stext);
    let unchanged = a1.diff(&a2).len();
    assert_eq!(unchanged, 0, "unchanged dashboard state MUST diff to 0 cells");

    let b1 = paint(&rings_b, &rows, &toks, &stext);
    let mut g = c.benchmark_group("diff");
    // gate: this count bench MUST report 0 (perf-check.sh asserts it)
    g.bench_function("unchanged_cells", |bench| {
        bench.iter(|| black_box(a1.diff(&a2).len()))
    });
    // worst case: one real tick of activity repaints what changed
    g.bench_function("frame", |bench| bench.iter(|| black_box(a1.diff(&b1).len())));
    g.finish();
}

criterion_group!(benches, bench_diff);
criterion_main!(benches);
