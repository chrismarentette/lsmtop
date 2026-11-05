use anyhow::Result;
use serde_json::{json, Value};
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub const GRAPH_MODES: [(&str, &[&str]); 13] = [
    ("chars", &["braille", "block", "line"]),
    ("fill", &["line", "bar", "area"]),
    ("colors", &["gradient", "flow", "solid"]),
    ("timebase", &["scroll", "squish"]),
    ("ramp", &["heat", "spectrum", "ice"]),
    // B5: per-graph visibility (Settings chips + Shift+letter hotkeys)...
    ("gen_graph", &["on", "off"]),
    ("prompt_graph", &["on", "off"]),
    ("kv_graph", &["on", "off"]),
    ("gpu_graph", &["on", "off"]),
    // ...and per-graph half-size (remembered; letter hotkey toggles)
    ("gen_half", &["on", "off"]),
    ("prompt_half", &["on", "off"]),
    ("kv_half", &["on", "off"]),
    ("gpu_half", &["on", "off"]),
];

fn validate_mode(key: &str, val: Option<&str>, default: &str) -> String {
    let allowed = GRAPH_MODES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .unwrap_or(&[][..]);
    if let Some(v) = val {
        if allowed.contains(&v) {
            return v.to_string();
        }
    }
    default.to_string()
}

#[derive(Clone)]
pub struct Config {
    pub(crate) raw: Value,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut cloned = self.raw.clone();
        if let Some(obj) = cloned.as_object_mut() {
            if obj.contains_key("hf_token") {
                obj.insert("hf_token".to_string(), json!("[set]"));
            }
        }
        write!(f, "{:?}", cloned)
    }
}

pub struct MetricsView {
    pub chars: String,
    pub fill: String,
    pub colors: String,
    pub timebase: String,
    pub ramp: String,
    pub gen_graph: String,
    pub prompt_graph: String,
    pub kv_graph: String,
    pub gpu_graph: String,
    pub gen_half: String,
    pub prompt_half: String,
    pub kv_half: String,
    pub gpu_half: String,
}

