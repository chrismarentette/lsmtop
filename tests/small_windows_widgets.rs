//! Small-window law for REAL widgets (v9.13 doctrine, Rust enforcement):
//! at 80x24, 70x18 and 60x14 nothing garbles and pinned rows survive.
use lsmtop::config::Config;
use lsmtop::ui::logs::{LogSource, LogState, LogsWidget};
use lsmtop::ui::opts::OptsState;
use ratatui::{backend::TestBackend, widgets::Widget, Terminal};
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
        let joined = lines.join("\n");
        assert!(joined.contains("journal line 199"), "{w}x{h}: live line lost");
        // bare-rows law (2026-11-07): NO box, NO banner inside the screen —
        // the app header and the bottom strip are the only chrome
        assert!(!joined.contains("journal:"), "{w}x{h}: banner chrome crept back");
        assert!(
            !lines[0].trim_start().starts_with(['┌', '╭']),
            "{w}x{h}: box border crept back"
        );
    }
}

#[test]
fn opts_state_machine() {
    // the subscreen is gone; the rows + the disabled law are what Settings
    // embeds, so the law is tested on the rows themselves
    let cfg = Config::load(Some(scratch("none-b.json"))).unwrap();
    let mut st = OptsState::from_config(&cfg);
    assert_eq!(st.value("chars"), "braille");
    assert_eq!(st.value("ramp"), "heat");
    let ci = st.rows.iter().position(|r| r.key == "chars").unwrap();
    let fi = st.rows.iter().position(|r| r.key == "fill").unwrap();
    assert!(!st.rows[fi].disabled);
    // v9.31 (maintainer 2026-11-07): chars -> line does NOT disable fill —
    // it SWAPS the row to line's own vocabulary, default off (pure
    // line); vert/horiz fill with the line alphabet.
    st.rows[ci].cycle(1);
    st.rows[ci].cycle(1); // braille -> block -> line
    st.sync_disabled();
    assert_eq!(st.value("chars"), "line");
    assert_eq!(st.value("fill"), "off", "line mode defaults to no underfill");
    assert_eq!(st.rows[fi].values, vec!["off", "vert", "horiz"]);
    assert!(!st.rows[fi].disabled, "fill is never disabled anymore");
    st.rows[fi].cycle(1); // off -> vert
    assert_eq!(st.value("fill"), "vert");
    // back to braille: the old vocabulary AND the old remembered value
    // reappear — the line-mode pick (vert) belongs to fill_line
    st.rows[ci].active = 0;
    st.sync_disabled();
    assert_eq!(st.rows[fi].values, vec!["bar", "area", "line"]);
    assert_eq!(st.value("fill"), "bar");
    // and saving while in line-mode vocabulary lands on fill_line
    st.rows[ci].cycle(1); // braille -> block
    st.rows[ci].cycle(1); // block -> line
    st.sync_disabled();
    assert_eq!(st.value("fill"), "vert", "line slot remembered its pick");
    let path = scratch("fill-vocab.json");
    let _ = std::fs::remove_file(&path);
    let mut cfg = Config::load(Some(path.clone())).unwrap();
    st.save(&mut cfg, &path).expect("save");
    let re = Config::load(Some(path.clone())).unwrap();
    assert_eq!(re.metrics().fill_line, "vert");
}

#[test]
fn opts_scale_chip_row_present_and_saves() {
    // maintainer 2026-11-07: the ruler position is a Settings option with its
    // own row; the generic ring must carry it like every other key.
    let cfg = lsmtop::config::Config::load(Some("/tmp/nonexistent-lsmtop.json")).unwrap();
    let mut o = lsmtop::ui::opts::OptsState::from_config(&cfg);
    let row = o.rows.iter().position(|r| r.key == "scale").expect("scale row");
    assert_eq!(o.rows[row].values, vec!["right", "left", "both", "off"]);
    assert_eq!(o.value("scale"), "right");
    // chips persist through the ONE save path Settings uses
    o.rows[row].active = 2; // pick `both`
    let path = std::path::PathBuf::from(format!(
        "{}/test_opts_scale.json",
        std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into())
    ));
    let mut cfg2 = cfg.clone();
    o.save(&mut cfg2, &path).unwrap();
    let re = lsmtop::config::Config::load(Some(&path)).unwrap();
    assert_eq!(re.metrics().scale, "both");
}
