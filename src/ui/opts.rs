//! M7.5: Metrics-options state (chips). Doctrine: all values visible,
//! direct pick, LIVE apply (every change persists the moment it
//! happens). v9.31 (maintainer 2026-11-07): fill is NEVER disabled — chars ==
//! line speaks its own fill vocabulary `off vert horiz` (default off;
//! vert/horiz fill with the line alphabet) while braille/block speak
//! `bar area line`. The rows render inside Settings.
use crate::config::Config;
use crate::ui::chips::ChipRow;

pub const ROW_KEYS: [&str; 16] = [
    "chars",
    "fill",
    // where the scale ruler lives (right | left | both | off) — right
    // after fill so the graph-shape knobs stay together (maintainer 2026-11-07)
    "scale",
    "colors",
    "timebase",
    "ramp",
    "gen_graph",
    "gen_size",
    "prompt_graph",
    "prompt_size",
    "kv_graph",
    "kv_size",
    "gpu_graph",
    "gpu_size",
    "journal",
    "lanes",
];

#[rustfmt::skip]
pub fn row_values(key: &str) -> Vec<&'static str> {
    // ONE source of truth: GRAPH_MODES feeds the chips exactly like
    // GRAPH_MODES feeds Settings + the accept-list (v9.10 lesson).
    crate::config::GRAPH_MODES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_vec())
        .unwrap_or_default()
}

pub struct OptsState {
    pub rows: Vec<ChipRow<'static>>,
    /// fill values kept per chars-vocabulary so toggling chars never
    /// clobbers the other mode's choice
    pub fill_bb: String,
    pub fill_line: String,
}

/// the two fill vocabularies (disjoint word sets — membership infers
/// which slot a chip value belongs to, no stale bookkeeping possible)
pub const FILL_BB: [&str; 3] = ["bar", "area", "line"];
pub const FILL_LINE: [&str; 3] = ["off", "vert", "horiz"];

impl OptsState {
    pub fn from_config(cfg: &Config) -> Self {
        let fill_bb = cfg.metric_value("fill");
        let fill_line = cfg.metric_value("fill_line");
        let rows = ROW_KEYS
            .iter()
            .map(|k| {
                let vals = row_values(k);
                // Config::metric_value is the ONE getter (chips, hotkeys,
                // and the dashboard all read the same truth).
                let cur = cfg.metric_value(k);
                let active = vals.iter().position(|v| *v == cur).unwrap_or(0);
                ChipRow::new(k, vals, active)
            })
            .collect();
        let mut s = Self {
            rows,
            fill_bb,
            fill_line,
        };
        s.sync_disabled();
        s
}

    pub fn value(&self, key: &str) -> String {
        self.rows
            .iter()
            .find(|r| r.key == key)
            .map(|r| r.values[r.active].to_string())
            .unwrap_or_default()
    }

    /// v9.31: chars == line does NOT disable fill — it swaps the fill
    /// row to its own vocabulary (off default / vert / horiz). Words are
    /// disjoint between vocabularies, so the chip's current value tells
    /// us which slot to record before rebuilding.
    pub fn sync_disabled(&mut self) {
        if let Some(row) = self.rows.iter().find(|r| r.key == "fill") {
            let v = row.values[row.active.min(row.values.len() - 1)];
            if FILL_LINE.contains(&v) {
                self.fill_line = v.to_string();
            } else {
                self.fill_bb = v.to_string();
            }
        }
        let line = self.value("chars") == "line";
        let (vals, want) = if line {
            (FILL_LINE.to_vec(), self.fill_line.clone())
        } else {
            (FILL_BB.to_vec(), self.fill_bb.clone())
        };
        if let Some(row) = self.rows.iter_mut().find(|r| r.key == "fill") {
            row.values = vals;
            row.active = row
                .values
                .iter()
                .position(|v| *v == want)
                .unwrap_or(if line { 0 } else { 1 });
            row.disabled = false;
        }
    }

    /// Persist through the config module (0600, unknown-key preserving).
    /// Path is explicit — tests must never touch the user's real config.
    pub fn save(&self, cfg: &mut Config, path: &std::path::Path) -> Result<(), String> {
        // the fill chip writes to the key of the vocabulary it speaks
        let pairs: Vec<(&str, String)> = self
            .rows
            .iter()
            .map(|r| {
                let v = self.value(r.key);
                let key = if r.key == "fill" {
                    if FILL_LINE.contains(&v.as_str()) {
                        "fill_line"
                    } else {
                        "fill"
                    }
                } else {
                    r.key
                };
                (key, v)
            })
            .collect();
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        cfg.set_metric_opts(&refs).map_err(|e| e.to_string())?;
        cfg.save(path).map_err(|e| e.to_string())
    }
}
