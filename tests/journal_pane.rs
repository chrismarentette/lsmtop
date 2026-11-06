//! Journal pane sizing law (tiers off/single/third + scaling-priority)
//! with validated option plumbing. The single tier rides the newest
//! entry whole — its wrapped row count (1-2) sizes the pane.
//! Full-height journal is a SCREEN the j-cycle opens, never a persisted
//! pane tier.
use lsmtop::config::Config;
use lsmtop::ui::dashboard::journal_pane_h;
use lsmtop::ui::dashboard::journal_single_rows;
use ratatui::text::Line;

#[test]
fn pane_size_law() {
    // off (or unknown) never takes space
    assert_eq!(journal_pane_h(50, "off", 12, 2), 0);
    assert_eq!(journal_pane_h(50, "bogus", 12, 2), 0);
    // small windows keep the classic layout untouched (small-window law)
    assert_eq!(journal_pane_h(23, "third", 12, 2), 0);
    assert_eq!(journal_pane_h(15, "single", 12, 1), 0);
    // single = the newest line's wrapped rows: 1 short, 2 long
    assert_eq!(journal_pane_h(50, "single", 12, 1), 1);
    assert_eq!(journal_pane_h(50, "single", 12, 2), 2);
    // third = a third of the screen on roomy bodies
    assert_eq!(journal_pane_h(40, "third", 12, 1), 13);
    assert_eq!(journal_pane_h(50, "third", 12, 1), 16);
    // scaling-priority: the journal flexes BEFORE graphs lose minimums.
    // cap = screen - 5 (2 chrome + 3 table stub) - graphs_min.
    assert_eq!(journal_pane_h(30, "third", 20, 1), 5); // cap 5: still honest
    assert_eq!(journal_pane_h(28, "third", 20, 1), 0); // cap 3 < 4 -> hide
    assert_eq!(journal_pane_h(26, "third", 12, 1), 8); // third=8 fits under cap 9
    // single survives the tightest bodies (it only ever needs 1-2 rows)
    assert_eq!(journal_pane_h(24, "single", 16, 2), 2);
    assert_eq!(journal_pane_h(24, "single", 18, 2), 1); // cap 1: one honest row
    assert_eq!(journal_pane_h(24, "single", 19, 2), 0); // cap 0 -> hide
}

#[test]
fn single_tier_measures_the_newest_line() {
    // short line -> one row; a line that soft-wraps at the pane width
    // -> two rows so the WHOLE log line stays readable.
    let short = Line::from("short line");
    assert_eq!(journal_single_rows(None, 60), 1);
    assert_eq!(journal_single_rows(Some(&short), 60), 1);
    let long = Line::from("x".repeat(200));
    assert_eq!(journal_single_rows(Some(&long), 60), 2); // capped at 2
    // a line that fits needs no second row
    let fits = Line::from("y".repeat(59));
    assert_eq!(journal_single_rows(Some(&fits), 60), 1);
}

#[test]
fn journal_option_validated_and_persisted() {
    let dir = std::env::temp_dir().join(format!("lsmtop-jp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cfg.json");
    std::fs::write(&path, r#"{"config_version":3,"targets":[]}"#).unwrap();
    let mut cfg = Config::load(Some(&path)).unwrap();
    assert_eq!(cfg.metrics().journal, "off"); // default hides the pane
    cfg.set_metric_opts(&[("journal", "third")]).unwrap();
    assert_eq!(cfg.metrics().journal, "third");
    // the retired pane tiers are rejected by the gate...
    assert!(cfg.set_metric_opts(&[("journal", "half")]).is_err());
    assert!(cfg.set_metric_opts(&[("journal", "fifth")]).is_err());
    cfg.save(&path).unwrap();
    let re = Config::load(Some(&path)).unwrap();
    assert_eq!(re.metrics().journal, "third");
    // ...and old configs carrying them LOAD as third, not off
    std::fs::write(
        &path,
        r#"{"config_version":3,"targets":[],"metrics":{"journal":"half"}}"#,
    )
    .unwrap();
    let legacy = Config::load(Some(&path)).unwrap();
    assert_eq!(legacy.metrics().journal, "third");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pane_size_cycles_full_half_single() {
    // G1 hotkey law: *_size keys cycle full -> half -> single -> full
    let dir = std::env::temp_dir().join(format!("lsmtop-jp2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cfg.json");
    std::fs::write(&path, r#"{"config_version":3,"targets":[]}"#).unwrap();
    let mut cfg = Config::load(Some(&path)).unwrap();
    assert_eq!(cfg.metrics().gen_size, "full"); // default shows the big panel
    assert_eq!(cfg.cycle_metric("gen_size").unwrap(), "half");
    assert_eq!(cfg.cycle_metric("gen_size").unwrap(), "single");
    assert_eq!(cfg.cycle_metric("gen_size").unwrap(), "full");
    // visibility keys still toggle; gates stay keyed (no cross-cycling)
    assert_eq!(cfg.toggle_metric("kv_graph").unwrap(), "off");
    assert_eq!(cfg.metric_value("kv_graph"), "off");
    assert!(cfg.cycle_metric("kv_graph").is_err());
    // legacy *_half keys are gone from the validated set
    assert!(cfg.set_metric_opts(&[("gen_half", "on")]).is_err());
    std::fs::remove_dir_all(&dir).ok();
}
