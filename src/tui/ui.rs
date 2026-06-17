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
    app::{InputMode, keybindings_help},
    event::AppEvent,
    widget::{center, nine_block, render_image},
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
            "Page {}/{} ({} results) [H prev | L next]",
            self.page + 1,
            total_pages.max(1),
            search_result.result_count
        );
        let pagination = Paragraph::new(page_info)
            .style(Style::default())
            .alignment(Alignment::Center);
        pagination.render(area, buf);
    }
    fn render_help_overlay(&self, area: Rect, buf: &mut Buffer) {
        let lines = keybindings_help();
        // Size the box to fit the content, with a little padding for borders.
        let inner_width = lines.iter().map(|l| l.len()).max().unwrap_or(0) as u16;
        let width = (inner_width + 4).min(area.width);
        let height = (lines.len() as u16 + 2).min(area.height);
        let popup = center(area, Constraint::Length(width), Constraint::Length(height));
        Clear.render(popup, buf);
        let help = Paragraph::new(lines.join("\n")).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title("Keybindings (? to close)")
                .title_alignment(Alignment::Center),
        );
        help.render(popup, buf);
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

    /// Renders a one-line control hint at the bottom of the zoom area while an
    /// image is zoomed.
    fn render_zoom_status(&self, area: Rect, buf: &mut Buffer) {
        if self.zoomed_image.is_none() {
            return;
        }
        // Place the hint on the bottom row of the displayed zoom image when its
        // rect is known, otherwise fall back to the bottom of the main area.
        let status_area = self
            .zoomed_image_rect
            .map(|rect| Rect::new(rect.x, rect.bottom().saturating_sub(1), rect.width, 1))
            .unwrap_or_else(|| Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1));
        Paragraph::new("scroll to zoom | right-click to reset | ESC to close")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center)
            .render(status_area, buf);
    }

    fn render_images(&mut self, area: Rect, buf: &mut Buffer) {
        if let Some(image) = self.zoomed_image.as_mut() {
            Clear.render(area, buf);
            match render_image(0, 9, image, area, buf) {
                Ok(rect) => self.zoomed_image_rect = Some(rect),
                Err(e) => error!("Failed to render zoomed image: {}", e),
            }
            self.render_zoom_status(area, buf);
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
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
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

        if self.show_help {
            self.render_help_overlay(area, buf);
        }
    }
}
