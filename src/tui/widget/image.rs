use anyhow::{Context, Result};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::{Color, Style},
    widgets::{Block, BorderType, StatefulWidget, Widget},
};
use ratatui_image::{FilterType, Resize, StatefulImage};

use crate::tui::{app::ImageEntry, widget::center};

pub fn render_image(
    index: u8,
    focused_image_index: u8,
    image_entry: &mut ImageEntry,
    area: Rect,
    buf: &mut Buffer,
) -> Result<Rect> {
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
    let image = StatefulImage::new().resize(Resize::Scale(Some(FilterType::CatmullRom)));
    image.render(center, buf, &mut image_entry.protocol);

    Ok(center)
}
