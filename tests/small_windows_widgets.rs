//! Small-window law for REAL widgets (v9.13 doctrine, Rust enforcement):
//! at 80x24, 70x18 and 60x14 nothing garbles and pinned rows survive.
use lsmtop::config::Config;
use lsmtop::ui::logs::{LogSource, LogState, LogsWidget};
use lsmtop::ui::manager::{ManagerWidget, PanelInfo};
use lsmtop::ui::opts::{OptsState, OptsWidget};
use ratatui::{backend::TestBackend, widgets::Widget, Terminal};
use serde_json::json;
use std::path::{Path, PathBuf};

fn render<W: Widget>(w: W, width: u16, height: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
    t.draw(move |f| w.render(f.area(), f.buffer_mut())).unwrap();
    let b = t.backend().buffer().clone();
    (0..b.area().height)
        .map(|y| {
            (0..b.area().width)
                .map(|x| {
                    b.cell((x, y))
                        .map(|c| c.symbol().chars().next().unwrap_or(' '))
                        .unwrap_or(' ')
                })
                .collect::<String>()
        })
        .collect()
}

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

const SIZES: [(u16, u16); 3] = [(80, 24), (70, 18), (60, 14)];

#[test]
fn manager_law_at_all_sizes() {
    let targets = vec![
        json!({"kind":"router","name":"router-main","unit":"llama-swap","scope":"system"}),
        json!({"kind":"server","name":"owned-box","unit":"","scope":"system","flags":{"model":"/m/x.gguf"}}),
    ];
    for (w, h) in SIZES {
        let infos = vec![
            PanelInfo {
                unit_state: Some("active (running)".into()),
                owned_running: false,
                router_lines: vec!["lane-a  :37347".into()],
                sample_error: None,
            },
            PanelInfo {
                unit_state: None,
                owned_running: true,
                router_lines: vec![],
                sample_error: None,
            },
        ];
        let lines = render(ManagerWidget { targets: &targets, infos, selected: 0, has_server: true }, w, h);
        let last = lines.last().unwrap();
        assert!(last.contains("esc Back"), "{w}x{h}: pinned footer lost: {last:?}");
        assert!(lines[0].contains("router-main"), "{w}x{h}: first panel title lost");
    }
}

#[test]
fn logs_law_at_all_sizes() {
    for (w, h) in SIZES {
        let mut s = LogState::new(LogSource::Journal {
            name: "router".into(),
            unit: "llama-server".into(),
            scope: "system".into(),
        });
        for i in 0..200 {
            s.push_line(&format!("journal line {i} tokens per second"));
        }
        let lines = render(LogsWidget(&s), w, h);
        assert!(lines[0].contains("journal:"), "{w}x{h}: banner lost");
        let joined = lines.join("\n");
        assert!(joined.contains("journal line 199"), "{w}x{h}: live line lost");
    }
}

#[test]
fn opts_chips_survive_small_sizes() {
    let cfg = Config::load(Some(scratch("none-a.json"))).unwrap();
    for (w, h) in SIZES {
        let st = OptsState::from_config(&cfg);
        let joined = render(OptsWidget(&st), w, h).join("\n");
        assert!(joined.contains("chars"), "{w}x{h}: chars row lost");
        assert!(joined.contains("fill"), "{w}x{h}: fill row lost");
        assert!(joined.contains("braille"), "{w}x{h}: chip values lost");
    }
}

#[test]
fn opts_state_machine() {
    let cfg = Config::load(Some(scratch("none-b.json"))).unwrap();
    let mut st = OptsState::from_config(&cfg);
    assert_eq!(st.value("chars"), "braille");
    assert_eq!(st.value("ramp"), "heat");
    assert!(!st.rows.iter().find(|r| r.key == "fill").unwrap().disabled);
    // chars -> line disables fill VISIBLY (v9.15), and the disabled row
    // refuses input
    st.sel = 0;
    st.cycle(1);
    st.cycle(1);
    assert_eq!(st.value("chars"), "line");
    assert!(
        st.rows.iter().find(|r| r.key == "fill").unwrap().disabled,
        "fill must show disabled while chars=line"
    );
    st.sel = 1;
    let before = st.value("fill");
    st.cycle(1);
    assert_eq!(st.value("fill"), before, "disabled row ignores input");
    // chars back to braille re-enables instantly
    st.sel = 0;
    st.cycle(1); // line -> wrap -> braille? (wrap through row end)
    if st.value("chars") != "braille" {
        st.cycle(1);
    }
    assert_eq!(st.value("chars"), "braille");
    assert!(!st.rows.iter().find(|r| r.key == "fill").unwrap().disabled);
}

#[test]
fn opts_save_roundtrip_explicit_path() {
    let path = scratch("opts-roundtrip.json");
    let _ = std::fs::remove_file(&path);
    let mut cfg = Config::load(Some(path.clone())).unwrap();
    let mut st = OptsState::from_config(&cfg);
    st.sel = 4; // ramp row
    st.cycle(1); // heat -> spectrum
    assert_eq!(st.value("ramp"), "spectrum");
    st.save(&mut cfg, &path).expect("save");
    let reloaded = Config::load(Some(path.clone())).unwrap();
    assert_eq!(reloaded.metrics().ramp, "spectrum");
    let mode = std::fs::metadata(&path).map(|m| {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o777
    });
    assert_eq!(mode.ok(), Some(0o600), "save keeps 0600");
    std::fs::remove_file(&path).ok();
}
