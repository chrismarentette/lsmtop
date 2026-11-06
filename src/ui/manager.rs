//! M7.4: manager screen — per-target panels (port of ManagerScreen's grid of
//! ServerPanel/RouterPanel). House doctrine: for system-scope units the panel
//! PRINTS the exact command for a human; the TUI never sudo's.
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Widget},
};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum ActionPlan {
    /// Run directly (user-scope systemctl argv — no privileges needed).
    ExecUnit(Vec<String>),
    /// Operate the TUI-owned child process.
    OwnedStart,
    OwnedStop,
    /// Print this for the human; execute nothing (system scope = hand-off).
    Handoff(String),
    /// Say nothing happened and why.
    Bark(String),
}

/// Pure action routing — no process is spawned here, ever.
pub fn plan_action(target: &Value, verb: &str, owned_running: bool) -> ActionPlan {
    let unit = target
        .get("unit")
        .and_then(Value::as_str)
        .unwrap_or("");
    let scope = target
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("system");
    if !unit.is_empty() {
        return match scope {
            "user" => ActionPlan::ExecUnit(vec![
                "systemctl".into(),
                "--user".into(),
                verb.into(),
                unit.into(),
            ]),
            _ => ActionPlan::Handoff(format!("sudo systemctl {verb} {unit}")),
        };
    }
    // owned server target
    match verb {
        "start" => {
            let has_model = target
                .get("flags")
                .and_then(|f| f.get("model"))
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if has_model {
                ActionPlan::OwnedStart
            } else {
                ActionPlan::Bark("no model configured for this target".into())
            }
        }
        "stop" => {
            if owned_running {
                ActionPlan::OwnedStop
            } else {
                ActionPlan::Bark("nothing running".into())
            }
        }
        _ => ActionPlan::Bark("owned targets support start/stop only".into()),
    }
}

/// The manager footer, shared by the widget and the mouse hit-map so the
/// clickable tokens always sit on the exact painted text.
pub fn footer_text(has_server: bool) -> &'static str {
    // R2: back sits bottom-left; R7: q is Back (no quit token in the strip —
    // the app quits from the metrics root, Q only where a side lane exists)
    if has_server {
        "esc Back  s Start  x Stop  r Refresh  u HUP  c cmds  h HF  S Settings  Q Quit+Stop"
    } else {
        "esc Back  r Refresh  u HUP  c cmds  h HF  S Settings"
    }
}

/// Router lane lines straight from /models — every lane with its status and
/// child port, exactly as the router reports it.
pub fn router_lane_lines(models: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(data) = models.get("data").and_then(Value::as_array) else {
        return out;
    };
    for m in data {
        let id = m.get("id").and_then(Value::as_str).unwrap_or("?");
        let status = m
            .get("status")
            .and_then(|s| s.get("value"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        let port = m
            .get("status")
            .and_then(|s| s.get("args"))
            .and_then(Value::as_array)
            .and_then(|a| {
                a.iter()
                    .position(|x| x.as_str() == Some("--port"))
                    .and_then(|p| a.get(p + 1))
                    .and_then(Value::as_str)
            });
        out.push(match port {
            Some(p) if status == "loaded" => format!("{id}  {status}  :{p}"),
            _ => format!("{id}  {status}"),
        });
    }
    out
}

/// Cursor-safe text writer (no closure-capture fights over `y`).
fn put(
    buf: &mut Buffer,
    x: u16,
    y: &mut u16,
    max_y: u16,
    s: String,
    st: Style,
) {
    if *y < max_y {
        buf.set_string(x, *y, s, st);
        *y += 1;
    }
}

pub struct PanelInfo<'a> {
    pub unit_state: Option<String>, // rendered state line for unit targets
    pub owned_running: bool,
    pub router_lines: Vec<String>,
    pub sample_error: Option<&'a str>,
}

pub struct ManagerWidget<'a> {
    pub targets: &'a [Value],
    pub infos: Vec<PanelInfo<'a>>,
    pub selected: usize,
    /// side-lane launcher keys (s/x, Q) exist only when a kind:"server"
    /// target exists — router-only configs must not advertise them (maintainer)
    pub has_server: bool,
}

