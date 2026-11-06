//! M7.5: live Metrics-options screen state (chips; v9.15 doctrine: all values
//! visible, direct pick, fill visibly disabled while chars == line, live
//! apply, Save persists).
use crate::config::Config;
use crate::ui::chips::ChipRow;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    widgets::Widget,
    text::Line,
};

/// R2/R3: the options strip leads with the back action (consistent mouse
/// target); there is no Save mechanic — chips live-save.
pub const OPTS_FOOTER: &str =
    " esc Back \u{b7} \u{2191}\u{2193} row \u{b7} \u{2190}\u{2192} value \u{b7} changes save live ";

pub const ROW_KEYS: [&str; 14] = [
    "chars",
    "fill",
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
    pub sel: usize,
}

impl OptsState {
    pub fn from_config(cfg: &Config) -> Self {
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
        let mut s = Self { rows, sel: 0 };
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

    /// fill is dead while chars == line — show it, never hide it (v9.15)
    pub fn sync_disabled(&mut self) {
        let line = self.value("chars") == "line";
        if let Some(row) = self.rows.iter_mut().find(|r| r.key == "fill") {
            row.disabled = line;
        }
    }

    pub fn up(&mut self) {
        self.sel = (self.sel + self.rows.len() - 1) % self.rows.len();
    }
    pub fn down(&mut self) {
        self.sel = (self.sel + 1) % self.rows.len();
    }
    /// live-applies immediately (graphs change under the screen); Save only
    /// persists. Disabled rows refuse input.
    pub fn cycle(&mut self, dir: isize) {
        let n = self.rows[self.sel].values.len() as isize;
        if self.rows[self.sel].disabled {
            return;
        }
        self.rows[self.sel].active =
            (((self.rows[self.sel].active as isize + dir).rem_euclid(n)) as usize)
                .min(self.rows[self.sel].values.len() - 1);
        self.sync_disabled();
    }

    /// Persist through the config module (0600, unknown-key preserving).
    /// Path is explicit — tests must never touch the user's real config.
    pub fn save(&self, cfg: &mut Config, path: &std::path::Path) -> Result<(), String> {
        let pairs: Vec<(&str, String)> =
            self.rows.iter().map(|r| (r.key, self.value(r.key))).collect();
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        cfg.set_metric_opts(&refs).map_err(|e| e.to_string())?;
        cfg.save(path).map_err(|e| e.to_string())
    }
}

pub struct OptsWidget<'a>(pub &'a OptsState);

impl Widget for OptsWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [Constraint::Length(1)]
                    .into_iter()
                    .chain(std::iter::repeat(Constraint::Length(2)).take(self.0.rows.len()))
                    .chain([Constraint::Min(0), Constraint::Length(1)])
                    .collect::<Vec<_>>(),
            )
            .split(area);
        buf.set_string(
            chunks[0].x,
            chunks[0].y,
            " Settings > Metrics — chips save live \u{b7} back with esc or q ",
            Style::default().add_modifier(Modifier::BOLD),
        );
        for (i, row) in self.0.rows.iter().enumerate() {
            let inner = chunks[1 + i];
            let style = if i == self.0.sel {
                Style::default().add_modifier(Modifier::BOLD).fg(ratatui::style::Color::Cyan)
            } else {
                Style::default()
            };
            // keyboard cursor (up/down) — visible focus, nvtop-chip parity
            buf.set_string(
                inner.x,
                inner.y,
                format!("{}{:>9}: ", if i == self.0.sel { "\u{25b8}" } else { " " }, row.key),
                style,
            );
            // chips start after the 12-col label; key suppressed inside the
            // row (it was painted twice before: "chars: chars  [...]")
            let chips_area = Rect {
                x: inner.x + 12,
                width: inner.width.saturating_sub(12),
                ..inner
            };
            let bare = ChipRow { key: "", ..row.clone() };
            bare.render(chips_area, buf);
        }
        let footer = chunks[1 + self.0.rows.len()]; // B5: 9 rows now
        let _ = Line::from("");
        if !footer.is_empty() {
            buf.set_string(
                footer.x,
                footer.y,
                OPTS_FOOTER,
                Style::default(),
            );
        }
    }
}
