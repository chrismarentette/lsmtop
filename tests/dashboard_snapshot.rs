use lsmtop::engine::leaky::LeakyMax;
use lsmtop::hist::Ring;
use lsmtop::paint::glyphs::Chars;
use lsmtop::paint::modes::Mode;
use lsmtop::paint::ramps::Ramp;
use lsmtop::ui::dashboard::{ring_stats, DashRow, DashboardWidget, Legend};
use lsmtop::ui::graph_widget::GraphOpts;
use ratatui::{backend::TestBackend, Terminal};

/// Snapshot v2 (2026-11-04): the M6-era snapshot froze a SKELETON dashboard
/// (2 unlabeled graphs, no KV/GPU panels) that had drifted behind the
/// v9.15 shell — maintainer caught it live. This golden now pins the
/// faithful shell: banner · stats · 4 legend graphs · 8-col table · strip.
fn build<'a>(
    gen: &'a Ring,
    prompt: &'a Ring,
    kv: &'a Ring,
    apu: &'a Ring,
    gl: &'a mut LeakyMax,
    pl: &'a mut LeakyMax,
    rows: Vec<DashRow>,
) -> DashboardWidget<'a> {
    let mk = |ring: &Ring, title: &'a str, head_word: &'a str, head_val: f64| {
        let (avg, max) = ring_stats(ring);
        Legend {
            title,
            head: (head_word, head_val),
            avg,
            max,
        }
    };
    DashboardWidget {
        banner: " demo-router — 2/3 lanes loaded · unit active · PID 4242 ",
        stats: "GPU  42% · VRAM 5.2/8.0 GiB · NVIDIA GeForce RTX 4060",
        hotkeys: " a Lanes  l Journal  g Llama  s Settings  h HF  q Quit ",
        opts: GraphOpts {
            squish: false,
            chars: Chars::Braille(None),
            colors: Mode::Gradient,
            ramp: Ramp::from_name("heat"),
        },
        tick: 7,
        gen,
        gen_leaky: gl,
        gen_legend: mk(gen, "GEN t/s", "run", 45.2),
        prompt,
        prompt_leaky: pl,
        prompt_legend: mk(prompt, "PROMPT t/s", "run", 610.0),
        kv,
        kv_legend: mk(kv, "KV≈ %", "now", 33.0),
        apu,
        apu_legend: mk(apu, "GPU %", "now", 42.0),
        vis: [true, true, true, true],
        half: [false, false, false, false],
        focused: false,
        graph_scroll: 0,
        rows,
    }
}

#[test]
fn test_dashboard_snapshot() {
    let mut gen = Ring::new(600);
    for i in 0..120 {
        gen.push(((i % 40) as f64) * 1.2);
    }
    let mut prompt = Ring::new(600);
    for i in 0..120 {
        prompt.push(if i % 3 == 0 { 300.0 + (i % 7) as f64 } else { 0.0 });
    }
    let mut kv = Ring::new(600);
    for i in 0..120 {
        kv.push(((i % 30) * 3) as f64);
    }
    let mut apu = Ring::new(600);
    for i in 0..120 {
        apu.push(((i % 17) * 5) as f64);
    }
    let mut gl = LeakyMax::new(0.997);
    let mut pl = LeakyMax::new(0.997);
    let rows = vec![
        DashRow {
            cells: [
                "gemma-4-e4b-MTP-128k".into(),
                "1".into(),
                "610.4".into(),
                "812".into(),
                "45.2".into(),
                "33.0".into(),
                "82%".into(),
                "128k".into(),
                "71%".into(),
            ],
        },
        DashRow {
            cells: [
                "qwen3-32b".into(),
                "0".into(),
                "—".into(),
                "".into(),
                "—".into(),
                "0.0".into(),
                "0%".into(),
                "32k".into(),
                "—".into(),
            ],
        },
        DashRow {
            cells: [
                "stopped-model".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "—".into(),
                "stopped".into(),
            ],
        },
    ];

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let w = build(&gen, &prompt, &kv, &apu, &mut gl, &mut pl, rows.clone());
            f.render_widget(w, f.area());
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let mut lines = Vec::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push(
                buffer
                    .cell((x, y))
                    .unwrap()
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' '),
            );
        }
        lines.push(line);
    }
    insta::assert_snapshot!(lines.join("\n"));
}