impl Widget for ManagerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // pinned footer (small-window law): always the LAST row
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);
        let foot = footer_text(self.has_server);
        buf.set_string(outer[1].x, outer[1].y, foot, Style::default().add_modifier(Modifier::BOLD));

        let shown = self.targets.len().min(3);
        let mut constraints: Vec<Constraint> = (0..shown.max(1))
            .map(|_| Constraint::Fill(1))
            .collect();
        if self.targets.len() > 3 {
            constraints.push(Constraint::Length(9));
        }
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(outer[0]);

        if self.targets.is_empty() {
            buf.set_string(
                outer[0].x,
                outer[0].y,
                "no targets configured (~/.lsmtop/config.json)",
                Style::default().fg(Color::Yellow),
            );
            return;
        }

        for (i, t) in self.targets.iter().enumerate().take(3) {
            let name = t.get("name").and_then(Value::as_str).unwrap_or("target");
            let kind = t.get("kind").and_then(Value::as_str).unwrap_or("server");
            let unit = t.get("unit").and_then(Value::as_str).unwrap_or("");
            let scope = t.get("scope").and_then(Value::as_str).unwrap_or("system");
            let sel = i == self.selected;
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" {name} "))
                .border_style(if sel {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                });
            let inner = block.inner(cols[i]);
            block.render(cols[i], buf);
            let info = self.infos.get(i);
            let mut y = inner.y;
            let max_y = inner.y + inner.height;
            put(buf, inner.x, &mut y, max_y, kind.to_string(), Style::default().add_modifier(Modifier::DIM));
            if !unit.is_empty() {
                let state = info
                    .and_then(|i| i.unit_state.clone())
                    .unwrap_or_else(|| "state: ?".into());
                let st = if state.contains("active") && !state.contains("inactive") {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                put(buf, inner.x, &mut y, max_y, format!("unit {unit} ({scope})"), Style::default());
                put(buf, inner.x, &mut y, max_y, state, st);
            } else if kind == "router" {
                put(buf, inner.x, &mut y, max_y, "API-only (no unit)".to_string(), Style::default().fg(Color::Blue));
            } else {
                let run = info.map(|i| i.owned_running).unwrap_or(false);
                put(
                    buf,
                    inner.x,
                    &mut y,
                    max_y,
                    if run { "owned: running" } else { "owned: stopped" }.to_string(),
                    if run {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                );
            }
            // action row : the
            // manager must READ like a console, not a bare bullet list.
            put(
                buf,
                inner.x,
                &mut y,
                max_y,
                " \u{25a4} Metrics(g)  \u{2261} Journal(j)  \u{27f3} HUP(u)  \u{29c9} cmds(c)  \u{21ba} Refresh(r)"
                    .to_string(),
                Style::default().fg(Color::Cyan),
            );
            put(buf, inner.x, &mut y, max_y, " LANES".to_string(), Style::default().add_modifier(Modifier::BOLD));
            let budget = max_y.saturating_sub(y) as usize;
            if let Some(lines) = info.map(|i| i.router_lines.clone()) {
                for l in lines.into_iter().take(budget) {
                    put(buf, inner.x, &mut y, max_y, format!(" \u{b7} {l}"), Style::default());
                }
            }
            if let Some(err) = info.and_then(|i| i.sample_error) {
                put(buf, inner.x, &mut y, max_y, format!("\u{26a0} {err}"), Style::default().fg(Color::Red));
            }
        }
        if self.targets.len() > 3 {
            buf.set_string(
                cols[3].x + 1,
                cols[3].y + 1,
                format!("+{} more", self.targets.len() - 3),
                Style::default().add_modifier(Modifier::DIM),
            );
        }
    }
}

/// Banner line for the metrics screen footer: current target + hint keys.
pub fn metrics_hints(target_name: &str, polls: u64) -> Line<'static> {
    Line::from(format!(
        " {target_name} · polls {polls} · a Lanes  l Journal  h HF-CLI  s Settings  g Llama  q Quit  Q Quit+Side"
    ))
}
