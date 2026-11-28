//! Shared bottom hotkey strip for every screen. G2 law made global: the key
//! glyph is HOT (yellow + bold, like the graph-title accents) and the whole
//! word — glyph plus label — is the mouse target: clicking the word IS
//! pressing the key. The dashboard's graph strip has its own StripTok form
//! (letter inside the word); nested screens use FootTok (hot glyph + the
//! action it triggers, "esc/q" when two keys share an action).

use crossterm::event::KeyCode;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FootTok {
    /// key glyph(s) painted hot; '/' inside splits as a dim separator
    /// ("esc/q Back" = both keys perform this action)
    pub hot: &'static str,
    /// action label painted dim after the hot glyph (" Back")
    pub label: &'static str,
    /// clicking the whole word dispatches this key; None = info token only
    pub key: Option<KeyCode>,
}

/// Plain-text rendering of the strip (tests, logging).
pub fn foot_text(toks: &[FootTok]) -> String {
    let mut s = String::from(" ");
    for (i, t) in toks.iter().enumerate() {
        if i > 0 {
            s.push_str(" \u{b7} ");
        }
        s.push_str(t.hot);
        s.push_str(t.label);
    }
    s.push(' ');
    s
}

fn put(buf: &mut Buffer, x: u16, y: u16, right: u16, ch: char, style: Style) {
    if x >= right {
        return;
    }
    buf[(x, y)].set_char(ch).set_style(style);
}

/// Paint the strip at (x, y) across `width`; returns one hit rect per
/// clickable token (the WHOLE word, hot + label) whose click dispatches
/// the key — paint and hit map come from the same walk, they cannot drift.
pub fn draw_foot(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    toks: &[FootTok],
) -> Vec<(Rect, KeyCode)> {
    let mut hits = Vec::new();
    let right = x + width;
    let hot_st = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let dim_st = Style::default().fg(Color::DarkGray);
    let mut col = x + 1;
    for (i, t) in toks.iter().enumerate() {
        if i > 0 {
            for ch in " \u{b7} ".chars() {
                put(buf, col, y, right, ch, dim_st);
                col += 1;
            }
        }
        let start = col;
        for ch in t.hot.chars() {
            let st = if ch == '/' { dim_st } else { hot_st };
            put(buf, col, y, right, ch, st);
            col += 1;
        }
        for ch in t.label.chars() {
            put(buf, col, y, right, ch, dim_st);
            col += 1;
        }
        if let Some(k) = t.key {
            hits.push((Rect::new(start, y, col.saturating_sub(start), 1), k));
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn toks() -> Vec<FootTok> {
        vec![
            FootTok { hot: "esc/q", label: " Back", key: Some(KeyCode::Esc) },
            FootTok { hot: "c", label: " Clear view", key: Some(KeyCode::Char('c')) },
            FootTok { hot: "wheel", label: " Scroll", key: None },
        ]
    }

    #[test]
    fn hot_glyphs_paint_yellow_bold_and_slash_stays_dim() {
        let area = ratatui::layout::Rect { x: 0, y: 0, width: 60, height: 1 };
        let mut buf = Buffer::empty(area);
        draw_foot(&mut buf, 0, 0, 60, &toks());
        // " esc/q Back ..." — col 1 is the 'e' of esc
        assert_eq!(buf[(1, 0)].symbol(), "e");
        assert_eq!(buf[(1, 0)].fg, Color::Yellow);
        assert!(buf[(1, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(4, 0)].symbol(), "/");
        assert_eq!(buf[(4, 0)].fg, Color::DarkGray); // the slash is NOT hot
        assert_eq!(buf[(5, 0)].symbol(), "q");
        assert_eq!(buf[(5, 0)].fg, Color::Yellow);
        assert_eq!(buf[(7, 0)].symbol(), "B"); // label paint starts after a space
        assert_eq!(buf[(7, 0)].fg, Color::DarkGray);
        // the third token's "wheel" is a key glyph too
        let row: String = (0..60).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        let wcol = row.find("wheel").unwrap();
        assert_eq!(buf[(wcol as u16, 0)].fg, Color::Yellow);
    }

    #[test]
    fn hit_rects_span_the_whole_word_and_skip_info_tokens() {
        let area = ratatui::layout::Rect { x: 0, y: 0, width: 60, height: 1 };
        let mut buf = Buffer::empty(area);
        let hits = draw_foot(&mut buf, 0, 0, 60, &toks());
        assert_eq!(hits.len(), 2, "wheel Scroll has no key -> no rect");
        assert_eq!(hits[0].1, KeyCode::Esc);
        assert_eq!(hits[0].0.x, 1);
        assert_eq!(hits[0].0.width, 10, "rect covers esc/q plus ' Back'");
        assert_eq!(hits[1].1, KeyCode::Char('c'));
    }

    #[test]
    fn foot_text_lists_the_words_in_order() {
        let t = foot_text(&toks());
        assert_eq!(t, " esc/q Back \u{b7} c Clear view \u{b7} wheel Scroll ");
    }
}
