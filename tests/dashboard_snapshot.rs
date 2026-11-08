use lsmtop::engine::leaky::LeakyMax;
use lsmtop::hist::Ring;
use lsmtop::paint::glyphs::Chars;
use lsmtop::paint::modes::Mode;
use lsmtop::paint::ramps::Ramp;
use lsmtop::ui::dashboard::{Part,metrics_strip, ring_stats, strip_text, DashRow, DashboardWidget, Legend, PaneSize, ScalePos, StripTok};
use lsmtop::ui::graph_widget::GraphOpts;
use ratatui::{backend::TestBackend, Terminal};

/// Snapshot v2 (2026-11-04): the M6-era snapshot froze a SKELETON dashboard
/// (2 unlabeled graphs, no KV/GPU panels) that had drifted behind the
/// v9.15 shell — maintainer caught it live. This golden now pins the
/// faithful shell: banner · stats · 4 legend graphs · 8-col table · strip.
// one widget under test, one parameter per field group — the count is the shape
#[allow(clippy::too_many_arguments)]
fn build<'a>(
    gen: &'a Ring,
    prompt: &'a Ring,
    kv: &'a Ring,
    apu: &'a Ring,
    gl: &'a mut LeakyMax,
    pl: &'a mut LeakyMax,
    rows: Vec<DashRow>,
    toks: &'a [StripTok],
    stext: &'a str,
    scale: ScalePos,
) -> DashboardWidget<'a> {
    let mk = |ring: &Ring, title: &'a str, _hw: &'a str, _hv: f64| {
        let (avg, max) = ring_stats(ring);
        Legend {
            title,
            parts: vec![Part::Num("avg", avg), Part::Num("max", max)],
        }
    };
    DashboardWidget {
        stats: "GPU  42% · VRAM 5.2/8.0 GiB · NVIDIA GeForce RTX 4060",
        hotkeys: stext,
        hotkey_toks: toks,
        show_lanes: true,
        opts: GraphOpts {
            squish: false,
            chars: Chars::Braille(None),
            colors: Mode::Gradient,
            ramp: Ramp::from_name("heat"),
        },
        tick: 7,
        tick_ms: 2000,
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
        sizes: [PaneSize::Full; 4],
        graph_scroll: 0,
        journal: None,
        journal_h: 0,
        rows,
        scale,
    }
}

/// Shared fixture rings so every position test drives the SAME data.
fn fixture() -> (Ring, Ring, Ring, Ring, Vec<DashRow>) {
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
    let rows = rows_fixture();
    (gen, prompt, kv, apu, rows)
}

fn rows_fixture() -> Vec<DashRow> {
    vec![
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
    ]
}

#[test]
fn gutter_columns_never_carry_graph_ink() {
    // maintainer law 2026-11-07: the graph data is NEVER drawn behind the
    // three-column ruler. The ruler's own furniture (│ border on the
    // gutter column ADJACENT to the plot, tick digits, live value) is
    // legal there; braille/block/line trace ink is not.
    for pos in [ScalePos::Right, ScalePos::Left, ScalePos::Both, ScalePos::Off] {
        let (gen, prompt, kv, apu, rows) = fixture();
        let mut gl = LeakyMax::new(0.997);
        let mut pl = LeakyMax::new(0.997);
        let toks = metrics_strip();
        let stext = strip_text(&toks);
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let w = build(
                    &gen, &prompt, &kv, &apu, &mut gl, &mut pl, rows.clone(), &toks, &stext, pos,
                );
                f.render_widget(w, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let w = buf.area.width;
        let guard_left = matches!(pos, ScalePos::Left | ScalePos::Both);
        let guard_right = matches!(pos, ScalePos::Right | ScalePos::Both);
        for (i, cell) in buf.content().iter().enumerate() {
            let x = (i % w as usize) as u16;
            let y = (i / w as usize) as u16;
            if y < 2 {
                continue; // banner + first label row are furniture
            }
            let in_left = guard_left && x < 4; // body margin + ruler cols 1..3
            let in_right = guard_right && (x >= w - 4 && x <= w - 2); // ruler cols
            if !in_left && !in_right {
                continue;
            }
            let g = cell.symbol().chars().next().unwrap_or(' ');
            let is_ink = ('\u{2800}'..='\u{28FF}').contains(&g) // braille
                || ('\u{2580}'..='\u{259F}').contains(&g) // blocks/shades
                || matches!(g, '─' | '╭' | '╮' | '╰' | '╯');
            if is_ink && g == '─' {
                // the LABEL row's rule legitimately crosses above the
                // ruler region (ruler = data rows only); identify it by
                // the row's legend text and skip that single row
                continue;
            }
            assert!(
                !is_ink,
                "graph ink {g:?} in {:?} gutter at ({x},{y})",
                if in_left { "left" } else { "right" }
            );
        }
    }
}

#[test]
fn test_dashboard_snapshot() {
    // the LEFT ruler position keeps the historical law visible in goldens
    let (gen, prompt, kv, apu, rows) = fixture();
    let mut gl = LeakyMax::new(0.997);
    let mut pl = LeakyMax::new(0.997);
    let toks = metrics_strip();
    let stext = strip_text(&toks);
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let w = build(
                &gen, &prompt, &kv, &apu, &mut gl, &mut pl, rows.clone(), &toks, &stext,
                ScalePos::Left,
            );
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

// four positions, one law: each ruler edge carries the ticks, the bottom
// 0, and the live value; the plot owns everything between. Rebaselined
// goldens are law documents — eyeball them (maintainer 2026-11-07).
#[test]
fn dashboard_scale_right() {
    snap_pos(ScalePos::Right);
}
#[test]
fn dashboard_scale_both() {
    snap_pos(ScalePos::Both);
}
#[test]
fn dashboard_scale_off() {
    snap_pos(ScalePos::Off);
}

fn snap_pos(pos: ScalePos) {
    let (gen, prompt, kv, apu, rows) = fixture();
    let mut gl = LeakyMax::new(0.997);
    let mut pl = LeakyMax::new(0.997);
    let toks = metrics_strip();
    let stext = strip_text(&toks);
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let w = build(
                &gen, &prompt, &kv, &apu, &mut gl, &mut pl, rows.clone(), &toks, &stext, pos,
            );
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
    insta::assert_snapshot!(format!("scale_{pos:?}"), lines.join("\n"));
}
