use image::{DynamicImage, GenericImageView, ImageReader};
use ratatui_image::{FilterType, thread::ThreadProtocol};
use tokio::sync::mpsc::unbounded_channel;
use tracing::debug;

use crate::tui::{app::App, app::ImageEntry};

pub fn zoom_center(img: &DynamicImage, zoom: u8) -> DynamicImage {
    let zoom = zoom.clamp(1, 4);
    let (w, h) = img.dimensions();

    let new_w = (w as f32 / zoom as f32) as u32;
    let new_h = (h as f32 / zoom as f32) as u32;

    let x = (w - new_w) / 2;
    let y = (h - new_h) / 2;

    let cropped = img.crop_imm(x, y, new_w, new_h);

    cropped.resize_exact(w, h, FilterType::Lanczos3)
}

impl App {
    pub fn handle_zoom_image(&mut self, zoom: Option<u8>) {
        if zoom.is_none() {
            self.zoomed_image_index = None;
            self.zoomed_image = None;
        } else {
            self.zoomed_image_index = zoom;
            if let Some(zoom_index) = zoom {
                let image_entry = self
                    .images
                    .get(zoom_index as usize)
                    .expect("image not found");
                let image_score = image_entry.score;
                let zoom_level = self.zoom_level;

                let base_image = image_entry.image.clone().unwrap();
                let image = zoom_center(&base_image, zoom_level);
                // let image = image.resize(800, 800, ratatui_image::FilterType::Triangle);
                debug!("Image decoded successfully");

                let (image_tx, image_rx) = unbounded_channel();
                let protocol = self.picker.new_resize_protocol(image.clone());
                let image_entry = ImageEntry {
                    path: image_entry.path.clone(),
                    score: image_score,
                    rx: image_rx,
                    current_zoom: zoom_level,
                    image: Some(base_image),
                    protocol: ThreadProtocol::new(image_tx, Some(protocol)),
                };

                self.zoomed_image = Some(image_entry);
            }
        }
    }
}
