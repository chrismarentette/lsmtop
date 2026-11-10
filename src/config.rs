use anyhow::Result;
use serde_json::{json, Value};
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub const GRAPH_MODES: [(&str, &[&str]); 17] = [
    ("chars", &["braille", "block", "line"]),
    ("fill", &["bar", "area", "line"]),
    // where the three-column scale ruler lives (maintainer 2026-11-07): right
    // is the default; both draws it on both edges; off removes ruler AND
    // the live value that rides it.
    ("scale", &["right", "left", "both", "off"]),
    // chars=line speaks its OWN fill vocabulary (maintainer 2026-11-07):
    // off (default, pure line) / vert (vertical rules) / horiz (horizontal rules)
    ("fill_line", &["off", "vert", "horiz"]),
    ("colors", &["gradient", "flow", "solid"]),
    ("timebase", &["scroll", "squish"]),
    ("ramp", &["heat", "spectrum", "ice"]),
    // per-graph visibility (Shift+letter hotkeys + Settings chips)
    ("gen_graph", &["on", "off"]),
    ("prompt_graph", &["on", "off"]),
    ("kv_graph", &["on", "off"]),
    ("gpu_graph", &["on", "off"]),
    // G1: per-graph display size replaces the old *_half booleans —
    // single = one-line sparkline + metrics text, half/full = weighted
    // panels. Journal joins the same size ladder (off hides it).
    ("gen_size", &["full", "half", "single"]),
    ("prompt_size", &["full", "half", "single"]),
    ("kv_size", &["full", "half", "single"]),
    ("gpu_size", &["full", "half", "single"]),
    ("journal", &["off", "four", "ten"]),
    ("lanes", &["on", "all", "off"]),
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
        write!(f, "Config({:?})", self.raw)
    }
}

// ───────────────────────── tick rate (metrics cadence) ─────────────────────────
// One collector poll per tick. Bounds keep the engine scrapes and the curve
// math in their realistic band; the grid is 50 ms so [ + ] / [ - ] steps land
// on clean values. Config may carry a number OR a numeric string — a
// hand-written `"2000"` means 2000; junk or absence falls back to default.
pub const TICK_MIN_MS: u32 = 500;
pub const TICK_MAX_MS: u32 = 5000;
pub const TICK_STEP_MS: u32 = 50;
pub const TICK_DEFAULT_MS: u32 = 2000;

/// Snap any raw value onto the tick grid: clamp to the bounds, then round to
/// the nearest 50 (a stored 2037 loads as 2050, never a rogue 2037).
pub fn normalize_tick_ms(raw: u32) -> u32 {
    let snapped = ((raw + TICK_STEP_MS / 2) / TICK_STEP_MS) * TICK_STEP_MS;
    snapped.clamp(TICK_MIN_MS, TICK_MAX_MS)
}

/// One 50 ms step, clamped — returns None when the bound already holds so
/// callers can skip pointless saves/respawns.
pub fn tick_step_from(cur: u32, dir: isize) -> Option<u32> {
    let next = (cur as i64 + (dir as i64) * TICK_STEP_MS as i64).clamp(TICK_MIN_MS as i64, TICK_MAX_MS as i64) as u32;
    if next == cur {
        None
    } else {
        Some(next)
    }
}

