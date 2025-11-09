use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    widgets::{Block, BorderType, Borders, Paragraph, StatefulWidget, Widget},
};
use ratatui_image::{
    Image, Resize, StatefulImage, picker::Picker, protocol::Protocol, thread::ThreadProtocol,
};

use crate::tui::app::InputMode;

use super::app::App;

impl App {
    fn render_input(&self, area: Rect, buf: &mut Buffer) {
        // keep 2 for borders and 1 for cursor
        let width = area.width.max(3) - 3;
        let scroll = self.input.visual_scroll(width as usize);
        let style = match self.input_mode {
            InputMode::Normal => Style::default(),
            InputMode::Editing => Color::Yellow.into(),
        };
        let input = Paragraph::new(self.input.value())
            .style(style)
            .scroll((0, scroll as u16))
            .block(Block::bordered().title("Input"));
        input.render(area, buf);

        if self.input_mode == InputMode::Editing {
            // Ratatui hides the cursor unless it's explicitly set. Position the  cursor past the
            // end of the input text and one line down from the border to the input line
            let x = self.input.visual_cursor().max(scroll) - scroll + 1;
            // frame.set_cursor_position((area.x + x as u16, area.y + 1))
        }
    }
}

impl Widget for &mut App {
    /// Renders the user interface widgets.
    ///
    // This is where you add new widgets.
    // See the following resources:
    // - https://docs.rs/ratatui/latest/ratatui/widgets/index.html
    // - https://github.com/ratatui/ratatui/tree/master/examples
    fn render(self, area: Rect, buf: &mut Buffer) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints(
                [
                    Constraint::Percentage(20),
                    Constraint::Percentage(60),
                    Constraint::Percentage(20),
                ]
                .as_ref(),
            )
            .split(area);
        let block = Block::bordered()
            .title("imgfind-cli")
            .title_alignment(Alignment::Center)
            .border_type(BorderType::Rounded);

        let text = format!(
            "Press left and right to increment and decrement the counter respectively.\n\
                Counter: {}",
            self.counter
        );

        let paragraph = Paragraph::new(text)
            .block(block)
            .fg(Color::Cyan)
            .bg(Color::Black)
            .centered();

        let block = Block::default().borders(Borders::ALL).title("Async test");

        paragraph.render(layout[0], buf);

        // StatefulImage::new().render(layout[1], buf, &mut self.protocol);
        let nines = nine_block(layout[1]);

        for (index, area) in nines.into_iter().enumerate() {
            if let Some((path, score, protocol)) = self.images.get_mut(index) {
                eprintln!("rendering image at index {}", index);
                StatefulImage::new().render(area, buf, protocol);
            }
        }

        self.render_input(layout[2], buf);

        // let protocol = self
        //     .picker
        //     .new_protocol(self.image.clone(), layout[1], Resize::Fit(None))
        //     .unwrap();
        // let image = Image::new(&protocol);
        // image.render(layout[1], buf);
        // StatefulImage::new().render(layout[1], buf, state);

        // .block(block)
        // .render(layout[1], buf);
    }
}

pub fn nine_block(area: Rect) -> Vec<Rect> {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage(33),
                Constraint::Percentage(34),
                Constraint::Percentage(33),
            ]
            .as_ref(),
        )
        .split(area);

    let mut areas = Vec::new();

    for area in layout.iter() {
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(
                [
                    Constraint::Percentage(33),
                    Constraint::Percentage(34),
                    Constraint::Percentage(33),
                ]
                .as_ref(),
            )
            .split(*area);

        for h_area in horizontal.iter() {
            areas.push(*h_area);
        }
    }

    areas
}
