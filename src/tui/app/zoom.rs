use image::ImageReader;
use ratatui_image::thread::ThreadProtocol;
use tokio::sync::mpsc::unbounded_channel;
use tracing::debug;

use crate::tui::{app::App, app::ImageEntry};

impl App {
    pub fn handle_zoom_image(&mut self, zoom: Option<u8>) {
        if self.zoomed_image_index == zoom || zoom.is_none() {
            self.zoomed_image_index = None;
            self.zoomed_image = None;
        } else {
            self.zoomed_image_index = zoom;
            if let Some(zoom_index) = zoom {
                let image_entry = self
                    .images
                    .get(zoom_index as usize)
                    .expect("image not found");
                let image_path = image_entry.path.clone();
                let image_score = image_entry.score;

                let zoom_tx = self.zoom_tx.clone();
                let picker = self.picker.clone();

                tokio::spawn(async move {
                    debug!("Image path is: {}", image_path);
                    let image = ImageReader::open(image_path.clone())
                        .expect("could not open")
                        .decode()
                        .expect("could not decoded");
                    // let image = image.resize(800, 800, ratatui_image::FilterType::Triangle);
                    debug!("Image decoded successfully");

                    let (image_tx, image_rx) = unbounded_channel();
                    let protocol = picker.new_resize_protocol(image);
                    let image_entry = ImageEntry {
                        path: image_path.clone(),
                        score: image_score,
                        rx: image_rx,
                        protocol: ThreadProtocol::new(image_tx, Some(protocol)),
                    };

                    zoom_tx
                        .send(image_entry)
                        .expect("Could not send image entry");
                });
            }
        }
    }
}
