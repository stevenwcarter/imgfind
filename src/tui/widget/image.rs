use anyhow::{Context, Result};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Rect},
    style::{Color, Style},
    text::{Line, Span},
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
        .size_for(Resize::Scale(None), area.into())
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

    // Overlay a compact, right-aligned similarity score on the bottom row of
    // the cell (rendered after the image so it stays visible).
    if area.height > 0 && area.width > 0 {
        let label = format!("{:.3}", image_entry.score);
        let label_area = Rect {
            x: area.x,
            y: area.bottom().saturating_sub(1),
            width: area.width,
            height: 1,
        };
        let line = Line::from(Span::styled(label, Style::default().fg(Color::DarkGray)))
            .alignment(Alignment::Right);
        line.render(label_area, buf);
    }

    Ok(center)
}
