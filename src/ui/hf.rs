//! M8.2: HF-CLI screen — pure reducer + widget (Python HfCliScreen parity:
//! paste link → list GGUFs → number/all + Download; token never echoed).
use crate::hf::{parse_hf_ref, TreeFile};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Widget},
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HfKey {
    Char(char),
    Backspace,
    Enter,
    Download,
    Esc,
    Other,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HfOut {
    Nothing,
    /// repo id to query via the tree API
    ListRepo(String),
    /// repo + explicit file(s) to download immediately
    Download(String, Vec<String>),
    Cancel,
}

#[derive(Debug, Clone)]
pub struct HfState {
    pub input: String,
    pub env_line: String,
    pub log: Vec<String>,
    pub listing: BTreeMap<usize, String>,
    pub repo: Option<String>,
}

impl HfState {
    pub fn new(env_line: String) -> Self {
        Self {
            input: String::new(),
            env_line,
            log: Vec::new(),
            listing: BTreeMap::new(),
            repo: None,
        }
    }
    pub fn push_log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > 2000 {
            self.log.remove(0);
        }
    }

    pub fn set_listing(&mut self, repo: &str, files: &[TreeFile]) {
        if files.is_empty() {
            self.push_log(format!("No .gguf files in {repo}."));
            return;
        }
        self.repo = Some(repo.to_string());
        self.listing = files
            .iter()
            .enumerate()
            .map(|(i, f)| (i + 1, f.path.clone()))
            .collect();
        self.push_log(format!("Found {} GGUF file(s) in {repo}:", files.len()));
        for (i, f) in files.iter().enumerate() {
            self.push_log(format!(" {:>3}  {}  {:.2} GB", i + 1, f.path, f.size as f64 / 1e9));
        }
        self.push_log("Type the number (or a name) in the box, then d = Download. 'all' = every GGUF.");
    }

    fn file_selection(&self) -> Vec<String> {
        let text = self.input.trim();
        if text.eq_ignore_ascii_case("all") {
            return self.listing.values().cloned().collect();
        }
        let mut out = Vec::new();
        for part in text.split(',').map(|p| p.trim()).filter(|p| !p.is_empty()) {
            if let Ok(n) = part.parse::<usize>() {
                if let Some(fp) = self.listing.get(&n) {
                    out.push(fp.clone());
                }
            } else {
                out.push(part.to_string());
            }
        }
        out
    }

    /// Pure state machine: returns what the app must DO next.
    pub fn apply(&mut self, k: HfKey) -> HfOut {
        match k {
            HfKey::Char(c) => {
                self.input.push(c);
                HfOut::Nothing
            }
            HfKey::Backspace => {
                self.input.pop();
                HfOut::Nothing
            }
            HfKey::Enter => match parse_hf_ref(&self.input) {
                Some((repo, Some(file))) => {
                    self.push_log(format!("Downloading {file} from {repo} …"));
                    HfOut::Download(repo, vec![file])
                }
                Some((repo, None)) => {
                    self.input.clear();
                    HfOut::ListRepo(repo)
                }
                None => {
                    if !self.input.trim().is_empty() {
                        self.push_log(format!(
                            "'{}' is not an HF link or org/repo id",
                            self.input.trim()
                        ));
                    }
                    HfOut::Nothing
                }
            },
            HfKey::Download => match &self.repo {
                None => {
                    self.push_log("list a repo first (paste link, Enter)");
                    HfOut::Nothing
                }
                Some(repo) => {
                    let files = self.file_selection();
                    if files.is_empty() {
                        self.push_log("pick a number, a name, or 'all' first");
                        return HfOut::Nothing;
                    }
                    self.input.clear();
                    HfOut::Download(repo.clone(), files)
                }
            },
            HfKey::Esc => HfOut::Cancel,
            HfKey::Other => HfOut::Nothing,
        }
    }
}

pub struct HfWidget<'a>(pub &'a HfState);

impl Widget for HfWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);
        let top = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(outer[0]);
        buf.set_string(
            top[0].x,
            top[0].y,
            self.0.env_line.clone(),
            Style::default().fg(Color::DarkGray),
        );
        let box_block = Block::default()
            .borders(Borders::ALL)
            .title(" HF link (repo page or …/resolve/…/file.gguf URL) or org/name ");
        let inner = box_block.inner(top[1]);
        box_block.render(top[1], buf);
        if inner.width > 0 {
            let shown: String = self.0.input.chars().take(inner.width as usize).collect();
            buf.set_string(inner.x, inner.y, shown, Style::default().fg(Color::White));
        }
        buf.set_string(
            top[2].x + 1,
            top[2].y,
            " File — name, list #, or 'all'  (blank = list .gguf files) · d = Download:",
            Style::default(),
        );
        let log_block = Block::default().borders(Borders::ALL).title(" hf log ");
        let linner = log_block.inner(top[3]);
        log_block.render(top[2], buf);
        let start = self.0.log.len().saturating_sub(linner.height as usize);
        for (row, line) in self.0.log[start..].iter().enumerate() {
            if row as u16 >= linner.height {
                break;
            }
            let s: String = line.chars().take(linner.width as usize).collect();
            buf.set_string(linner.x, linner.y + row as u16, s, Style::default());
        }
        buf.set_string(
            outer[1].x,
            outer[1].y,
            "Enter List · d Download · backspace Edit · esc Back",
            Style::default().add_modifier(Modifier::BOLD),
        );
    }
}
