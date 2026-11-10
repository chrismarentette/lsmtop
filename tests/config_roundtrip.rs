use lsmtop::config::{router_owned_ports, Config};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn scratch_path(name: &str) -> PathBuf {
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(format!("{}/{}", dir, name))
}

#[test]
fn missing_file_yields_defaults() {
    let cfg = Config::load(Some("/tmp/nonexistent-lsmtop.json")).unwrap();
    assert_eq!(cfg.metrics().chars, "braille");
    assert_eq!(cfg.metrics().ramp, "heat");
    assert_eq!(cfg.metrics().colors, "gradient");
    assert_eq!(cfg.metrics().fill, "bar"); // bar is the natural default (maintainer 2026-11-07)
    assert_eq!(cfg.targets().len(), 1);
}

#[test]
fn fill_chip_order_is_bar_area_line() {
    let modes = lsmtop::config::GRAPH_MODES
        .iter()
        .find(|(k, _)| *k == "fill")
        .unwrap()
        .1;
    assert_eq!(modes, &["bar", "area", "line"]);
}

#[test]
fn save_preserves_unknown_keys() {
    let path = scratch_path("test_save_preserves.json");
    let raw = json!({
        "future_key": {"x": 1},
        "targets": []
    });
    std::fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

    let cfg = Config::load(Some(&path)).unwrap();
    cfg.save(&path).unwrap();

    let reloaded = Config::load(Some(&path)).unwrap();
    assert_eq!(reloaded.raw()["future_key"]["x"], 1);

    let perms = std::fs::metadata(&path).unwrap().permissions();
    assert_eq!(perms.mode() & 0o777, 0o600);
}

#[test]
fn mode_validation() {
    let path = scratch_path("test_mode_validation.json");
    let raw = json!({
        "metrics": {
            "colors": "bogus",
            "chars": "block",
        }
    });
    std::fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

    let cfg = Config::load(Some(&path)).unwrap();
    assert_eq!(cfg.metrics().colors, "gradient"); // fallback
    assert_eq!(cfg.metrics().chars, "block"); // valid
}

#[test]
fn tick_ms_loads_validates_and_snaps() {
    // number, numeric STRING (hand-editable config), junk, off-grid,
    // and out-of-bounds all land on the 50 ms grid inside 500..=5000
    let path = scratch_path("test_tick_ms.json");
    let cases: [(&str, u32); 7] = [
        ("2000", 2000),
        ("\"2000\"", 2000),
        ("100", 500),
        ("7000", 5000),
        ("2037", 2050),
        ("2024", 2000),
        ("\"abc\"", 2000),
    ];
    for (raw, want) in cases {
        let body = format!(r#"{{"metrics": {{"tick_ms": {raw}}}}}"#);
        std::fs::write(&path, body).unwrap();
        let cfg = Config::load(Some(&path)).unwrap();
        assert_eq!(cfg.tick_ms(), want, "raw {raw}");
    }
    // absent = default
    std::fs::write(&path, "{}").unwrap();
    let cfg = Config::load(Some(&path)).unwrap();
    assert_eq!(cfg.tick_ms(), lsmtop::config::TICK_DEFAULT_MS);
}

#[test]
fn tick_ms_setter_snaps_and_persists_as_number() {
    let path = scratch_path("test_tick_set.json");
    let _ = std::fs::remove_file(&path);
    let mut cfg = Config::load(Some(path.clone())).unwrap();
    assert_eq!(cfg.set_tick_ms(1234), 1250);
    cfg.save(&path).unwrap();
    let reloaded = Config::load(Some(path.clone())).unwrap();
    assert_eq!(reloaded.tick_ms(), 1250);
    assert!(reloaded.raw()["metrics"]["tick_ms"].is_number());
    std::fs::remove_file(&path).ok();
}

#[test]
fn targets_view() {
    let cfg = Config::load(Some("/tmp/nonexistent.json")).unwrap();
    let targets = cfg.targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["kind"], "server");
    assert_eq!(targets[0]["port"], 9931);

    let raw = json!({
        "targets": [
            { "kind": "server", "port": 8081 },
            { "kind": "router", "base_url": "http://127.0.0.1:8085" },
            { "kind": "router", "base_url": "http://127.0.0.1:8086" }
        ]
    });
    let ports = router_owned_ports(raw["targets"].as_array().unwrap());
    assert_eq!(ports.len(), 2);
    assert!(ports.contains(&8085));
    assert!(ports.contains(&8086));
}

