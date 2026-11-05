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
}
