//! Journal pane sizing law (fixed-row tiers off/four/ten + scaling priority)
//! with validated option plumbing. Tiers are a FIXED number of display rows
//! (wrap-aware), so new lines never move the dashboard; full-screen journal
//! is a SCREEN the j-cycle opens and j closes again (cycle wraps to four).
use lsmtop::config::{journal_next, Config};
use lsmtop::ui::dashboard::journal_pane_h;

#[test]
fn pane_size_law() {
    // off (or unknown) never takes space
    assert_eq!(journal_pane_h(50, "off", 12), 0);
    assert_eq!(journal_pane_h(50, "bogus", 12), 0);
    // small windows keep the classic layout untouched (small-window law)
    assert_eq!(journal_pane_h(23, "ten", 12), 0);
    // fixed row counts: exactly 4 and exactly 10 on any body with room
    assert_eq!(journal_pane_h(50, "four", 12), 4);
    assert_eq!(journal_pane_h(50, "ten", 12), 10);
    assert_eq!(journal_pane_h(24, "four", 4), 4);
    assert_eq!(journal_pane_h(24, "ten", 4), 10);
    // scaling-priority: the journal flexes BEFORE graphs lose minimums.
    // cap = screen - 5 (2 chrome + 3 table stub) - graphs_min.
    assert_eq!(journal_pane_h(30, "ten", 20), 5); // cap 5: truncated, honest
    assert_eq!(journal_pane_h(26, "four", 18), 3); // cap 3: truncated
    // below two honest rows the pane hides rather than show a stub
    assert_eq!(journal_pane_h(24, "four", 17), 2); // cap 2: last honest size
    assert_eq!(journal_pane_h(24, "four", 18), 0); // cap 1 -> hide
    assert_eq!(journal_pane_h(24, "ten", 22), 0); // cap would be negative -> hide
}

#[test]
fn j_cycle_walks_four_ten_full_and_wraps_to_four() {
    // from the dashboard the tier itself is the state: every press advances
    assert_eq!(journal_next("off", false), ("four", false));
    assert_eq!(journal_next("four", false), ("ten", false));
    // "ten" opens the full-screen journal AND parks the pane at four behind it
    assert_eq!(journal_next("ten", false), ("four", true));
    // pressing j ON the full screen closes it and restarts the cycle at four
    assert_eq!(journal_next("four", true), ("four", false));
    assert_eq!(journal_next("ten", true), ("four", false));
    assert_eq!(journal_next("off", true), ("four", false));
}

#[test]
fn journal_option_validated_and_persisted() {
    let dir = std::env::temp_dir().join(format!("lsmtop-jp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cfg.json");
    std::fs::write(&path, r#"{"config_version":3,"targets":[]}"#).unwrap();
    let mut cfg = Config::load(Some(&path)).unwrap();
    assert_eq!(cfg.metrics().journal, "off"); // default hides the pane
    cfg.set_metric_opts(&[("journal", "four")]).unwrap();
    assert_eq!(cfg.metrics().journal, "four");
    cfg.set_metric_opts(&[("journal", "ten")]).unwrap();
    cfg.save(&path).unwrap();
    let re = Config::load(Some(&path)).unwrap();
    assert_eq!(re.metrics().journal, "ten");
    // the retired tiers are REJECTED by the write gate...
    assert!(cfg.set_metric_opts(&[("journal", "single")]).is_err());
    assert!(cfg.set_metric_opts(&[("journal", "third")]).is_err());
    assert!(cfg.set_metric_opts(&[("journal", "half")]).is_err());
    // ...and an old config carrying them LOADS as the nearest successor,
    // never silently off
    for (old, new) in [("single", "four"), ("third", "ten"), ("half", "ten"), ("full", "ten")] {
        std::fs::write(
            &path,
            format!(r#"{{"config_version":3,"targets":[],"metrics":{{"journal":"{old}"}}}}"#),
        )
        .unwrap();
        let legacy = Config::load(Some(&path)).unwrap();
        assert_eq!(legacy.metrics().journal, new, "legacy {old}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pane_size_cycles_full_half_single() {
    // G1 hotkey law (UNCHANGED): *_size keys cycle full -> half -> single
    let dir = std::env::temp_dir().join(format!("lsmtop-jp2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cfg.json");
    std::fs::write(&path, r#"{"config_version":3,"targets":[]}"#).unwrap();
    let mut cfg = Config::load(Some(&path)).unwrap();
    assert_eq!(cfg.metrics().gen_size, "full");
    assert_eq!(cfg.cycle_metric("gen_size").unwrap(), "half");
    assert_eq!(cfg.cycle_metric("gen_size").unwrap(), "single");
    assert_eq!(cfg.cycle_metric("gen_size").unwrap(), "full");
    assert_eq!(cfg.toggle_metric("kv_graph").unwrap(), "off");
    assert_eq!(cfg.metric_value("kv_graph"), "off");
    assert!(cfg.cycle_metric("kv_graph").is_err());
    assert!(cfg.set_metric_opts(&[("gen_half", "on")]).is_err());
    std::fs::remove_dir_all(&dir).ok();
}