#[test]
fn lanes_chip_mode_law() {
    // metrics.lanes: on (active only) | all | off — default on, junk -> on.
    let cases = [
        (r#"{"metrics":{"lanes":"all"}}"#, "all"),
        (r#"{"metrics":{"lanes":"off"}}"#, "off"),
        (r#"{"metrics":{"lanes":"on"}}"#, "on"),
        (r#"{"metrics":{"lanes":"sideways"}}"#, "on"),
    ];
    for (raw, want) in cases {
        let path = scratch_path("test_lanes_mode.json");
        std::fs::write(&path, raw).unwrap();
        let cfg = Config::load(Some(&path)).unwrap();
        assert_eq!(cfg.metrics().lanes, want, "raw={raw}");
    }
    let missing = Config::load(Some("/tmp/nonexistent-lsmtop-lanes.json")).unwrap();
    assert_eq!(missing.metrics().lanes, "on");
    // the chip write path round-trips
    let path = scratch_path("test_lanes_mode.json");
    std::fs::write(&path, "{}").unwrap();
    let mut cfg = Config::load(Some(&path)).unwrap();
    cfg.set_metric_opts(&[("lanes", "off")]).unwrap();
    assert_eq!(cfg.metrics().lanes, "off");
}

#[test]
fn lanes_unified_key_laws() {
    // l = SIZE, skips off entirely.
    use lsmtop::config::{lanes_size_next, lanes_vis_next};
    assert_eq!(lanes_size_next("on"), "all");
    assert_eq!(lanes_size_next("all"), "on");
    assert_eq!(lanes_size_next("off"), "on"); // size brings the pane back
    // L = VISIBILITY only, restores the last size.
    assert_eq!(lanes_vis_next("on", "on"), "off");
    assert_eq!(lanes_vis_next("all", "all"), "off");
    assert_eq!(lanes_vis_next("off", "all"), "all");
    assert_eq!(lanes_vis_next("off", "on"), "on");
    assert_eq!(lanes_vis_next("off", "junk"), "on");
}

#[test]
fn scale_default_right_and_vocab() {
    // maintainer 2026-11-07: the ruler lives RIGHT by default; the option is
    // right|left|both|off, nothing else loads.
    let cfg = Config::load(Some("/tmp/nonexistent-lsmtop.json")).unwrap();
    assert_eq!(cfg.metrics().scale, "right");
    assert_eq!(cfg.metric_value("scale"), "right");
    let modes = lsmtop::config::GRAPH_MODES
        .iter()
        .find(|(k, _)| *k == "scale")
        .unwrap()
        .1;
    assert_eq!(modes, &["right", "left", "both", "off"]);
    // junk loads as the default, never crashes
    let path = scratch_path("test_scale_validate.json");
    std::fs::write(&path, serde_json::json!({"metrics": {"scale": "middle"}}).to_string()).unwrap();
    let cfg = Config::load(Some(&path)).unwrap();
    assert_eq!(cfg.metrics().scale, "right");
}

#[test]
fn scale_save_roundtrip() {
    let path = scratch_path("test_scale_roundtrip.json");
    let mut cfg = Config::load(Some(&path)).unwrap();
    cfg.set_metric_opts(&[("scale", "both")]).unwrap();
    cfg.save(&path).unwrap();
    let re = Config::load(Some(&path)).unwrap();
    assert_eq!(re.metrics().scale, "both");
}
