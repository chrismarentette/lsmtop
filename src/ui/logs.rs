//! M7.5: live log viewer (port of LogScreen). 'journal' = journalctl -f of a
//! unit; 'file' = tail of a TUI-owned lane log. Read-only: Clear empties the
//! VIEWER, never the journal.
use crate::svc::logs::engine_line_spans;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Widget,
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

/// R1: terminal-style soft wrap — a line longer than `width` folds onto
/// continuation rows instead of being chopped (journalctl lines are far
/// wider than the pane on 80-col screens). Styles ride each span; a chunk
/// split mid-span keeps the span's style on both rows. Char-count widths
/// (journal content is cell-width-1 text).
pub fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(Vec::new())];
    }
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for sp in &line.spans {
        let mut start = 0usize; // byte index into sp.content
        for (i, ch) in sp.content.char_indices() {
            if used == width {
                if i > start {
                    cur.push(Span::styled(sp.content[start..i].to_string(), sp.style));
                }
                rows.push(Line::from(std::mem::take(&mut cur)));
                used = 0;
                start = i;
            }
            used += 1;
            let _ = ch;
        }
        if start < sp.content.len() {
            cur.push(Span::styled(sp.content[start..].to_string(), sp.style));
        }
    }
    rows.push(Line::from(cur)); // the (possibly empty) final row
    rows
}

/// G1 single-tier journal ticker: paint the FIRST wrapped segment of one
/// styled line at (x, y) — the same fold law as wrap_line, first fold only.
pub fn render_line_head(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: usize,
    line: &Line<'static>,
) {
    if width == 0 {
        return;
    }
    let Some(seg) = wrap_line(line, width).into_iter().next() else {
        return;
    };
    let mut col = 0usize;
    for sp in &seg.spans {
        if col >= width {
            break;
        }
        let s: String = sp.content.chars().take(width - col).collect();
        if s.is_empty() {
            continue;
        }
        let n = s.chars().count();
        buf.set_string(x + col as u16, y, &s, sp.style);
        col += n;
    }
}

/// R1: render the tail window with wrapping. Scroll counts LOG LINES (wheel
/// steps stay stable when the terminal resizes); wrapped continuation rows
/// are pure display, and the bottom-pinned tail is assembled by walking
/// backward through wrapped rows until the pane is full.
pub fn render_tail(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    lines: &VecDeque<Line<'static>>,
    scroll: usize,
) {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return;
    }
    let end = lines.len().saturating_sub(scroll.min(lines.len()));
    let mut disp: Vec<Line<'static>> = Vec::new();
    let mut i = end;
    while i > 0 && disp.len() < h {
        i -= 1;
        for row in wrap_line(&lines[i], w).iter().rev() {
            disp.push(row.clone());
            if disp.len() >= h {
                break;
            }
        }
    }
    disp.reverse();
    for (row, ln) in disp.iter().enumerate() {
        let mut col = 0usize;
        for sp in &ln.spans {
            if col >= w {
                break;
            }
            let s: String = sp.content.chars().take(w - col).collect();
            if s.is_empty() {
                continue;
            }
            let n = s.chars().count();
            buf.set_string(x + col as u16, y + row as u16, s, sp.style);
            col += n;
        }
    }
}

pub struct LogsWidget<'a>(pub &'a LogState);

/// Full-screen journal: BARE rows edge to edge (maintainer 2026-11-07: the box
/// around everything wasted space in fullscreen; the title bar is the app
/// header, not a box title, and the Clear hint lives once in the bottom
/// strip). The widget paints only the tail window.
impl Widget for LogsWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_tail(buf, area.x, area.y, area.width, area.height, &self.0.lines, self.0.scroll);
    }
}

#[cfg(test)]
mod wrap_tests {
    //! R1 law: the journal wraps long lines like a terminal — nothing is
    //! chopped, styles survive the fold, and the bottom-pinned tail is
    //! measured in DISPLAY rows so the newest line is always fully visible.
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect, style::Color};

    fn txt(l: &Line<'static>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wrap_folds_long_lines_and_keeps_short_ones() {
        let l = Line::from("abcdefgh".to_string());
        let rows: Vec<String> = wrap_line(&l, 3).iter().map(txt).collect();
        assert_eq!(rows, vec!["abc", "def", "gh"]);
        assert_eq!(wrap_line(&Line::from("ab".to_string()), 5).len(), 1);
        assert_eq!(wrap_line(&Line::from(String::new()), 5).len(), 1);
    }

    #[test]
    fn wrap_keeps_span_styles_across_the_fold() {
        let l = Line::from(vec![
            Span::styled("aaaa".to_string(), Style::default().fg(Color::Red)),
            Span::styled("bb".to_string(), Style::default().fg(Color::Green)),
        ]);
        let rows = wrap_line(&l, 3);
        assert_eq!(rows.len(), 2);
        assert_eq!(txt(&rows[0]), "aaa");
        assert_eq!(txt(&rows[1]), "abb");
        // continuation row keeps the red chunk's style
        assert_eq!(rows[1].spans[0].style.fg, Some(Color::Red));
        assert_eq!(rows[1].spans[1].style.fg, Some(Color::Green));
    }

    fn journal(lines: &[&str]) -> LogState {
        let mut ls = LogState::new(LogSource::Journal {
            name: "t".into(),
            unit: "u".into(),
            scope: "system".into(),
        });
        for l in lines {
            ls.push_line(l);
        }
        ls
    }

    #[test]
    fn tail_window_wraps_and_bottom_pins() {
        // width 5 pane, height 4: each 10-char line folds to 2 rows, so the
        // visible tail is the last TWO lines, fully wrapped.
        let ls = journal(&["0123456789", "abcdefghij", "ABCDEFGHIJ"]);
        let area = Rect { x: 0, y: 0, width: 5, height: 4 };
        let mut buf = Buffer::empty(area);
        render_tail(&mut buf, 0, 0, 5, 4, &ls.lines, 0);
        let row = |r: u16| -> String {
            (0..5).map(|x| buf[(x, r)].symbol().to_string()).collect()
        };
        assert_eq!(row(0), "abcde");
        assert_eq!(row(1), "fghij");
        assert_eq!(row(2), "ABCDE");
        assert_eq!(row(3), "FGHIJ");
    }

    #[test]
    fn tail_window_scroll_counts_log_lines() {
        let ls = journal(&["0123456789", "abcdefghij", "ABCDEFGHIJ"]);
        let area = Rect { x: 0, y: 0, width: 5, height: 3 };
        let mut buf = Buffer::empty(area);
        // scroll 2 rows up the LINE list -> the first line is on screen
        render_tail(&mut buf, 0, 0, 5, 3, &ls.lines, 2);
        let row = |r: u16| -> String {
            (0..5).map(|x| buf[(x, r)].symbol().to_string()).collect()
        };
        assert_eq!(row(0), "01234");
        assert_eq!(row(1), "56789");
    }
}
