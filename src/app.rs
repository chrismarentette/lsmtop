use crossterm::event::KeyCode;
use crate::net::{ServerSample, RouterSample};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Metrics,
    Settings,
    Logs,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    Tick,
    Key(crossterm::event::KeyEvent),
    /// mouse events (earlier builds had full mouse; we must not lose it —
    /// maintainer: "Mouse support was previously integral")
    Mouse(crossterm::event::MouseEvent),
    Sample(usize, Result<SampleData, ScrapeError>),
    ScrapeError(String),
    LogLine(String),
    /// B4: journal lower pane stream (own tail process; separate so
    /// the full-screen journal and the pane never duplicate each other).
    JPaneLine(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SampleData {
    Server(ServerSample),
    Router(RouterSample),
}

/// What kind of llama endpoint an HTTP endpoint actually answers as —
/// decided by the PAYLOAD, never by the config label (users mislabel; a
/// bare llama-server behind kind:"router" must still be scraped right).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EngineMode {
    /// llama-swap style: data[] entries carry status.value + args
    Router,
    /// bare llama.cpp: OpenAI data[] without status
    Single,
    /// answered HTTP, but not a shape we know
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapeError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    GoBack,
    SwitchScreen(Screen),
    LanesSize,
    LanesVis,
    JournalToggle,
    /// G2: cycle the journal pane tier off -> four -> ten -> full screen,
    /// and on the full screen close it and wrap back to four
    JournalCycle,
    /// multi-server (2026-11-10): `t` cycles the ACTIVE target — the whole
    /// dashboard (banner, LANES, curves) repaints from that target's state.
    /// One-key-per-target; targets are config, lsmtop never manages them.
    NextTarget,
    Clear,
    /// Settings-screen ring navigation + chip presses
    SetEdit(SetKey),
    /// positive = scroll toward older lines
    Scroll(isize),
    Noop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetKey {
    NextField,
    PrevField,
    Char(char),
    /// enter / space: "press" whatever the ring is on — cycle a chip row
    /// forward, or fire the highlighted [ + ] / [ - ] tick button
    Activate,
    /// left/right: cycle the focused chip row, or move the highlight
    /// between the tick row's [ + ] and [ - ] buttons
    ChipDir(isize),
}

pub struct App {
    pub screen: Screen,
}

impl App {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            screen: Screen::Metrics,
        }
    }

    /// Hotkeys are page-scoped and the bottom strip is the primary UI:
    /// the dashboard carries Journal / Lanes / Settings / Quit (strip
    /// words), graph letters (p g v u + shift) act directly, and `l`
    /// still opens the full journal. Server management and the HF
    /// downloader are NOT lsmtop's job (one tool, one thing) — the keys
    /// are gone, the docs live in help.md.
    pub fn handle_key(screen: &Screen, key: KeyCode) -> Action {
        match screen {
            Screen::Metrics => match key {
                KeyCode::Char('q') => Action::Quit,
                // L = Lanes (all/active table filter — 'L' freed when the
                // Llama page left the project); j cycles the journal tier.
                KeyCode::Char('l') => Action::LanesSize,
                KeyCode::Char('L') => Action::LanesVis,
                KeyCode::Char('J') => Action::JournalToggle,
                KeyCode::Char('j') => Action::JournalCycle,
                // 'l' now belongs to LANES size (unified key law); the
                // journal has exactly one lowercase owner: 'j'.
                KeyCode::Char('s') => Action::SwitchScreen(Screen::Settings),
                // e walks the configured ENDPOINTS (banner names the
                // active one). maintainer 2026-11-10: BOTH e and E toggle —
                // Endpoint is the one sanctioned exception to the rule
                // that Shift+letter only ever toggles pane visibility.
                KeyCode::Char('e') | KeyCode::Char('E') => Action::NextTarget,
                // g/G p/P v/V u/U act on the graphs directly and are
                // consumed earlier (graph_hotkey) before this table.
                _ => Action::Noop,
            },
            Screen::Settings => match key {
                KeyCode::Esc => Action::GoBack,
                KeyCode::Tab | KeyCode::Down => Action::SetEdit(SetKey::NextField),
                KeyCode::Up => Action::SetEdit(SetKey::PrevField),
                // left/right act on the focused row: cycle a chip, or
                // slide the highlight between the tick buttons
                KeyCode::Left => Action::SetEdit(SetKey::ChipDir(-1)),
                KeyCode::Right => Action::SetEdit(SetKey::ChipDir(1)),
                KeyCode::Enter | KeyCode::Char(' ') => Action::SetEdit(SetKey::Activate),
                KeyCode::Char('q') => Action::GoBack,
                _ => Action::Noop,
            },
            Screen::Logs => match key {
                KeyCode::Esc | KeyCode::Char('q') => Action::GoBack,
                // j on the full-screen journal CONTINUES the hotkey cycle:
                // it closes the screen and the pane wraps back to four.
                // (Arrows/PageUp/PageDown/End still scroll; k stays the
                // legacy scroll-up alias — only j changed hands.)
                KeyCode::Char('j') => Action::JournalCycle,
                KeyCode::Char('J') => Action::JournalToggle,
                KeyCode::Char('c') => Action::Clear,
                KeyCode::Up | KeyCode::Char('k') => Action::Scroll(1),
                KeyCode::Down => Action::Scroll(-1),
                KeyCode::PageUp => Action::Scroll(10),
                KeyCode::PageDown => Action::Scroll(-10),
                KeyCode::End => Action::Scroll(isize::MIN),
                _ => Action::Noop,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_key_matrix() {
        let cases = vec![
            (Screen::Metrics, KeyCode::Char('q'), Action::Quit),
            (Screen::Metrics, KeyCode::Char('l'), Action::LanesSize),
            (Screen::Metrics, KeyCode::Char('L'), Action::LanesVis),
            (Screen::Metrics, KeyCode::Char('J'), Action::JournalToggle),
            (Screen::Metrics, KeyCode::Char('a'), Action::Noop), // All/Active is gone
            (Screen::Metrics, KeyCode::Char('h'), Action::Noop), // HF-CLI is gone
            (Screen::Metrics, KeyCode::Char('Q'), Action::Noop), // Quit+Stop is gone
            (Screen::Logs, KeyCode::Char('J'), Action::JournalToggle),
            (Screen::Metrics, KeyCode::Char('j'), Action::JournalCycle),
            (Screen::Metrics, KeyCode::Char('s'), Action::SwitchScreen(Screen::Settings)),
            (Screen::Metrics, KeyCode::Char('e'), Action::NextTarget),
            (Screen::Metrics, KeyCode::Char('E'), Action::NextTarget), // maintainer: e/E both toggle
            (Screen::Metrics, KeyCode::Char('t'), Action::Noop), // t retired when Endpoint joined the strip
            (Screen::Logs, KeyCode::Char('e'), Action::Noop), // e belongs to the metrics root only
            (Screen::Settings, KeyCode::Tab, Action::SetEdit(SetKey::NextField)),
            (Screen::Settings, KeyCode::Enter, Action::SetEdit(SetKey::Activate)),
            (Screen::Settings, KeyCode::Char(' '), Action::SetEdit(SetKey::Activate)),
            (Screen::Settings, KeyCode::Right, Action::SetEdit(SetKey::ChipDir(1))),
            (Screen::Settings, KeyCode::Left, Action::SetEdit(SetKey::ChipDir(-1))),
            (Screen::Settings, KeyCode::Char('q'), Action::GoBack),
            (Screen::Settings, KeyCode::Esc, Action::GoBack),
            (Screen::Logs, KeyCode::Char('c'), Action::Clear),
            (Screen::Logs, KeyCode::Char('j'), Action::JournalCycle),
            (Screen::Logs, KeyCode::Char('k'), Action::Scroll(1)),
            (Screen::Logs, KeyCode::Down, Action::Scroll(-1)),
        ];

        for (screen, key, expected) in cases {
            assert_eq!(App::handle_key(&screen, key), expected);
        }
    }
}
