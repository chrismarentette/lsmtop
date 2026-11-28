//! Unified button doctrine (maintainer 2026-11-07): every on-screen "button" is a
//! bracketed key like `[ line ]` and carries a SUBTLE highlight at all times
//! so it reads as pressable; the selected state uses the blue-and-white combo
//! of the app title bar. One source of those styles — no screen may invent
//! its own button colours.
use ratatui::style::{Color, Modifier, Style};

/// The app title bar blue — THE selection colour, everywhere.
pub const TITLE_BLUE: Color = Color::Rgb(9, 84, 170);

/// Header bar style (title bar AND every consistent page title).
pub fn header() -> Style {
    Style::default()
        .bg(TITLE_BLUE)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// A bracketed button at rest: white on a muted blue block — always
/// slightly lit, so buttons never masquerade as plain text.
pub fn button() -> Style {
    Style::default().fg(Color::White).bg(Color::Blue)
}

/// The selected / active button: the title-bar combo rendered as a white
/// block with blue letters (REVERSED over the title blue). REVERSED also
/// survives every terminal AND the pyte harness (bright-bg SGRs do not).
pub fn button_active() -> Style {
    button()
        .bg(TITLE_BLUE)
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD)
}

/// Row-focus indicator (the label of the row the ring sits on): blue block,
/// white letters — the same family, quieter than an active button.
pub fn focus_label() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(TITLE_BLUE)
        .add_modifier(Modifier::BOLD)
}

/// The [ + ] button: the normal button combo (maintainer 2026-11-07: no
/// special ink — blue/white like every other bracketed button).
pub fn plus(pressed: bool) -> Style {
    let s = Style::default().fg(Color::White).bg(Color::Blue);
    if pressed {
        s.bg(TITLE_BLUE).add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        s
    }
}

/// The [ - ] button: same normal combo, no red ink.
pub fn minus(pressed: bool) -> Style {
    let s = Style::default().fg(Color::White).bg(Color::Blue);
    if pressed {
        s.bg(TITLE_BLUE).add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        s
    }
}

/// A disabled button: dim, never lit — the block stays so the layout holds.
pub fn button_disabled() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .bg(Color::Black)
        .add_modifier(Modifier::DIM)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    // The unified button doctrine (maintainer 2026-11-07), locked: buttons are
    // blue/white ONLY — no red/green ink — and pressed state is the title
    // blue REVERSED over white.

    #[test]
    fn header_is_the_title_bar() {
        let s = header();
        assert_eq!(s.bg, Some(TITLE_BLUE));
        assert_eq!(s.fg, Some(Color::White));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn button_at_rest_is_always_lit() {
        let s = button();
        assert_eq!(s.fg, Some(Color::White));
        assert_eq!(s.bg, Some(Color::Blue)); // never plain: reads as pressable
    }

    #[test]
    fn button_active_is_the_reversed_title_combo() {
        let s = button_active();
        assert_eq!(s.bg, Some(TITLE_BLUE));
        assert!(s.add_modifier.contains(Modifier::REVERSED)); // harness-safe
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn focus_label_is_the_quieter_sibling() {
        let s = focus_label();
        assert_eq!(s.fg, Some(Color::White));
        assert_eq!(s.bg, Some(TITLE_BLUE));
        assert!(!s.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn plus_minus_share_the_neutral_doctrine() {
        // no special green/red ink; pressed == active combo, rest == button
        assert_eq!(plus(false), button());
        assert_eq!(minus(false), button());
        for pressed in [plus(true), minus(true)] {
            assert_eq!(pressed, button_active());
        }
        for s in [button(), button_active(), plus(false), minus(false), plus(true), minus(true)] {
            assert!(!matches!(s.fg, Some(Color::Red | Color::Green | Color::LightRed | Color::LightGreen)));
            assert!(!matches!(s.bg, Some(Color::Red | Color::Green | Color::LightRed | Color::LightGreen)));
        }
    }

    #[test]
    fn disabled_keeps_the_block_shape() {
        let s = button_disabled();
        assert_eq!(s.fg, Some(Color::DarkGray));
        assert!(s.add_modifier.contains(Modifier::DIM));
    }
}
