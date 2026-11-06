//! M8: HuggingFace features — HF-CLI screen core
//! (). Doctrine: we shell out to the REAL `hf` CLI,
//! the token travels ONLY via environment (never argv — invisible to ps), the
//! download is detached (survives TUI exit), and an absent CLI only barks the
//! install command (we never pip-install behind the user's back).
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub const TOKEN_FILE: &str = ".cache/huggingface/token"; // under $HOME

/// Token resolution order: lsmtop setting → HF_TOKEN env → the hub's own
/// token file (`hf auth login`). Never printed anywhere. Pure args → testable.
pub fn hf_token_of(
    cfg_token: Option<&str>,
    env_token: Option<&str>,
    token_file: Option<&Path>,
) -> String {
    if let Some(t) = cfg_token.map(str::trim).filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    if let Some(t) = env_token.map(str::trim).filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    if let Some(p) = token_file {
        if let Ok(s) = std::fs::read_to_string(p) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}

/// Convenience wrapper for the live app: config → $HF_TOKEN → ~/.cache/…
pub fn hf_token(cfg: &crate::config::Config) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    hf_token_of(
        cfg.hf_token(),
        std::env::var("HF_TOKEN").ok().as_deref(),
        Some(&PathBuf::from(home).join(TOKEN_FILE)),
    )
}

