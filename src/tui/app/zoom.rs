use image::{DynamicImage, GenericImageView, ImageReader};
use ratatui_image::{FilterType, thread::ThreadProtocol};
use tokio::sync::mpsc::unbounded_channel;
use tracing::{debug, warn};

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
                let Some(image_entry) = self.images.get(zoom_index as usize) else {
                    warn!("zoom requested for missing image index {zoom_index}");
                    return;
                };
                let image_path = image_entry.path.clone();
                let image_score = image_entry.score;
                let zoom_level = self.zoom_level;

                // find the currently zoomed path
                let zoomed_path = self
                    .zoomed_image
                    .as_ref()
                    .map(|zoomed_image| zoomed_image.path.clone());

                // find the previous zoom level
                let previous_zoom = self
                    .zoomed_image
                    .as_ref()
                    .map(|zoomed_image| zoomed_image.current_zoom)
                    .unwrap_or(1);

                // find the DynamicImage for the fullsize image if we already have it
                let zoomed_image = match self.zoomed_image.as_ref() {
                    Some(zoomed_image) => zoomed_image.image.clone(),
                    None => None,
                };

                if previous_zoom == zoom_level && Some(&image_path) == zoomed_path.as_ref() {
                    return;
                }

                let zoom_tx = self.zoom_tx.clone();
                let picker = self.picker.clone();

                tokio::spawn(async move {
                    debug!("Image path is: {}", image_path);
                    let base_image = if Some(&image_path) == zoomed_path.as_ref()
                        && let Some(zoomed_image) = zoomed_image
                    {
                        zoomed_image
                    } else {
                        match ImageReader::open(&image_path) {
                            Ok(reader) => match reader.decode() {
                                Ok(img) => img,
                                Err(e) => {
                                    warn!("failed to decode image {image_path}: {e}");
                                    return;
                                }
                            },
                            Err(e) => {
                                warn!("failed to open image {image_path}: {e}");
                                return;
                            }
                        }
                    };
                    let image = zoom_center(&base_image, zoom_level);
                    // let image = image.resize(800, 800, ratatui_image::FilterType::Triangle);
                    debug!("Image decoded successfully");

                    let (image_tx, image_rx) = unbounded_channel();
                    let protocol = picker.new_resize_protocol(image.clone());
                    let image_entry = ImageEntry {
                        path: image_path.clone(),
                        score: image_score,
                        rx: image_rx,
                        current_zoom: zoom_level,
                        image: Some(base_image),
                        protocol: ThreadProtocol::new(image_tx, Some(protocol)),
                    };

                    if let Err(e) = zoom_tx.send(image_entry) {
                        debug!("zoom receiver dropped, discarding image entry: {e}");
                    }
                });
            }
        }
    }
}
