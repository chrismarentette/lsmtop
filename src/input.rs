//! The key law, extracted from the main loop so it is TESTABLE without a
//! terminal (coverage arc P2, maintainer 2026-11-07). State mutation stays in
//! main; every DECISION a key makes lives here as a pure function.
use crossterm::event::{KeyCode, KeyModifiers};

/// Ctrl+C is the only chord the loop handles before any screen law.
pub fn is_quit_chord(code: KeyCode, m: KeyModifiers) -> bool {
    m.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c')
}

/// Enhanced-keyboard (kitty/CSI-u) terminals report a shifted letter as the
/// BASE codepoint + SHIFT; legacy terminals send the uppercase char itself.
/// Normalize so one key law holds everywhere (a bare 'l' spelling of L
/// opened the journal instead of toggling lanes — live report 2026-11-07).
pub fn normalize_key(code: KeyCode, m: KeyModifiers) -> KeyCode {
    match code {
        KeyCode::Char(c) if m.contains(KeyModifiers::SHIFT) => {
            KeyCode::Char(c.to_ascii_uppercase())
        }
        c => c,
    }
}

/// What a graph hotkey means under the unified key law.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotKind {
    /// lowercase: cycle SIZE, never parks on off
    Size,
    /// SHIFT: toggle VISIBILITY, restore last size
    Visibility,
}

/// `t` law (multi-server, 2026-11-10): the ACTIVE target walks forward
/// through the configured targets and wraps. With 0 or 1 targets the
/// press is honestly INERT — no phantom cycle, no state churn.
pub fn next_target(cur: usize, n: usize) -> usize {
    if n <= 1 {
        cur
    } else {
        (cur + 1) % n
    }
}

/// The `p g v u` / `P G V U` table — pure, so the unified key law is locked
/// even though Rt and the config live behind the loop.
pub fn graph_hotkey_map(code: KeyCode) -> Option<(&'static str, &'static str, HotKind)> {
    let (cfg_key, name, kind) = match code {
        KeyCode::Char('g') => ("gen_size", "GEN", HotKind::Size),
        KeyCode::Char('p') => ("prompt_size", "PROMPT", HotKind::Size),
        KeyCode::Char('v') => ("kv_size", "KV", HotKind::Size),
        KeyCode::Char('u') => ("gpu_size", "GPU", HotKind::Size),
        KeyCode::Char('G') => ("gen_graph", "GEN", HotKind::Visibility),
        KeyCode::Char('P') => ("prompt_graph", "PROMPT", HotKind::Visibility),
        KeyCode::Char('V') => ("kv_graph", "KV", HotKind::Visibility),
        KeyCode::Char('U') => ("gpu_graph", "GPU", HotKind::Visibility),
        _ => return None,
    };
    Some((cfg_key, name, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_chord_only_ctrl_c() {
        assert!(is_quit_chord(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!is_quit_chord(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(!is_quit_chord(KeyCode::Char('C'), KeyModifiers::CONTROL));
        assert!(!is_quit_chord(KeyCode::Char('q'), KeyModifiers::CONTROL));
    }

    #[test]
    fn csi_u_shift_normalizes_to_upper() {
        // kitty: Char('l') + SHIFT  == legacy: Char('L')
        assert_eq!(
            normalize_key(KeyCode::Char('l'), KeyModifiers::SHIFT),
            KeyCode::Char('L')
        );
        assert_eq!(normalize_key(KeyCode::Char('l'), KeyModifiers::NONE), KeyCode::Char('l'));
        // already uppercase stays
        assert_eq!(normalize_key(KeyCode::Char('L'), KeyModifiers::SHIFT), KeyCode::Char('L'));
        // non-chars untouched
        assert_eq!(normalize_key(KeyCode::Enter, KeyModifiers::SHIFT), KeyCode::Enter);
        assert_eq!(normalize_key(KeyCode::Char('1'), KeyModifiers::SHIFT), KeyCode::Char('1'));
    }

    #[test]
    fn t_cycles_targets_and_stays_inert_below_two() {
        // walk + wrap over three servers
        assert_eq!(next_target(0, 3), 1);
        assert_eq!(next_target(1, 3), 2);
        assert_eq!(next_target(2, 3), 0);
        // one server (or none): the press changes NOTHING
        assert_eq!(next_target(0, 1), 0);
        assert_eq!(next_target(0, 0), 0);
    }

    #[test]
    fn unified_key_law_table() {
        // the eight graph keys, and ONLY they, plus kind truth
        for (c, cfg, name) in [
            ('g', "gen_size", "GEN"),
            ('p', "prompt_size", "PROMPT"),
            ('v', "kv_size", "KV"),
            ('u', "gpu_size", "GPU"),
        ] {
            let (k, n, kind) = graph_hotkey_map(KeyCode::Char(c)).unwrap();
            assert_eq!((k, n, kind), (cfg, name, HotKind::Size));
            let (k2, n2, kind2) =
                graph_hotkey_map(KeyCode::Char(c.to_ascii_uppercase())).unwrap();
            let want = cfg.replace("size", "graph");
            assert_eq!((k2, n2, kind2), (want.as_str(), name, HotKind::Visibility));
        }
        // the law reserves these; every other key belongs to the screens
        for c in ['j', 'J', 'l', 'L', 's', 'q', 'k', 'h', 'x', '1'] {
            assert!(graph_hotkey_map(KeyCode::Char(c)).is_none(), "{c} must not be a graph key");
        }
        assert!(graph_hotkey_map(KeyCode::Esc).is_none());
    }
}
