//! M7.2: llama-server subprocess OWNED by the TUI. Port of app.py ServiceProcess
//! (:383-545). Only 'kind: server' targets use this; a systemd-managed router
//! is never process-owned here.
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Minimal shlex-style tokenizer (whitespace + single/double quotes).
/// // port: app.py:437 shlex.split(extra) — no shell metachars are honored,
/// only quoting, exactly what extra_args needs.
pub fn split_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for ch in s.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    has = true;
                }
                c if c.is_whitespace() => {
                    if has {
                        out.push(std::mem::take(&mut cur));
                    }
                    has = false;
                }
                c => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Build CLI args from the structured flags dict.
/// // port: app.py:401 ServiceProcess._build_args
pub fn build_args(flags: &Value) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let get = |k: &str| flags.get(k);
    let as_i64 = |v: &Value| -> Option<i64> {
        v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
    };
    if let Some(v) = get("ctx_size").and_then(as_i64) {
        if v > 0 {
            args.extend(["-c".to_string(), v.to_string()]);
        }
    }
    if let Some(ngl) = get("gpu_layers").filter(|v| !v.is_null()).and_then(as_i64) {
        if ngl >= 0 {
            args.extend(["-ngl".to_string(), ngl.to_string()]);
        }
    }
    if get("flash_attn").and_then(Value::as_bool).unwrap_or(false) {
        args.extend(["--flash-attn".to_string(), "on".to_string()]);
    }
    if let Some(v) = get("threads").and_then(as_i64) {
        if v > 0 {
            args.extend(["--threads".to_string(), v.to_string()]);
        }
    }
    if let Some(v) = get("parallel").and_then(as_i64) {
        if v > 0 {
            args.extend(["--parallel".to_string(), v.to_string()]);
        }
    }
    if get("mlock").and_then(Value::as_bool).unwrap_or(false) {
        args.push("--mlock".to_string());
    }
    if get("no_mmap").and_then(Value::as_bool).unwrap_or(false) {
        args.push("--no-mmap".to_string());
    }
    let extra = get("extra_args").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !extra.is_empty() {
        // TUI-managed flags stay authoritative; duplicates in extra_args are
        // dropped WITH their values. // port: app.py:438-452
        const MANAGED_WITH_VALUE: [&str; 5] =
            ["-c", "--ctx-size", "-ngl", "--threads", "--parallel"];
        const MANAGED_SWITCHES: [&str; 3] = ["--flash-attn", "--mlock", "--no-mmap"];
        let tokens = split_tokens(&extra);
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            if MANAGED_WITH_VALUE.contains(&t.as_str()) {
                i += 2;
                continue;
            }
            if MANAGED_SWITCHES.contains(&t.as_str()) {
                i += 1;
                if i < tokens.len() && !tokens[i].starts_with('-') {
                    i += 1;
                }
                continue;
            }
            args.push(t.clone());
            i += 1;
        }
    }
    args
}

/// // port: app.py:485-490 start() cmd assembly
pub fn build_start_cmd(
    server_bin: &str,
    host: &str,
    port: u16,
    model_path: &str,
    flags: &Value,
) -> Vec<String> {
    let mut cmd = vec![
        server_bin.to_string(),
        "--host".to_string(),
        host.to_string(),
        "--port".to_string(),
        port.to_string(),
        "--model".to_string(),
        model_path.to_string(),
    ];
    cmd.extend(build_args(flags));
    cmd
}

fn pid_alive(pid: u64) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Our own child counts as alive only while try_wait reports no exit status
/// (Ok(None)); Ok(Some(_)/Err) means exited — and try_wait reaps the zombie,
/// mirroring Python's poll() side effect.
fn child_exited(c: &mut Child) -> bool {
    matches!(c.try_wait(), Ok(Some(_)) | Err(_))
}

pub struct OwnedProcess {
    pub(crate) tid: u32,
    pub(crate) name: String,
    pub(crate) port: u16,
    pub(crate) host: String,
    pub(crate) flags: Value,
    pub(crate) log_dir: PathBuf,
    pub(crate) child: Option<Child>,
    // Interior-mutable so is_running() can self-clear a dead attach (Python
    // parity: app.py:458-463 clears _external_pid on probe).
    pub(crate) external_pid: std::cell::Cell<Option<u64>>,
}

