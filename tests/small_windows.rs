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
use lsmtop::ui::dashboard::{ring_stats, DashRow, DashboardWidget, Legend};
use lsmtop::ui::graph_widget::GraphOpts;

// Layout doctrine test for the v9.15 shell: instead of poking at Rects,
// RENDER the real widget into tiny buffers — the small-window law is
// "never panic, keep banner and hotkey strip" (v9.13 doctrine).

pub fn manager_layout(size: Rect) -> (Rect, Rect, Rect) {
    let banner = Rect { x: size.x, y: size.y, width: size.width, height: 1.min(size.height) };
    let actions = Rect { 
        x: size.x, 
        y: size.y + size.height.saturating_sub(3), 
        width: size.width, 
        height: 3.min(size.height.saturating_sub(1)) 
    };
    let body = Rect { 
        x: size.x, 
        y: size.y + 1, 
        width: size.width, 
        height: size.height.saturating_sub(4) 
    };
    (banner, body, actions)
}

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
            "—".into(),
            "0.0".into(),
            "0%".into(),
            "8k".into(),
            "—".into(),
        ],
    }];
    let mk = |ring: &Ring, title: &'static str, val: f64| {
        let (avg, max) = ring_stats(ring);
        Legend { title, head: ("now", val), avg, max }
    };

    let sizes = [(80, 24), (70, 18), (60, 14)];
    for &(w, h) in &sizes {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let wdg = DashboardWidget {
            banner: " demo-host — 1/2 lanes loaded · unit active · PID 1 ",
            stats: "GPU  10% · VRAM 1.0/8.0 GiB",
            hotkeys: " a Lanes  l Journal  g Llama  s Settings  h HF  q Quit ",
            opts: GraphOpts {
                squish: false,
                chars: Chars::Braille(None),
                colors: Mode::Gradient,
                ramp: Ramp::from_name("heat"),
            },
            tick: 3,
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
            apu_on: true,
            rows: rows.clone(),
        };
        wdg.render(area, &mut buf); // never panics, whatever the size
        let text = buf_text(&buf);
        assert!(text[0].contains("lanes loaded"), "banner at {w}x{h}");
        assert!(
            text[h as usize - 1].contains("Lanes"),
            "hotkey strip at {w}x{h}"
        );

        // Settings / Manager / MetricsOpts layout pinning
        let (banner, body, actions) = manager_layout(area);
        // The action row is pinned at the bottom, outside the scroll body
        assert!(actions.y >= body.y + body.height);
        assert!(actions.height > 0);
        assert!(banner.height > 0);
        assert_eq!(banner.height + body.height + actions.height, area.height);
    }
}
