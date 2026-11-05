//! M7: service management. `systemctl_show` / `systemctl_commands` are a
//! direct port of app.py:351-379 (READ-ONLY oracle). House doctrine: the TUI
//! house doctrine: the TUI READS unit state unprivileged and PRINTS the exact
//! start/stop commands a human must run — it never sudo's itself. User-scope
//! units are the ones it may act on directly.
pub mod logs;
pub mod owned;
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Indirection for process execution so unit tests never touch real systemd.
pub trait CmdRunner {
    /// Run argv, return stdout on success; Err(message) on spawn failure,
    /// timeout, or read error. Non-zero exit still yields stdout (systemctl
    /// `show` answers with key=value text either way).
    fn run(&self, argv: &[String], timeout: Duration) -> Result<String, String>;
}

/// Production runner: real processes, hard timeout via poll+kill.
pub struct RealRunner;

impl CmdRunner for RealRunner {
    fn run(&self, argv: &[String], timeout: Duration) -> Result<String, String> {
        if argv.is_empty() {
            return Err("empty argv".into());
        }
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(_status) => break,
                None => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err("timeout".into());
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        let mut buf = Vec::new();
        if let Some(out) = child.stdout.as_mut() {
            out.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

/// Test runner: scripted stdout answers + recorded argv. Never spawns.
#[derive(Default)]
pub struct MockRunner {
    script: std::cell::RefCell<VecDeque<String>>,
    fail: std::cell::RefCell<Option<String>>,
    log: std::cell::RefCell<Vec<Vec<String>>>,
}

impl MockRunner {
    pub fn script(&mut self, q: VecDeque<String>) {
        self.script.borrow_mut().extend(q);
    }
    pub fn fail_next(&mut self, msg: &str) {
        *self.fail.borrow_mut() = Some(msg.to_string());
    }
    pub fn calls(&self) -> usize {
        self.log.borrow().len()
    }
    pub fn last_argv(&self) -> Vec<String> {
        self.log.borrow().last().cloned().unwrap_or_default()
    }
}

impl CmdRunner for MockRunner {
    fn run(&self, argv: &[String], _timeout: Duration) -> Result<String, String> {
        self.log.borrow_mut().push(argv.to_vec());
        if let Some(msg) = self.fail.borrow_mut().take() {
            return Err(msg);
        }
        self.script
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| "mock: empty script".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct UnitInfo {
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub main_pid: Option<u64>,
    pub start_ts: Option<String>,
}

/// Read-only unit state; works unprivileged. Empty unit or no systemd →
/// Err(message), never a panic. // port: app.py:351 systemctl_show
pub fn systemctl_show(
    runner: &dyn CmdRunner,
    unit: &str,
    scope: &str,
    timeout: Duration,
) -> Result<UnitInfo, String> {
    if unit.is_empty() {
        return Err("no unit configured".into());
    }
    let mut argv: Vec<String> = vec!["systemctl".into()];
    if scope == "user" {
        argv.push("--user".into());
    }
    argv.push("show".into());
    argv.push(unit.into());
    for p in [
        "LoadState",
        "ActiveState",
        "SubState",
        "MainPID",
        "ExecMainStartTimestamp",
    ] {
        argv.push("-p".into());
        argv.push(p.into());
    }
    let out = runner.run(&argv, timeout).map_err(|_| {
        // port: app.py:366 — any spawn/exec failure maps to this message
        "systemctl unavailable on this host".to_string()
    })?;

    let mut kv: Vec<(String, String)> = Vec::new();
    for line in out.lines() {
        if let Some((k, v)) = line.split_once('=') {
            kv.push((k.to_string(), v.trim().to_string()));
        }
    }
    let get = |want: &str| -> String {
        kv.iter()
            .find(|(k, _)| k == want)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let load_state = get("LoadState");
    let active_state = get("ActiveState");
    if load_state.is_empty() || load_state == "not-found" || load_state == "masked"
        || active_state.is_empty()
    {
        // port: app.py:373 not-found semantics
        return Err(format!("unit '{unit}' not found on this host"));
    }
    Ok(UnitInfo {
        main_pid: get("MainPID").parse::<u64>().ok(),
        start_ts: Some(get("ExecMainStartTimestamp")).filter(|s| !s.is_empty()),
        load_state,
        active_state,
        sub_state: get("SubState"),
    })
}

/// The exact commands a human must run — the TUI prints them, never sudo's.
/// // port: app.py:378 systemctl_commands
pub fn systemctl_commands(unit: &str, scope: &str) -> Vec<String> {
    let pre = if scope == "user" {
        "systemctl --user "
    } else {
        "sudo systemctl "
    };
    ["status", "start", "stop", "restart"]
        .iter()
        .map(|verb| format!("{pre}{verb} {unit}"))
        .collect()
}