pub struct MetricsView {
    pub chars: String,
    pub fill: String,
    /// underfill while chars == line: off | vert | horiz (default off)
    pub fill_line: String,
    pub colors: String,
    pub timebase: String,
    pub ramp: String,
    pub gen_graph: String,
    pub prompt_graph: String,
    pub kv_graph: String,
    pub gpu_graph: String,
    pub gen_size: String,
    pub prompt_size: String,
    pub kv_size: String,
    pub gpu_size: String,
    pub journal: String,
    /// lanes TABLE mode: on = active only, all = every model, off = hide
    /// the pane (L cycles it; Settings chip lanes on/all/off).
    pub lanes: String,
    /// scale ruler position: right (default) | left | both | off
    pub scale: String,
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
                        "port": 9931,
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
            fill: validate_mode("fill", get_str("fill"), "bar"),
            fill_line: validate_mode("fill_line", get_str("fill_line"), "off"),
            colors: validate_mode("colors", get_str("colors"), "gradient"),
            timebase: validate_mode("timebase", get_str("timebase"), "scroll"),
            ramp: validate_mode("ramp", get_str("ramp"), "heat"),
            gen_graph: validate_mode("gen_graph", get_str("gen_graph"), "on"),
            prompt_graph: validate_mode("prompt_graph", get_str("prompt_graph"), "on"),
            kv_graph: validate_mode("kv_graph", get_str("kv_graph"), "on"),
            gpu_graph: validate_mode("gpu_graph", get_str("gpu_graph"), "on"),
            gen_size: validate_mode("gen_size", get_str("gen_size"), "full"),
            prompt_size: validate_mode("prompt_size", get_str("prompt_size"), "full"),
            kv_size: validate_mode("kv_size", get_str("kv_size"), "full"),
            gpu_size: validate_mode("gpu_size", get_str("gpu_size"), "full"),
            journal: {
                // Fixed-row tiers (four/ten display rows). Retired values
                // LOAD as their nearest successor — never silently off:
                // single -> four; third/half/full -> ten. Full-height
                // journal is a SCREEN (the j-cycle opens it), never a tier.
                let j = match get_str("journal") {
                    Some("single") => Some("four"),
                    Some("third") | Some("half") | Some("full") => Some("ten"),
                    other => other,
                };
                validate_mode("journal", j, "off")
            },
            lanes: validate_mode("lanes", get_str("lanes"), "on"),
            scale: validate_mode("scale", get_str("scale"), "right"),
        }
    }

    /// Collector cadence in ms (validated + snapped; never panics on junk).
    pub fn tick_ms(&self) -> u32 {
        let m = self.raw.get("metrics").and_then(|v| v.as_object());
        let raw = m
            .and_then(|m| m.get("tick_ms"))
            .and_then(|v| match v {
                Value::Number(n) => n.as_u64().map(|n| n as u32),
                // numeric strings are valid too (hand-editable config)
                Value::String(s) => s.trim().parse::<u32>().ok(),
                _ => None,
            });
        raw.map(normalize_tick_ms).unwrap_or(TICK_DEFAULT_MS)
    }

    /// Set the tick rate (snapped); returns the value actually stored.
    pub fn set_tick_ms(&mut self, v: u32) -> u32 {
        let v = normalize_tick_ms(v);
        if let Some(obj) = self.raw.as_object_mut() {
            let metrics = obj
                .entry("metrics")
                .or_insert_with(|| serde_json::json!({}));
            if !metrics.is_object() {
                *metrics = serde_json::json!({});
            }
            metrics
                .as_object_mut()
                .unwrap()
                .insert("tick_ms".to_string(), json!(v));
        }
        v
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

    /// One getter for every metrics key (visibility on/off, pane sizes,
    /// style chips) — hotkeys and chips share this view of the truth.
    pub fn metric_value(&self, key: &str) -> String {
        let m = self.metrics();
        match key {
            "gen_graph" => m.gen_graph,
            "prompt_graph" => m.prompt_graph,
            "kv_graph" => m.kv_graph,
            "gpu_graph" => m.gpu_graph,
            "gen_size" => m.gen_size,
            "prompt_size" => m.prompt_size,
            "kv_size" => m.kv_size,
            "gpu_size" => m.gpu_size,
            "journal" => m.journal,
            "lanes" => m.lanes,
            "scale" => m.scale,
            "chars" => m.chars,
            "fill" => m.fill,
            "fill_line" => m.fill_line,
            "colors" => m.colors,
            "timebase" => m.timebase,
            "ramp" => m.ramp,
            _ => String::new(),
        }
    }

    /// Toggle one on/off metric key (visibility chips + Shift+letter).
    pub fn toggle_metric(&mut self, key: &str) -> Result<String, String> {
        let cur = self.metric_value(key);
        let next = if cur == "on" { "off" } else { "on" };
        self.set_metric_opts(&[(key, next)])?;
        Ok(next.to_string())
    }

    /// G1: cycle a per-pane size full -> half -> single -> full.
    pub fn cycle_metric(&mut self, key: &str) -> Result<String, String> {
        if !key.ends_with("_size") {
            return Err(format!("not a size key: {key}"));
        }
        let cur = self.metric_value(key);
        let next = match cur.as_str() {
            "full" => "half",
            "half" => "single",
            _ => "full",
        };
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

/// UNIFIED KEY LAW (maintainer 2026-11-07): lowercase hotkeys cycle a pane's
/// visible SIZE and never pass through off; SHIFT+hotkey toggles
/// VISIBILITY. Graphs obey it (p/g/v/u sizes, P/G/V/U visibility); journal
/// (j four->ten->full, J on/off) and lanes follow the same law.
///
/// l law (lanes table SIZE): off -> on (turn the pane back), on <-> all
/// (active lanes only vs every model). The cycle skips off.
pub fn lanes_size_next(cur: &str) -> &'static str {
    match cur {
        "on" => "all",
        _ => "on", // "all" -> on; off/anything -> on (size never parks on off)
    }
}

/// L law (lanes table VISIBILITY): anything -> off; off -> the last size
/// seen (`last`: "all" stays "all", otherwise "on"). Mirrors the journal J
/// rule; both states live in metrics.lanes, so a press is durable.
pub fn lanes_vis_next(cur: &str, last: &str) -> &'static str {
    if cur == "off" {
        if last == "all" { "all" } else { "on" }
    } else {
        "off"
    }
}

/// The j-cycle law: four -> ten -> FULL-SCREEN journal -> four. "off" is
/// entered by the Settings chip, never a cycle stop; pressing j ON the
/// full-screen journal closes it and wraps the cycle back to four (no
/// "esc Back" needed). Returns (next tier, open the full screen?).
pub fn journal_next(cur: &str, on_full_screen: bool) -> (&'static str, bool) {
    if on_full_screen {
        return ("four", false);
    }
    match cur {
        "four" => ("ten", false),
        "ten" => ("four", true),
        _ => ("four", false), // off, or anything unexpected
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
