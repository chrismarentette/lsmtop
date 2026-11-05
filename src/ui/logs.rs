//! M7.5: live log viewer (port of LogScreen). 'journal' = journalctl -f of a
//! unit; 'file' = tail of a TUI-owned lane log. Read-only: Clear empties the
//! VIEWER, never the journal.
use crate::svc::logs::engine_line_spans;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::collections::VecDeque;
use std::path::PathBuf;

pub const MAX_LINES: usize = 2000;
pub const INITIAL_TAIL: usize = 300;

#[derive(Debug, Clone, PartialEq)]
pub enum LogSource {
    Journal { name: String, unit: String, scope: String },
    OwnedFile { name: String, port: u16, path: PathBuf },
}

pub struct LogState {
    pub source: LogSource,
    pub lines: VecDeque<Line<'static>>,
    /// rows scrolled back from the live bottom (0 = pinned to newest)
    pub scroll: usize,
}

impl LogState {
    pub fn new(source: LogSource) -> Self {
        let mut s = Self { source, lines: VecDeque::new(), scroll: 0 };
        if let LogSource::OwnedFile { path, .. } = &s.source {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    for l in crate::svc::logs::tail_lines(&text, INITIAL_TAIL) {
                        s.push_line(&l);
                    }
                }
                Err(_) => s.push_plain("No log file yet — start the service to see output."),
            }
        }
        s
    }

    pub fn banner(&self) -> String {
        match &self.source {
            LogSource::Journal { name, unit, .. } => {
                format!(" {name} — journal: {unit} — live ")
            }
            LogSource::OwnedFile { name, port, .. } => {
                format!(" {name}  |  :{port}  |  Live Log ")
            }
        }
    }

    pub fn push_line(&mut self, line: &str) {
        let styled = styled_line(line);
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(styled);
        // live bottom follows new content (viewer scroll-up persists)
    }

    pub fn push_plain(&mut self, line: &str) {
        if self.lines.len() >= MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(Line::from(line.to_string()));
    }

    pub fn clear_view(&mut self) {
        self.lines.clear();
        self.scroll = 0;
    }

    pub fn scroll_by(&mut self, delta: isize) {
        // positive delta = toward older lines (scroll up away from live)
        let max = self.lines.len().saturating_sub(1);
        self.scroll = ((self.scroll as isize + delta).clamp(0, max as isize)) as usize;
    }
}

/// Port of colorize_engine_line: explicit pattern spans only — arbitrary log
/// content can never inject styles (no markup parsing).
pub fn styled_line(line: &str) -> Line<'static> {
    let spans = engine_line_spans(line);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut pos = 0usize;
    for (a, b, style) in spans {
        let a = a.max(pos).min(line.len());
        let b = b.max(a).min(line.len());
        if a > pos {
            out.push(Span::styled(
                line[pos..a].to_string(),
                Style::default(),
            ));
        }
        if b > a {
            out.push(Span::styled(
                line[a..b].to_string(),
                style.to_rate(),
            ));
        }
        pos = b;
    }
    if pos < line.len() {
        out.push(Span::styled(line[pos..].to_string(), Style::default()));
    }
    if out.is_empty() {
        out.push(Span::styled(String::new(), Style::default()));
    }
    Line::from(out)
}

pub struct LogsWidget<'a>(pub &'a LogState);

impl Widget for LogsWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);
        buf.set_string(
            chunks[0].x,
            chunks[0].y,
            self.0.banner(),
            Style::default().add_modifier(Modifier::BOLD),
        );
        let body = Block::default().borders(Borders::ALL).title(" q/esc back · c clear view ");
        let inner = body.inner(chunks[1]);
        body.render(chunks[1], buf);
        let h = inner.height as usize;
        let w = inner.width as usize;
        let len = self.0.lines.len();
        let end = len.saturating_sub(self.0.scroll);
        let start = end.saturating_sub(h);
        for (row, i) in (start..end).enumerate() {
            if let Some(line) = self.0.lines.get(i) {
                let mut col = 0usize;
                for sp in &line.spans {
                    if col >= w {
                        break;
                    }
                    let s: String = sp.content.chars().take(w - col).collect();
                    if s.is_empty() {
                        continue;
                    }
                    let n = s.chars().count();
                    buf.set_string(inner.x + col as u16, inner.y + row as u16, s, sp.style);
                    col += n;
                }
            }
        }
    }
}