impl OwnedProcess {
    pub fn new(
        tid: u32,
        name: &str,
        port: u16,
        host: String,
        flags: Value,
        log_dir: PathBuf,
    ) -> Self {
        Self {
            tid,
            name: name.to_string(),
            port,
            host,
            flags,
            log_dir,
            child: None,
            external_pid: std::cell::Cell::new(None),
        }
    }

    /// Attach to a llama-server started outside the TUI. // port: app.py:454
    pub fn attach(&mut self, pid: u64) {
        self.external_pid.set(Some(pid));
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(c) = self.child.as_mut() {
            // try_wait, NOT /proc: an exited child stays in /proc as a zombie
            // until reaped — /proc would call it alive forever.
            if !child_exited(c) {
                return true;
            }
        }
        if let Some(p) = self.external_pid.get() {
            if pid_alive(p) {
                return true;
            }
            self.external_pid.set(None); // stale attach self-clears
        }
        false
    }

    pub fn pid(&mut self) -> Option<u64> {
        if let Some(c) = self.child.as_mut() {
            if !child_exited(c) {
                return Some(c.id() as u64);
            }
        }
        self.external_pid.get().filter(|p| pid_alive(*p))
    }

    pub fn log_path(&self) -> PathBuf {
        self.log_dir.join(format!("svc-{}.log", self.tid))
    }

    fn spawn_argv(&mut self, argv: &[String], env: &[(String, String)]) -> std::io::Result<()> {
        if self.is_running() {
            self.stop(); // Python: start() stops a running instance first
        }
        std::fs::create_dir_all(&self.log_dir)?;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())?;
        let sep = "=".repeat(60);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        writeln!(
            log,
            "\n{sep}\n[ts={ts}] START  {}  port={}\nCMD: {}\n{sep}",
            self.name,
            self.port,
            argv.join(" ")
        )?;
        log.flush()?;
        let log_out = log.try_clone()?;
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdout(Stdio::from(log_out))
            .stderr(Stdio::from(log.try_clone()?))
            .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())));
        // Own session so a terminal death doesn't reap the server.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        self.child = Some(cmd.spawn()?);
        Ok(())
    }

    /// Spawn with the target's structured flags (generic env passthrough only —
    /// no hardware assumptions, flags.env wins). // port: app.py:473-498
    pub fn start(&mut self, server_bin: &str, model: &str) -> std::io::Result<()> {
        let host = if self.host.is_empty() {
            "127.0.0.1".to_string()
        } else {
            self.host.clone()
        };
        let cmd = build_start_cmd(server_bin, &host, self.port, model, &self.flags);
        let env: Vec<(String, String)> = self
            .flags
            .get("env")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        self.spawn_argv(&cmd, &env)
    }

    /// Test/advanced entry: spawn an exact argv.
    pub fn start_raw(&mut self, argv: Vec<String>) -> std::io::Result<()> {
        self.spawn_argv(&argv, &[])
    }

    /// Graceful app exit: the owned server OUTLIVES the viewer (own process
    /// group, Python parity) — forget the handle, do NOT kill.
    pub fn disown(&mut self) {
        let _ = self.child.take();
        self.external_pid.set(None);
    }

    /// TERM, wait up to 6 s, then KILL. // port: app.py:518-544 stop()
    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let pid = c.id().to_string();
            let _ = Command::new("kill").args(["-TERM", &pid]).status();
            let deadline = Instant::now() + Duration::from_secs(6);
            loop {
                match c.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = c.kill();
                            let _ = c.wait();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => break,
                }
            }
        }
        if let Some(p) = self.external_pid.take() {
            let pid = p.to_string();
            let _ = Command::new("kill").args(["-TERM", &pid]).status();
            for _ in 0..60 {
                if !pid_alive(p) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if pid_alive(p) {
                let _ = Command::new("kill").args(["-KILL", &pid]).status();
            }
        }
    }

    /// Poll until this process is gone (test helper). Must use is_running,
    /// not /proc: our exited child is a zombie until reaped.
    pub fn wait_gone(&mut self, max: Duration) -> bool {
        let deadline = Instant::now() + max;
        loop {
            if !self.is_running() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // An owned server must not outlive the TUI by accident.
        self.stop();
    }
}
