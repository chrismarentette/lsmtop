use crossterm::event::KeyCode;
use crate::net::{ServerSample, RouterSample};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Metrics,
    Manager,
    Settings,
    MetricsOpts,
    Logs,
    Confirm,
    Hf,
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
    /// tree API result for the HF screen (repo, files|error)
    HfTree(String, Result<Vec<crate::hf::TreeFile>, String>),
    /// line destined for the HF screen log
    HfLogLine(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SampleData {
    Server(ServerSample),
    Router(RouterSample),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapeError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    QuitAndStop,
    GoBack,
    SwitchScreen(Screen),
    ToggleLanes,
    Journal,
    Refresh,
    Download,
    Clear,
    Cancel,
    Save,
    OpenOpts,
    MgrNext,
    MgrPrev,
    Start,
    Stop,
    SelectTarget,
    /// SIGHUP the router's MainPID
    Hup,
    /// print the systemctl commands for a human (hand-off doctrine)
    ShowCmds,
    /// Settings-screen line editor
    SetEdit(SetKey),
    /// Settings esc: leave edit mode first, screen second
    SetEsc,
    OptsUp,
    OptsDown,
    OptsCycle,
    /// positive = scroll toward older lines
    Scroll(isize),
    /// pass-through key for the HF text-entry screen
    HfKey(crate::ui::hf::HfKey),
    Noop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetKey {
    NextField,
    PrevField,
    Char(char),
    Backspace,
    Commit,
    ClearTok,
    /// vim-ish: Enter toggles edit mode for the focused field; outside
    /// edit mode the classic `m` hotkey opens the metrics options (maintainer)
    ToggleEdit,
    /// 'm' with context: main decides edit-mode insert vs OpenOpts
    MaybeOpts,
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

    /// Hotkeys ported from the BINDINGS, page-scoped, doctrine kept:
    /// home (Metrics) has NO esc and only what the page is about (a/l);
    /// app-level h/s/g/q/Q live on the home stack; esc belongs to sub-pages.
    pub fn handle_key(screen: &Screen, key: KeyCode) -> Action {
        match screen {
            Screen::Metrics => match key {
                KeyCode::Char('q') => Action::Quit,
                KeyCode::Char('Q') => Action::QuitAndStop,
                KeyCode::Char('a') => Action::ToggleLanes,
                KeyCode::Char('l') => Action::Journal,
                KeyCode::Char('g') => Action::SwitchScreen(Screen::Manager),
                KeyCode::Char('s') => Action::SwitchScreen(Screen::Settings),
                KeyCode::Char('h') => Action::Download,
                _ => Action::Noop,
            },
            Screen::Manager => match key {
                KeyCode::Esc => Action::GoBack,
                KeyCode::Char('q') => Action::Quit,
                KeyCode::Char('Q') => Action::QuitAndStop,
                KeyCode::Char('g') => Action::GoBack,
                KeyCode::Char('h') => Action::Download,
                KeyCode::Char('S') => Action::SwitchScreen(Screen::Settings),
                KeyCode::Char('r') => Action::Refresh,
                KeyCode::Char('u') => Action::Hup,
                KeyCode::Char('c') => Action::ShowCmds,
                KeyCode::Char('j') => Action::Journal,
                KeyCode::Tab | KeyCode::Down => Action::MgrNext,
                KeyCode::Char('k') | KeyCode::Up => Action::MgrPrev,
                KeyCode::Char('s') => Action::Start,
                KeyCode::Char('x') => Action::Stop,
                KeyCode::Enter => Action::SelectTarget,
                _ => Action::Noop,
            },
            Screen::Settings => match key {
                KeyCode::Esc => Action::SetEsc,
                KeyCode::Tab | KeyCode::Down => Action::SetEdit(SetKey::NextField),
                KeyCode::Up => Action::SetEdit(SetKey::PrevField),
                KeyCode::Enter => Action::SetEdit(SetKey::ToggleEdit),
                KeyCode::Char('m') => Action::SetEdit(SetKey::MaybeOpts),
                KeyCode::Char('!') => Action::SetEdit(SetKey::ClearTok),
                KeyCode::Char('#') => Action::OpenOpts, // safe: '#' is not a path/token char
                KeyCode::Backspace => Action::SetEdit(SetKey::Backspace),
                KeyCode::Char(c) => Action::SetEdit(SetKey::Char(c)),
                _ => Action::Noop,
            },
            Screen::MetricsOpts => match key {
                KeyCode::Esc | KeyCode::Char('q') => Action::GoBack,
                KeyCode::Enter => Action::Save,
                KeyCode::Up => Action::OptsUp,
                KeyCode::Down => Action::OptsDown,
                KeyCode::Char(' ') => Action::OptsCycle,
                KeyCode::Left | KeyCode::Char('h') => Action::MgrPrev,
                KeyCode::Right | KeyCode::Char('l') => Action::MgrNext,
                _ => Action::Noop,
            },
            Screen::Logs => match key {
                KeyCode::Esc | KeyCode::Char('q') => Action::GoBack,
                KeyCode::Char('c') => Action::Clear,
                KeyCode::Up | KeyCode::Char('k') => Action::Scroll(1),
                KeyCode::Down | KeyCode::Char('j') => Action::Scroll(-1),
                KeyCode::PageUp => Action::Scroll(10),
                KeyCode::PageDown => Action::Scroll(-10),
                KeyCode::End => Action::Scroll(isize::MIN),
                _ => Action::Noop,
            },
            Screen::Hf => match key {
                KeyCode::Esc => Action::GoBack,
                KeyCode::Enter => Action::HfKey(crate::ui::hf::HfKey::Enter),
                KeyCode::Backspace => Action::HfKey(crate::ui::hf::HfKey::Backspace),
                KeyCode::Char('d') => Action::HfKey(crate::ui::hf::HfKey::Download),
                KeyCode::Char(c) => Action::HfKey(crate::ui::hf::HfKey::Char(c)),
                _ => Action::HfKey(crate::ui::hf::HfKey::Other),
            },
            Screen::Confirm => match key {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('n') => Action::Cancel,
                KeyCode::Char('y') | KeyCode::Enter => Action::QuitAndStop,
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
            (Screen::Hf, KeyCode::Char('d'), Action::HfKey(crate::ui::hf::HfKey::Download)),
            (Screen::Hf, KeyCode::Enter, Action::HfKey(crate::ui::hf::HfKey::Enter)),
            (Screen::Hf, KeyCode::Esc, Action::GoBack),
            (Screen::Hf, KeyCode::Char('o'), Action::HfKey(crate::ui::hf::HfKey::Char('o'))),
            (Screen::Metrics, KeyCode::Char('Q'), Action::QuitAndStop),
            (Screen::Metrics, KeyCode::Char('a'), Action::ToggleLanes),
            (Screen::Metrics, KeyCode::Char('l'), Action::Journal),
            (Screen::Manager, KeyCode::Esc, Action::GoBack),
            (Screen::Manager, KeyCode::Char('q'), Action::Quit),
            (Screen::Manager, KeyCode::Char('Q'), Action::QuitAndStop),
            (Screen::Manager, KeyCode::Char('u'), Action::Hup),
            (Screen::Manager, KeyCode::Char('c'), Action::ShowCmds),
            (Screen::Manager, KeyCode::Char('S'), Action::SwitchScreen(Screen::Settings)),
            (Screen::Settings, KeyCode::Tab, Action::SetEdit(SetKey::NextField)),
            (Screen::Settings, KeyCode::Char('!'), Action::SetEdit(SetKey::ClearTok)),
            (Screen::Settings, KeyCode::Char('a'), Action::SetEdit(SetKey::Char('a'))),
            (Screen::Settings, KeyCode::Char('m'), Action::SetEdit(SetKey::MaybeOpts)),
            (Screen::Settings, KeyCode::Enter, Action::SetEdit(SetKey::ToggleEdit)),
            (Screen::Settings, KeyCode::Esc, Action::SetEsc),
            (Screen::Settings, KeyCode::Char('#'), Action::OpenOpts),
            (Screen::MetricsOpts, KeyCode::Up, Action::OptsUp),
            (Screen::MetricsOpts, KeyCode::Down, Action::OptsDown),
            (Screen::MetricsOpts, KeyCode::Char(' '), Action::OptsCycle),
            (Screen::Manager, KeyCode::Char('r'), Action::Refresh),
            (Screen::MetricsOpts, KeyCode::Char('q'), Action::GoBack),
            (Screen::Logs, KeyCode::Char('c'), Action::Clear),
            (Screen::Confirm, KeyCode::Esc, Action::Cancel),
        ];

        for (screen, key, expected) in cases {
            assert_eq!(App::handle_key(&screen, key), expected);
        }
    }
}
