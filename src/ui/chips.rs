use ratatui::{
    buffer::Buffer,
    layout::Rect,
    widgets::Widget,
    style::{Style, Modifier, Color},
};

#[derive(Clone, Debug, PartialEq)]
pub struct ChipRow<'a> {
    pub key: &'a str,
    pub values: Vec<&'a str>,
    pub active: usize,
    pub disabled: bool,
}

impl<'a> ChipRow<'a> {
    pub fn new(key: &'a str, values: Vec<&'a str>, active: usize) -> Self {
        Self { key, values, active, disabled: false }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn press(&mut self, index: usize) {
        if !self.disabled && index < self.values.len() {
            self.active = index;
        }
    }

    /// Keyboard cycle one step (dir = +1/-1), wrapping; disabled refuses.
    pub fn cycle(&mut self, dir: isize) {
        if self.disabled || self.values.is_empty() {
            return;
        }
        let n = self.values.len() as isize;
        self.active = ((self.active as isize + dir).rem_euclid(n)) as usize;
    }
}

impl<'a> Widget for ChipRow<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 { return; }

        let base_style = if self.disabled {
            Style::default().add_modifier(Modifier::DIM).fg(Color::DarkGray)
        } else {
            Style::default()
        };

        let active_style = if self.disabled {
            base_style
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        };

        // Render key
        buf.set_string(area.x, area.y, self.key, base_style);
        
        let mut x_offset = area.x + self.key.len() as u16 + 2;
        
        for (i, val) in self.values.iter().enumerate() {
            let label = format!("[ {} ]", val);
            let style = if i == self.active { active_style } else { base_style };
            
            if x_offset + label.len() as u16 > area.x + area.width {
                break;
            }
            
            buf.set_string(x_offset, area.y, &label, style);
            x_offset += label.len() as u16 + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chip_row_state_machine() {
        let mut row = ChipRow::new("test", vec!["a", "b", "c"], 0);
        row.press(2);
        assert_eq!(row.active, 2);
        row.press(1);
        assert_eq!(row.active, 1);
        row.press(5); // out of bounds ignored
        assert_eq!(row.active, 1);
        
        row.disabled = true;
        row.press(0); // disabled, ignored
        assert_eq!(row.active, 1);
    }

    #[test]
    fn cycle_wraps_both_ways_and_disabled_refuses() {
        let mut row = ChipRow::new("t", vec!["a", "b", "c"], 0);
        row.cycle(-1);
        assert_eq!(row.active, 2); // wraps backwards
        row.cycle(1);
        assert_eq!(row.active, 0); // and forwards
        row.cycle(2); // two steps -> c
        assert_eq!(row.active, 2);
        row.disabled = true;
        row.cycle(1);
        assert_eq!(row.active, 2); // disabled refuses
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    #[test]
    fn brackets_always_visible_active_value_inside() {
        let row = ChipRow::new("chars", vec!["braille", "block", "line"], 1);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 1));
        row.render(Rect::new(0, 0, 30, 1), &mut buf);
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains('[') && text.contains(']'), "{text}");
        assert!(text.contains("block"), "active value must render: {text}");
    }

    #[test]
    fn press_disabled_and_cycle_unchanged_when_disabled() {
        let mut row = ChipRow::new("k", vec!["a", "b", "c"], 0);
        row.press(2);
        assert_eq!(row.active, 2);
        let mut dis = ChipRow::new("k", vec!["a", "b"], 0).disabled(true);
        dis.press(1);
        dis.cycle(1);
        assert_eq!(dis.active, 0);
    }
}
