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
    assert_eq!(cfg.targets().len(), 1);
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
fn hf_token_handling() {
    let path = scratch_path("test_hf_token.json");
    let raw = json!({
        "hf_token": "secret123"
    });
    std::fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

    let cfg = Config::load(Some(&path)).unwrap();
    assert_eq!(cfg.hf_token(), Some("secret123"));
    let debug_str = format!("{:?}", cfg);
    assert!(!debug_str.contains("secret123"));
    assert!(debug_str.contains("[set]"));
}

#[test]
fn targets_view() {
    let cfg = Config::load(Some("/tmp/nonexistent.json")).unwrap();
    let targets = cfg.targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0]["kind"], "server");
    assert_eq!(targets[0]["port"], 8080);

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
