//! M7.4/M7.5: manager routing + live log viewer behavior.
use lsmtop::svc::logs::tail_lines;
use lsmtop::ui::logs::{LogSource, LogState, LogsWidget, MAX_LINES};
use lsmtop::ui::manager::{plan_action, router_lane_lines, ActionPlan, ManagerWidget, PanelInfo};
use ratatui::{backend::TestBackend, Terminal, widgets::Widget};
use serde_json::json;

fn target(kind: &str, unit: &str, scope: &str, model: Option<&str>) -> serde_json::Value {
    json!({"kind": kind, "name": "t", "unit": unit, "scope": scope,
           "flags": {"model": model.unwrap_or("")}})
}

#[test]
fn system_scope_is_hand_off_never_executed() {
    assert_eq!(
        plan_action(&target("server", "llama-server", "system", None), "start", false),
        ActionPlan::Handoff("sudo systemctl start llama-server".into())
    );
}

#[test]
fn user_scope_executes_directly_without_sudo() {
    assert_eq!(
        plan_action(&target("server", "llama", "user", None), "restart", false),
        ActionPlan::ExecUnit(vec![
            "systemctl".into(), "--user".into(), "restart".into(), "llama".into()
        ])
    );
}

#[test]
fn owned_target_routing() {
    // start needs a configured model
    assert_eq!(
        plan_action(&target("server", "", "", None), "start", false),
        ActionPlan::Bark("no model configured for this target".into())
    );
    assert_eq!(
        plan_action(&target("server", "", "", Some("/m/x.gguf")), "start", false),
        ActionPlan::OwnedStart
    );
    // stop only when actually running
    assert_eq!(
        plan_action(&target("server", "", "", Some("/m/x.gguf")), "stop", false),
        ActionPlan::Bark("nothing running".into())
    );
    assert_eq!(
        plan_action(&target("server", "", "", Some("/m/x.gguf")), "stop", true),
        ActionPlan::OwnedStop
    );
}

#[test]
fn router_lines_carry_loaded_child_ports() {
    let models = json!({"data": [
        {"id": "lane-a", "status": {"value": "loaded", "args": ["x", "--port", "37347"]}},
        {"id": "lane-b", "status": {"value": "unloaded", "args": ["x", "--port", "41533"]}}
    ]});
    let lines = router_lane_lines(&models);
    assert_eq!(lines[0], "lane-a  loaded  :37347", "loaded lane shows status + child port");
    assert_eq!(lines[1], "lane-b  unloaded", "unloaded lane shows status, no port");
}

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
fn manager_footer_pinned_at_every_size() {
    let targets = vec![target("router", "llama-swap", "system", None), target("server", "", "", Some("/m.gguf"))];
    for (w, h) in [(80u16, 24u16), (70, 18), (60, 14)] {
        let infos = vec![
            PanelInfo { unit_state: Some("active (running)".into()), owned_running: false, router_lines: vec!["lane-a  :37347".into()], sample_error: None },
            PanelInfo { unit_state: None, owned_running: false, router_lines: vec![], sample_error: None },
        ];
        let text = render_widget(
            ManagerWidget { targets: &targets, infos, selected: 0, has_server: true },
            w,
            h,
        );
        let last = text.lines().last().unwrap();
        assert!(last.contains("esc Back"), "{w}x{h} footer missing: {last}");
        let first_panel = text.lines().next().unwrap();
        assert!(first_panel.contains("t") || first_panel.contains('─'), "{w}x{h} panel missing");
    }
}

#[test]
fn logstate_caps_and_tails_like_python() {
    let mut s = LogState::new(LogSource::Journal {
        name: "r".into(), unit: "u".into(), scope: "system".into(),
    });
    for i in 0..(MAX_LINES + 500) {
        s.push_line(&format!("line {i}"));
    }
    assert_eq!(s.lines.len(), MAX_LINES);
    assert_eq!(s.banner(), " r — journal: u — live ");
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