fn is_exe(p: &Path) -> bool {
    p.is_file()
        && std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

fn which_in(path_var: Option<&str>, name: &str) -> Option<String> {
    for dir in path_var?.split(':') {
        let cand = PathBuf::from(dir).join(name);
        if is_exe(&cand) {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

/// The ACTUAL HF CLI: `hf` (huggingface_hub ≥1.0) or legacy
/// `huggingface-cli`; own-dir candidates first ,
/// then PATH. None → the HF screen barks the install command.
pub fn hf_cli_path_from(own_dir: Option<&Path>, path_var: Option<&str>) -> Option<String> {
    if let Some(dir) = own_dir {
        for name in ["hf", "huggingface-cli"] {
            let cand = dir.join(name);
            if is_exe(&cand) {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    for name in ["hf", "huggingface-cli"] {
        if let Some(p) = which_in(path_var, name) {
            return Some(p);
        }
    }
    None
}

pub fn hf_cli_path() -> Option<String> {
    let own = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let path = std::env::var("PATH").ok();
    hf_cli_path_from(own.as_deref(), path.as_deref())
}

/// Paste an HF link or 'org/repo' → (repo, filename|None). Accepts
/// https://huggingface.co|hf.co/[/models|/datasets|/spaces/]/org/repo,
/// /resolve/<rev>/path/to/file.gguf, bare repo ids, 'org/repo/file'
/// shorthand, trailing '/' and '?query'. (direct port )
pub fn parse_hf_ref(text: &str) -> Option<(String, Option<String>)> {
    let t = text
        .trim()
        .split('?')
        .next()
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('/')
        .to_string();
    if t.is_empty() {
        return None;
    }
    let rest = if t.starts_with("http://") || t.starts_with("https://") {
        let after = t.split("://").nth(1).unwrap_or("");
        after.split_once('/').map(|(_, r)| r.to_string()).unwrap_or_default()
    } else {
        t.trim_matches('/').to_string()
    };
    let mut parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    if parts.first().is_some_and(|p| *p == "models" || *p == "datasets" || *p == "spaces") {
        parts = parts[1..].to_vec();
    }
    if parts.len() < 2 {
        return None;
    }
    let repo = format!("{}/{}", parts[0], parts[1]);
    let filename = if parts.len() >= 4 && parts[2] == "resolve" {
        let f = parts[4..].join("/");
        (!f.is_empty()).then_some(f)
    } else if parts.len() > 2 {
        let f = parts[2..].join("/");
        (!f.is_empty()).then_some(f)
    } else {
        None
    };
    Some((repo, filename))
}

/// Properly-named folder under the models dir (repo-name segment). A
/// .lsmtop-hf-repo marker inside remembers the repo id — a DIFFERENT org
/// reusing the name gets an org-prefixed dir so both survive.
pub fn hf_target_dir(models_dir: &str, repo: &str) -> PathBuf {
    let seg = repo.rsplit('/').next().unwrap_or("");
    let safe: String = seg
        .chars()
        .map(|c| if c.is_alphanumeric() || "-._".contains(c) { c } else { '-' })
        .collect::<String>()
        .trim_matches(|c| c == '-' || c == '.')
        .to_string();
    let safe = if safe.is_empty() { "repo".to_string() } else { safe };
    let base = PathBuf::from(shellexpand_home(models_dir));
    let mut d = base.join(&safe);
    let marker = d.join(".lsmtop-hf-repo");
    if d.is_dir() {
        if let Ok(s) = std::fs::read_to_string(&marker) {
            if s.trim() != repo {
                let org = repo.split('/').next().unwrap_or("");
                d = base.join(format!("{org}-{safe}"));
            }
        }
    }
    d
}

fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~") {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}{rest}")
    } else {
        p.to_string()
    }
}

/// Hub tree API endpoint (sizes included, recursive — GGUFs live in
/// subfolders).
pub fn repo_tree_url(repo: &str) -> String {
    format!("https://huggingface.co/api/models/{repo}/tree/main?recursive=true")
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeFile {
    pub path: String,
    pub size: u64,
}

/// Keep only .gguf entries (case-insensitive); missing size = 0.
pub fn parse_gguf_tree(v: &Value) -> Vec<TreeFile> {
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for f in arr {
            let path = f.get("path").and_then(Value::as_str).unwrap_or("");
            if path.to_lowercase().ends_with(".gguf") {
                // int(f.get("size") or 0) — numeric strings ok
                let size = match f.get("size") {
                    Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
                    Some(Value::String(s)) => s.parse().unwrap_or(0),
                    _ => 0,
                };
                out.push(TreeFile { path: path.to_string(), size });
            }
        }
    }
    out
}

/// Status-message mapping for the tree call (exact failure doctrine).
pub fn tree_status_hint(code: u16) -> Option<&'static str> {
    match code {
        401 => Some("401 — private repo: set a token in Settings \u{b7} HuggingFace (or run `hf auth login`)."),
        404 => Some("404 — no such repo (or gated: needs a token + accepted licence on HF)."),
        _ => None,
    }
}

/// `hf download <repo> <files…> --local-dir <models>/<repo-name>` — the
/// token NEVER appears here.
pub fn build_download_cmd(cli: &str, repo: &str, files: &[String], dest: &Path) -> Vec<String> {
    let mut cmd = vec![cli.to_string(), "download".to_string(), repo.to_string()];
    cmd.extend(files.iter().cloned());
    cmd.push("--local-dir".to_string());
    cmd.push(dest.to_string_lossy().into_owned());
    cmd
}

pub fn download_log_path(dir: &Path, repo: &str) -> PathBuf {
    dir.join(format!("{}.log", repo.replace('/', "_")))
}

/// Detached download spawn: output appended to the log file, new process
/// group (survives TUI exit), token via env ONLY.
pub fn spawn_download(
    cmd: &[String],
    dest: &Path,
    log_path: &Path,
    token: Option<&str>,
) -> io::Result<Child> {
    let logf = std::fs::OpenOptions::new().create(true).append(true).open(log_path)?;
    let logf2 = logf.try_clone()?;
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]);
    c.current_dir(dest);
    c.stdout(Stdio::from(logf));
    c.stderr(Stdio::from(logf2));
    c.process_group(0); // detach — start_new_session parity
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        c.env("HF_TOKEN", t);
    }
    c.spawn()
}

/// File-box parser (): comma-separated parts; a number picks
/// from the listing, 'all' picks everything, anything else passes through
/// as a literal filename.
pub fn file_selection(listing: &BTreeMap<usize, String>, text: &str, want_all_flag: bool) -> Vec<String> {
    let text = text.trim();
    if want_all_flag || text.eq_ignore_ascii_case("all") {
        return listing.values().cloned().collect();
    }
    let mut files = Vec::new();
    for part in text.split(',').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        if let Ok(n) = part.parse::<usize>() {
            if let Some(fp) = listing.get(&n) {
                files.push(fp.clone());
            }
        } else {
            files.push(part.to_string());
        }
    }
    files
}

/// The absent-CLI bark (install command, never auto-install).
pub fn install_bark() -> String {
    "hf CLI not found — HF-CLI drives the real huggingface CLI. install: pip install -U 'huggingface_hub' \
     (hub >=1.0 ships `hf`; older shipped `huggingface-cli` — lsmtop finds either; restart lsmtop after installing)"
        .to_string()
}
