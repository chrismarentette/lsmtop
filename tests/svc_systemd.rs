//! M7.1: systemctl_show port + systemctl_commands parity.
//! Tests run against a MOCK CmdRunner only — unit tests must NEVER touch
//! real systemd units (house rule: service actions are hand-offs/tests-free).
use lsmtop::svc::{systemctl_commands, systemctl_show, MockRunner};
use std::collections::VecDeque;
use std::time::Duration;

#[test]
fn show_parses_key_value_output() {
    let mut m = MockRunner::default();
    m.script(VecDeque::from(vec![
        "LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=4242\n\
         ExecMainStartTimestamp=Tue 2026-11-03 20:00:00 UTC\n"
            .to_string(),
    ]));
    let info = systemctl_show(&m, "llama-server", "system", Duration::from_secs(5))
        .expect("ok unit parses");
    assert_eq!(info.load_state, "loaded");
    assert_eq!(info.active_state, "active");
    assert_eq!(info.sub_state, "running");
    assert_eq!(info.main_pid, Some(4242));
    // exact property list ported from app.py systemctl_show
    let argv = m.last_argv();
    assert_eq!(argv[0], "systemctl");
    for p in ["LoadState", "ActiveState", "SubState", "MainPID", "ExecMainStartTimestamp"] {
        assert!(argv.iter().any(|a| a == p), "missing -p {p}");
    }
}

#[test]
fn user_scope_adds_user_flag_before_show() {
    let mut m = MockRunner::default();
    m.script(VecDeque::from(vec![
        "LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0\n".to_string(),
    ]));
    let info = systemctl_show(&m, "llama-server", "user", Duration::from_secs(5)).unwrap();
    assert_eq!(info.active_state, "inactive");
    assert_eq!(info.main_pid, Some(0));
    let argv = m.last_argv();
    assert_eq!(argv[1], "--user");
    assert_eq!(argv[2], "show");
}

#[test]
fn empty_unit_errors_without_running_anything() {
    let m = MockRunner::default();
    let e = systemctl_show(&m, "", "system", Duration::from_secs(5)).unwrap_err();
    assert_eq!(e, "no unit configured");
    assert_eq!(m.calls(), 0);
}

#[test]
fn missing_systemctl_maps_to_unavailable() {
    let mut m = MockRunner::default();
    m.fail_next("No such file or directory (os error 2)");
    let e = systemctl_show(&m, "u", "system", Duration::from_secs(5)).unwrap_err();
    assert_eq!(e, "systemctl unavailable on this host");
}

#[test]
fn not_found_or_masked_units_error() {
    for ls in ["", "not-found", "masked"] {
        let mut m = MockRunner::default();
        m.script(VecDeque::from(vec![format!(
            "LoadState={ls}\nActiveState=inactive\nSubState=dead\nMainPID=0\n"
        )]));
        let e = systemctl_show(&m, "ghost", "system", Duration::from_secs(5)).unwrap_err();
        assert!(e.contains("unit 'ghost' not found on this host"), "{ls}: {e}");
    }
    // missing ActiveState entirely is also not-found (Python parity)
    let mut m = MockRunner::default();
    m.script(VecDeque::from(vec!["LoadState=loaded\n".to_string()]));
    let e = systemctl_show(&m, "x", "system", Duration::from_secs(5)).unwrap_err();
    assert!(e.contains("not found on this host"), "{e}");
}

#[test]
fn commands_are_hand_off_strings_never_executed() {
    let sys = systemctl_commands("llama", "system");
    assert_eq!(
        sys,
        vec![
            "sudo systemctl status llama",
            "sudo systemctl start llama",
            "sudo systemctl stop llama",
            "sudo systemctl restart llama",
        ]
    );
    let usr = systemctl_commands("llama", "user");
    assert_eq!(usr[1], "systemctl --user start llama");
}
