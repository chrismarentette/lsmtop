//! Frame-render cost of the FULL dashboard on a headless TestBackend.
//!
//! What the perf-check gate reads: the `frame/*` groups, p95 in ms —
//! the contract is a full 120x36 dashboard paint well under the 16.6 ms
//! terminal budget (gate floor 8 ms, docs/perf-gate.md).
//!
//! Scale knobs that are HONEST for this app: `hist::Ring` capacity fed
//! 1k/10k/100k samples once at setup (history volume through the squish
//! path) and lane rows 1 vs 11 (the table + ACCEPT column widen). The
//! render pass itself sees fixed screen cells either way — that IS the
//! point: paint cost must not grow with stored history.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use lsmtop::engine::leaky::LeakyMax;
use lsmtop::hist::Ring;
use lsmtop::paint::glyphs::Chars;
use lsmtop::paint::modes::Mode;
use lsmtop::paint::ramps::Ramp;
use lsmtop::ui::dashboard::{
    metrics_strip, ring_stats, strip_text, DashRow, DashboardWidget, Legend, PaneSize, Part,
    ScalePos,
};
use lsmtop::ui::graph_widget::GraphOpts;
use ratatui::{backend::TestBackend, Terminal};

fn fed(cap: usize, ticks: usize, amp: f64, period: f64) -> Ring {
    let mut r = Ring::new(cap);
    for i in 0..ticks {
        r.push((amp * (i as f64 / period).sin()).abs() + 0.5);
    }
    r
}

fn lanes(n: usize) -> Vec<DashRow> {
    (0..n)
        .map(|i| DashRow {
            cells: [
                format!("lane-model-{i}"),
                "42q".into(),
                "610t/s".into(),
                "12.3k".into(),
                "45.2t/s".into(),
                "33.0%".into(),
                "12%".into(),
                "24.5k".into(),
                "0.88".into(),
            ],
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn widget<'a>(
    gen: &'a Ring,
    prompt: &'a Ring,
    kv: &'a Ring,
    apu: &'a Ring,
    gl: &'a mut LeakyMax,
    pl: &'a mut LeakyMax,
    rows: Vec<DashRow>,
    toks: &'a [lsmtop::ui::dashboard::StripTok],
    stext: &'a str,
) -> DashboardWidget<'a> {
    let mk = |ring: &Ring, title: &'a str| {
        let (avg, max) = ring_stats(ring);
        Legend {
            title,
            parts: vec![Part::Num("avg", avg), Part::Num("max", max)],
        }
    };
    DashboardWidget {
            seam: None,
        stats: "GPU  42% · VRAM 5.2/8.0 GiB · AMD Radeon",
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
        gen,
        gen_leaky: gl,
        gen_legend: mk(gen, "GEN t/s"),
        prompt,
        prompt_leaky: pl,
        prompt_legend: mk(prompt, "PROMPT t/s"),
        kv,
        kv_legend: mk(kv, "CACHE"),
        apu,
        apu_legend: mk(apu, "GPU %"),
        vis: [true, true, true, true],
        sizes: [PaneSize::Full; 4],
        graph_scroll: 0,
        journal: None,
        journal_h: 0,
        rows,
        scale: ScalePos::Right,
    }
}

fn bench_frame(c: &mut Criterion) {
    let toks = metrics_strip();
    let stext = strip_text(&toks);
    let mut g = c.benchmark_group("frame/history");
    // (label, ring cap, samples fed, lanes)
    for (label, cap, fed_n, rows_n) in
        [("1k-1lane", 1_000usize, 1_000usize, 1usize), ("10k-1lane", 10_000, 10_000, 1), ("100k-1lane", 100_000, 100_000, 1), ("100k-11lanes", 100_000, 100_000, 11)]
    {
        let gen = fed(cap, fed_n, 60.0, 37.0);
        let prompt = fed(cap, fed_n, 600.0, 53.0);
        let kv = fed(cap, fed_n, 90.0, 29.0);
        let apu = fed(cap, fed_n, 100.0, 23.0);
        let rows = lanes(rows_n);
        g.bench_function(BenchmarkId::from_parameter(label), |b| {
            let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
            b.iter(|| {
                let mut gl = LeakyMax::new(0.997);
                let mut pl = LeakyMax::new(0.997);
                terminal
                    .draw(|f| {
                        let w = widget(
                            black_box(&gen),
                            &prompt,
                            &kv,
                            &apu,
                            &mut gl,
                            &mut pl,
                            rows.clone(),
                            &toks,
                            &stext,
                        );
                        f.render_widget(w, f.area());
                    })
                    .unwrap();
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench_frame);
criterion_main!(benches);
