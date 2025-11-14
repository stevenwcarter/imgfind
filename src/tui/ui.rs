use anyhow::{Context, Result};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, BorderType, Clear, Paragraph, StatefulWidget, Widget},
};
use ratatui_image::{Resize, StatefulImage};
use tracing::error;

use super::app::App;
use crate::tui::app::{ImageEntry, InputMode};

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
            if let Err(e) = render_image(0, 9, image, area, buf) {
                error!("Failed to render zoomed image: {}", e);
            }
        } else {
            let nines = nine_block(area);

            for (index, area) in nines.into_iter().enumerate() {
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

fn center(area: Rect, horizontal: Constraint, vertical: Constraint) -> Rect {
    let [area] = Layout::horizontal([horizontal])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([vertical]).flex(Flex::Center).areas(area);
    area
}

fn render_image(
    index: u8,
    focused_image_index: u8,
    image_entry: &mut ImageEntry,
    area: Rect,
    buf: &mut Buffer,
) -> Result<()> {
    let image_area = image_entry
        .protocol
        .size_for(Resize::Scale(None), area)
        .context("could not find size for image")?;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    if index == focused_image_index {
        block.render(area, buf);
    }
    let center = center(
        inner,
        Constraint::Length(image_area.width),
        Constraint::Length(image_area.height),
    );
    let image = StatefulImage::new().resize(Resize::Scale(None));
    image.render(center, buf, &mut image_entry.protocol);

    Ok(())
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
                    Constraint::Fill(1),
                    Constraint::Length(1),
                    Constraint::Length(3),
                ]
                .as_ref(),
            )
            .split(area);
        Block::bordered()
            .title("imgfind-cli")
            .title_alignment(Alignment::Center)
            .border_type(BorderType::Rounded)
            .render(area, buf);

        Clear.render(layout[0], buf);
        self.render_images(layout[0], buf);

        self.render_pagination(layout[1], buf);

        self.render_input(layout[2], buf);
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
