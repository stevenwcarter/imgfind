use std::rc::Rc;

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Style},
    widgets::{Block, BorderType, Clear, Paragraph, Widget},
};
use tracing::error;

use super::app::App;
use crate::tui::{
    app::InputMode,
    event::AppEvent,
    widget::{nine_block, render_image},
};

impl App {
    fn render_pagination(&self, area: Rect, buf: &mut Buffer) {
        let search_result = self.search_result.as_ref();
        if search_result.is_none() {
            return;
        }
        let search_result = search_result.unwrap();
        let total_pages = search_result.result_count.div_ceil(9);
        let page_info = format!(
            "Page {}/{} ({} results)",
            self.page + 1,
            total_pages.max(1),
            search_result.result_count
        );
        let pagination = Paragraph::new(page_info)
            .style(Style::default())
            .alignment(Alignment::Center);
        pagination.render(area, buf);
    }
    fn render_input(&self, area: Rect, buf: &mut Buffer) {
        // keep 2 for borders and 1 for cursor
        let width = area.width.max(3) - 3;
        let scroll = self.input.visual_scroll(width as usize);
        let style = match self.input_mode {
            InputMode::Normal => Style::default(),
            InputMode::Editing => Color::Yellow.into(),
        };
        let title = if let Some(query) = self.last_search.as_ref() {
            if self.is_searching {
                format!("Search query (searching '{}'...)", query)
            } else {
                format!("Search query (current: {})", query)
            }
        } else {
            "Search query".to_string()
        };
        let input = Paragraph::new(self.input.value())
            .style(style)
            .scroll((0, scroll as u16))
            .block(Block::bordered().title(title));
        input.render(area, buf);

        if self.input_mode == InputMode::Editing {
            // Ratatui hides the cursor unless it's explicitly set. Position the cursor past the
            // end of the input text and one line down from the border to the input line
            let _x = self.input.visual_cursor().max(scroll) - scroll + 1;
        }
    }

    fn render_images(&mut self, area: Rect, buf: &mut Buffer) {
        if let Some(image) = self.zoomed_image.as_mut() {
            Clear.render(area, buf);
            match render_image(0, 9, image, area, buf) {
                Ok(rect) => self.zoomed_image_rect = Some(rect),
                Err(e) => error!("Failed to render zoomed image: {}", e),
            }
        } else {
            self.zoomed_image_rect = None;
            let nines = nine_block(area);

            for (index, area) in nines.into_iter().enumerate() {
                if let Some(mouse_event) = self.mouse_click
                    && area.contains(Position {
                        x: mouse_event.column,
                        y: mouse_event.row,
                    })
                {
                    self.mouse_click = None;
                    self.events.send(AppEvent::ZoomImage(Some(index as u8)));
                }
                if let Some(image_entry) = self.images.get_mut(index)
                    && let Err(e) = render_image(
                        index as u8,
                        self.focused_image_index,
                        image_entry,
                        area,
                        buf,
                    )
                {
                    error!("Failed to render image at index {}: {}", index, e);
                }
            }
        }
    }
}

fn build_layout(area: Rect) -> Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints(
            [
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(3),
            ]
            .as_ref(),
        )
        .split(area)
}

impl Widget for &mut App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let layout = build_layout(area);
        Block::bordered()
            .title("imgfind-cli")
            .title_alignment(Alignment::Center)
            .border_type(BorderType::Rounded)
            .render(area, buf);

        self.render_images(layout[0], buf);

        self.render_pagination(layout[1], buf);

        self.render_input(layout[2], buf);
    }
}
