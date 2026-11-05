//! M7.2: TUI-owned llama-server subprocess (port of app.py ServiceProcess).
use lsmtop::svc::owned::{build_args, build_start_cmd, OwnedProcess};
use serde_json::json;

#[test]
fn build_args_maps_structured_flags_exactly() {
    // port parity: app.py:401 _build_args
    let f = json!({
        "ctx_size": 8192, "gpu_layers": 99, "flash_attn": true,
        "threads": 8, "parallel": 2, "mlock": true, "no_mmap": true,
        "extra_args": ""
    });
    let a = build_args(&f);
    assert_eq!(
        a,
        vec![
            "-c", "8192", "-ngl", "99", "--flash-attn", "on", "--threads", "8",
            "--parallel", "2", "--mlock", "--no-mmap",
        ]
    );
    // zero/None fields emit nothing; gpu_layers=0 is VALID (emit 0), null emits nothing
    let f2 = json!({"gpu_layers": 0});
    assert_eq!(build_args(&f2), vec!["-ngl", "0"]);
    let f3 = json!({"ctx_size": 0, "gpu_layers": null, "threads": 0, "parallel": 0});
    assert!(build_args(&f3).is_empty());
}

#[test]
fn extra_args_cannot_override_tui_managed_flags() {
    // Python keeps managed flags authoritative: duplicates in extra_args are
    // dropped WITH their values; unmanaged extras survive in order.
    let f = json!({
        "ctx_size": 4096, "flash_attn": true,
        "extra_args": "--alias my --ctx-size 1234 -c 99 --flash-attn --mlock --rope-scale 2.0"
    });
    let a = build_args(&f);
    assert_eq!(a[0..2], ["-c", "4096"]);
    // simpler explicit asserts
    assert!(a.contains(&"--alias".to_string()));
    assert!(a.contains(&"my".to_string()));
    assert!(a.contains(&"--rope-scale".to_string()));
    assert!(a.contains(&"2.0".to_string()));
    assert!(!a.iter().any(|x| x == "1234" || x == "99"), "managed dup leaked: {a:?}");
    // ours appears exactly once (with value "on"); the extra's bare switch AND
    // any trailing value were stripped
    let fa_count = a.iter().filter(|x| x.as_str() == "--flash-attn").count();
    assert_eq!(fa_count, 1, "{a:?}");
    assert!(a.contains(&"on".to_string()), "{a:?}");
}

#[test]
fn start_cmd_shape_matches_python() {
    let f = json!({"ctx_size": 1024});
    let cmd = build_start_cmd("/usr/bin/llama-server", "127.0.0.1", 8080, "/models/x.gguf", &f);
    assert_eq!(cmd[0], "/usr/bin/llama-server");
    assert_eq!(
        cmd[1..7],
        ["--host", "127.0.0.1", "--port", "8080", "--model", "/models/x.gguf"]
    );
    assert_eq!(&cmd[7..], &["-c", "1024"]);
}

#[test]
fn spawn_capture_stop_roundtrip() {
    let dir = std::path::PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()));
    let mut p = OwnedProcess::new(7, "test-lane", 0, String::new(), json!({}), dir.clone());
    // owned child writes through to the per-target log file
    p.start_raw(vec!["/bin/sh".into(), "-c".into(), "echo svc-hello-from-child".into()])
        .expect("spawn");
    assert!(p.wait_gone(std::time::Duration::from_secs(3)), "child should exit");
    // log header + child output land in svc-7.log
    let log = std::fs::read_to_string(dir.join("svc-7.log")).expect("log file");
    assert!(log.contains("svc-hello-from-child"), "{log}");
    assert!(log.contains("START") && log.contains("test-lane"), "{log}");
    p.stop();
    assert!(!p.is_running());
    let _ = std::fs::remove_file(dir.join("svc-7.log"));
}

#[test]
fn stop_terminates_a_live_child_within_grace() {
    let dir = std::path::PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()));
    let mut p = OwnedProcess::new(8, "sleeper", 0, String::new(), json!({}), dir.clone());
    p.start_raw(vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()])
        .expect("spawn sleeper");
    assert!(p.is_running());
    let t = std::time::Instant::now();
    p.stop(); // TERM, 6 s grace, KILL — Python parity
    assert!(!p.is_running());
    assert!(t.elapsed() < std::time::Duration::from_secs(3), "TERM should be prompt for sh");
}

#[test]
fn attach_fake_pid_is_not_running() {
    let mut p = OwnedProcess::new(
        9,
        "ghost",
        0,
        String::new(),
        json!({}),
        std::path::PathBuf::from("/tmp"),
    );
    p.attach(4_000_000_000); // certainly not alive
    assert!(!p.is_running()); // stale attach self-clears
    assert_eq!(p.pid(), None);
}
