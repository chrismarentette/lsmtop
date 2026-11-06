use ratatui::{
    backend::TestBackend,
    Terminal,
    layout::{Rect, Layout, Constraint, Direction},
    widgets::Widget,
    buffer::Buffer,
};
use lsmtop::ui::chips::ChipRow;

pub struct MetricsOptsWidget {
    pub chars_active: usize,
    pub fill_active: usize,
    pub colors_active: usize,
}

impl Widget for MetricsOptsWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Min(0),
            ])
            .split(area);

        buf.set_string(chunks[0].x, chunks[0].y, " Settings > Metrics Options ", ratatui::style::Style::default());

        let chars_row = ChipRow::new("chars", vec!["braille", "block", "line"], self.chars_active);
        let disabled_fill = self.chars_active == 2; // disabled if line
        let fill_row = ChipRow::new("fill", vec!["stroke", "columns", "area"], self.fill_active).disabled(disabled_fill);
        let colors_row = ChipRow::new("colors", vec!["gradient", "flow", "solid"], self.colors_active);

        chars_row.render(chunks[1], buf);
        fill_row.render(chunks[2], buf);
        colors_row.render(chunks[3], buf);
        
        buf.set_string(chunks[5].x, chunks[5].y + chunks[5].height - 1, "[Save]  [Cancel]", ratatui::style::Style::default());
    }
}

#[test]
fn test_metrics_opts_snapshot() {
    let backend = TestBackend::new(90, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|f| {
        let widget = MetricsOptsWidget {
            chars_active: 2, // line mode
            fill_active: 0,
            colors_active: 1,
        };
        f.render_widget(widget, f.area());
    }).unwrap();

    let buffer = terminal.backend().buffer();
    
    let mut lines = Vec::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push(buffer.cell((x, y)).unwrap().symbol().chars().next().unwrap_or(' '));
        }
        lines.push(line);
    }
    
    let rendered = lines.join("\n");
    insta::assert_snapshot!(rendered);
}