impl Config {
    pub fn default_path() -> std::path::PathBuf {
        if let Ok(env_path) = std::env::var("LSMTOP_CONFIG") {
            std::path::PathBuf::from(env_path)
        } else {
            let mut p = std::env::var("HOME").ok().map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("."));
            // same file as the app (dual-run): ~/.lsmtop/config.json
            p.push(".lsmtop");
            p.push("config.json");
            p
        }
    }


    pub fn load<P: AsRef<Path>>(path: Option<P>) -> Result<Self> {
        let resolved: Option<std::path::PathBuf> = match path {
            Some(p) => Some(p.as_ref().to_path_buf()),
            None => {
                let d = Self::default_path();
                if d.exists() {
                    Some(d)
                } else {
                    None
                }
            }
        };
        let mut raw = if let Some(p) = resolved {
            if p.is_file() {
                let content = fs::read_to_string(&p)?;
                serde_json::from_str(&content).unwrap_or_else(|_| json!({}))
            } else {
                json!({})
            }
        } else {
            json!({})
        };

        if !raw.is_object() {
            raw = json!({});
        }

        let obj = raw.as_object_mut().unwrap();

        if !obj.contains_key("targets")
            || !obj["targets"].is_array()
            || obj["targets"].as_array().unwrap().is_empty()
        {
            obj.insert(
                "targets".to_string(),
                json!([
                    {
                        "kind": "server",
                        "port": 8080,
                        "host": "127.0.0.1",
                        "name": "Server 0"
                    }
                ]),
            );
        }

        let metrics = obj.entry("metrics").or_insert_with(|| json!({}));
        if !metrics.is_object() {
            *metrics = json!({});
        }

        Ok(Config { raw })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut to_save = self.raw.clone();
        if let Some(obj) = to_save.as_object_mut() {
            obj.insert("config_version".to_string(), json!(3));
        }

        let content = serde_json::to_string_pretty(&to_save)?;
        fs::write(path, content)?;

        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
        Ok(())
    }

    pub fn metrics(&self) -> MetricsView {
        let m = self.raw.get("metrics").and_then(|v| v.as_object());
        let get_str = |key: &str| m.and_then(|m| m.get(key)).and_then(|v| v.as_str());

        MetricsView {
            chars: validate_mode("chars", get_str("chars"), "braille"),
            fill: validate_mode("fill", get_str("fill"), "line"),
            colors: validate_mode("colors", get_str("colors"), "gradient"),
            timebase: validate_mode("timebase", get_str("timebase"), "scroll"),
            ramp: validate_mode("ramp", get_str("ramp"), "heat"),
            gen_graph: validate_mode("gen_graph", get_str("gen_graph"), "on"),
            prompt_graph: validate_mode("prompt_graph", get_str("prompt_graph"), "on"),
            kv_graph: validate_mode("kv_graph", get_str("kv_graph"), "on"),
            gpu_graph: validate_mode("gpu_graph", get_str("gpu_graph"), "on"),
            gen_half: validate_mode("gen_half", get_str("gen_half"), "off"),
            prompt_half: validate_mode("prompt_half", get_str("prompt_half"), "off"),
            kv_half: validate_mode("kv_half", get_str("kv_half"), "off"),
            gpu_half: validate_mode("gpu_half", get_str("gpu_half"), "off"),
        }
    }

    pub fn hf_token(&self) -> Option<&str> {
        self.raw.get("hf_token").and_then(|v| v.as_str())
    }

    /// Settings-screen edit: set a top-level string key.
    /// // port: save_config({**cfg, "llama_server_path": ...}) writes
    pub fn set_str(&mut self, key: &str, val: &str) {
        if let serde_json::Value::Object(obj) = &mut self.raw {
            obj.insert(key.to_string(), serde_json::json!(val));
        }
    }

    /// Write validated metric options (same GRAPH_MODES accept-list as
    /// load). Slice form so B5 per-graph keys ride the same gate.
    pub fn set_metric_opts(&mut self, want: &[(&str, &str)]) -> Result<(), String> {
        let root = self
            .raw
            .as_object_mut()
            .ok_or_else(|| "config root is not an object".to_string())?;
        let metrics = root
            .entry("metrics")
            .or_insert_with(|| serde_json::json!({}));
        if !metrics.is_object() {
            *metrics = serde_json::json!({});
        }
        let obj = metrics.as_object_mut().unwrap();
        for (k, v) in want {
            let allowed = GRAPH_MODES
                .iter()
                .find(|p| p.0 == *k)
                .map(|p| p.1)
                .ok_or_else(|| format!("unknown metric option {k}"))?;
            if !allowed.contains(v) {
                return Err(format!("invalid {k}={v} (allowed: {})", allowed.join(", ")));
            }
            obj.insert((*k).to_string(), serde_json::json!(v));
        }
        Ok(())
    }

    /// Toggle one validated metric key (B5 hotkeys persist through this).
    pub fn toggle_metric(&mut self, key: &str) -> Result<String, String> {
        let cur = self.metrics();
        let getv = |m: &MetricsView| match key {
            "gen_graph" => m.gen_graph.clone(),
            "prompt_graph" => m.prompt_graph.clone(),
            "kv_graph" => m.kv_graph.clone(),
            "gpu_graph" => m.gpu_graph.clone(),
            "gen_half" => m.gen_half.clone(),
            "prompt_half" => m.prompt_half.clone(),
            "kv_half" => m.kv_half.clone(),
            "gpu_half" => m.gpu_half.clone(),
            _ => String::new(),
        };
        let cur = getv(&cur);
        let next = if cur == "on" { "off" } else { "on" };
        self.set_metric_opts(&[(key, next)])?;
        Ok(next.to_string())
    }


    pub fn targets(&self) -> Vec<Value> {
        self.raw
            .get("targets")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    pub fn raw(&self) -> &Value {
        &self.raw
    }
}

pub fn router_owned_ports(targets: &[Value]) -> std::collections::HashSet<u16> {
    let mut ports = std::collections::HashSet::new();
    for t in targets {
        if t.get("kind").and_then(|v| v.as_str()) == Some("router") {
            if let Some(base_url) = t.get("base_url").and_then(|v| v.as_str()) {
                if let Ok(url) = reqwest::Url::parse(base_url) {
                    if let Some(port) = url.port_or_known_default() {
                        ports.insert(port);
                    }
                }
            }
        }
    }
    ports
}
