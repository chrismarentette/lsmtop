use ratatui::{
    layout::Rect,
    widgets::Widget,
    buffer::Buffer,
};
use lsmtop::engine::leaky::LeakyMax;
use lsmtop::hist::Ring;
use lsmtop::paint::glyphs::Chars;
use lsmtop::paint::modes::Mode;
use lsmtop::paint::ramps::Ramp;
use lsmtop::ui::dashboard::{Part,metrics_strip, ring_stats, strip_text, DashRow, DashboardWidget, Legend, PaneSize, ScalePos};
use lsmtop::ui::graph_widget::GraphOpts;

// Layout doctrine test for the v9.15 shell: instead of poking at Rects,
// RENDER the real widget into tiny buffers — the small-window law is
// "never panic, keep banner and hotkey strip" (v9.13 doctrine).

fn buf_text(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| {
                    buf.cell((x, y))
                        .map(|c| c.symbol().chars().next().unwrap_or(' '))
                        .unwrap_or(' ')
                })
                .collect::<String>()
        })
        .collect()
}

#[test]
fn test_small_windows_doctrine() {
    let mut gen = Ring::new(600);
    let mut prompt = Ring::new(600);
    let mut kv = Ring::new(600);
    let mut apu = Ring::new(600);
    for i in 0..64 {
        gen.push((i % 9) as f64);
        prompt.push((i % 13) as f64 * 9.0);
        kv.push((i % 3) as f64 * 30.0);
        apu.push((i % 5) as f64 * 15.0);
    }
    let mut gl = LeakyMax::new(0.997);
    let mut pl = LeakyMax::new(0.997);
    let rows = vec![DashRow {
        cells: [
            "lane-a".into(),
            "0".into(),
            "—".into(),
            "".into(),
            "—".into(),
            "0.0".into(),
            "0%".into(),
            "8k".into(),
            "—".into(),
        ],
    }];
    let mk = |ring: &Ring, title: &'static str, val: f64| {
        let (avg, max) = ring_stats(ring);
        let _ = val;
        Legend { title, parts: vec![Part::Num("avg", avg), Part::Num("max", max)] }
    };

    // ONE source for the strip: the real tokens, never a hand-typed copy
    let toks = metrics_strip();
    let stext = strip_text(&toks);
    let sizes = [(80, 24), (70, 18), (60, 14)];
    for &(w, h) in &sizes {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let wdg = DashboardWidget {
            seam: None,
            stats: "GPU  10% · VRAM 1.0/8.0 GiB",
            hotkeys: &stext,
            hotkey_toks: &toks,
            show_lanes: true,
            opts: GraphOpts {
                squish: false,
                chars: Chars::Braille(None),
                colors: Mode::Gradient,
                ramp: Ramp::from_name("heat"),
            },
            tick: 3,
            tick_ms: 2000,
            gen: &gen,
            gen_leaky: &mut gl,
            gen_legend: mk(&gen, "GEN t/s", 3.0),
            prompt: &prompt,
            prompt_leaky: &mut pl,
            prompt_legend: mk(&prompt, "PROMPT t/s", 100.0),
            kv: &kv,
            kv_legend: mk(&kv, "KV≈ %", 60.0),
            apu: &apu,
            apu_legend: mk(&apu, "GPU %", 60.0),
            vis: [true, true, true, true],
            sizes: [PaneSize::Full; 4],
            graph_scroll: 0,
            journal: None,
            journal_h: 0,
            rows: rows.clone(),
            scale: ScalePos::Left,
        };
        wdg.render(area, &mut buf); // never panics, whatever the size
        let text = buf_text(&buf);
        assert!(text[0].contains("GPU"), "stats line lost at top at {w}x{h}");
        let last = &text[h as usize - 1];
        for want in ["Journal", "Lanes", "Settings", "Quit"] {
            assert!(last.contains(want), "strip token {want} lost at {w}x{h}: {last:?}");
        }
        for gone in ["All/Active", "HF-CLI", "Llama", "Quit+Stop"] {
            assert!(!last.contains(gone), "dead strip token {gone} at {w}x{h}");
        }
    }
}
