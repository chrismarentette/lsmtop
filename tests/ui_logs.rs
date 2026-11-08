//! Live log viewer behavior (the manager page left the app 2026-11-07 —
//! server lifecycle is another tool's job; this file keeps the journal).
use lsmtop::svc::logs::tail_lines;
use lsmtop::ui::logs::{LogSource, LogState, LogsWidget, MAX_LINES};
use ratatui::{backend::TestBackend, widgets::Widget, Terminal};

fn render_widget<W: Widget>(w: W, wdt: u16, hgt: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(wdt, hgt)).unwrap();
    t.draw(move |f| w.render(f.area(), f.buffer_mut())).unwrap();
    let b = t.backend().buffer();
    (0..b.area.height)
        .map(|y| {
            (0..b.area.width)
                .map(|x| b.cell((x, y)).unwrap().symbol().chars().next().unwrap_or(' '))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn logstate_caps_and_tails() {
    let mut s = LogState::new(LogSource::Journal {
        name: "r".into(), unit: "u".into(), scope: "system".into(),
    });
    for i in 0..(MAX_LINES + 500) {
        s.push_line(&format!("line {i}"));
    }
    assert_eq!(s.lines.len(), MAX_LINES);
}

#[test]
fn owned_file_initial_tail_300_of_400() {
    let dir = std::path::PathBuf::from(
        std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()),
    );
    let path = dir.join("lsmtop-tail-test.log");
    let all: Vec<String> = (1..=400).map(|i| format!("l{i} tokens per second")).collect();
    std::fs::write(&path, all.join("\n")).unwrap();
    let s = LogState::new(LogSource::OwnedFile { name: "own".into(), port: 8080, path: path.clone() });
    let text: Vec<String> = s.lines.iter().map(|l| l.spans.iter().map(|sp| sp.content.as_ref()).collect::<String>()).collect();
    assert_eq!(text.len(), 300);
    assert_eq!(text[0], "l101 tokens per second", "splitlines()[-300:] parity");
    assert_eq!(text[299], "l400 tokens per second");
    // styling actually applied: unstyled prefix + bold-green t/s run
    let last_spans = s.lines.back().unwrap();
    assert!(last_spans.spans.len() >= 2, "t/s line should be styled in spans");
    let _ = tail_lines("a", 2);
    std::fs::remove_file(&path).ok();
}

#[test]
fn logs_widget_scrolls_up_from_live_bottom() {
    let mut s = LogState::new(LogSource::Journal {
        name: "r".into(), unit: "llama-server".into(), scope: "system".into(),
    });
    for i in 0..50 {
        s.push_line(&format!("logline-{i}"));
    }
    let bottom = render_widget(LogsWidget(&s), 90, 20);
    assert!(bottom.contains("logline-49"), "live line at bottom");
    assert!(!bottom.contains("logline-0"));
    s.scroll_by(40); // scroll up (away from live bottom)
    let up = render_widget(LogsWidget(&s), 90, 20);
    assert!(up.contains("logline-9"), "scrolled view shows older lines");
    assert!(!up.contains("logline-49"), "no scroll-follow once scrolled up");
    s.scroll_by(-1000);
    assert_eq!(s.scroll, 0);
    let back = render_widget(LogsWidget(&s), 90, 20);
    assert!(back.contains("logline-49"));
}
