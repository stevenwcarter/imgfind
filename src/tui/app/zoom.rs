use image::{DynamicImage, GenericImageView, ImageReader};
use ratatui::layout::Rect;
use ratatui_image::{FilterType, thread::ThreadProtocol};
use tokio::sync::mpsc::unbounded_channel;
use tracing::{debug, warn};

use crate::tui::{app::App, app::ImageEntry};

/// Compute the new crop-window center (focal point, in original-image normalized
/// [0,1] coordinates) so that the pixel currently under the cursor stays under the
/// cursor when zooming from `old_zoom` to `new_zoom`.
///
/// `rect` is the on-screen rect the zoomed image is drawn into; `col`/`row` are the
/// cursor's terminal cell. Works in normalized coordinates, so image dimensions cancel.
pub fn focal_for_zoom_at_cursor(
    rect: Rect,
    col: u16,
    row: u16,
    old_zoom: u8,
    old_focal: (f32, f32),
    new_zoom: u8,
) -> (f32, f32) {
    if new_zoom <= 1 {
        return (0.5, 0.5);
    }

    let u = if rect.width == 0 {
        0.5
    } else {
        (col.saturating_sub(rect.x) as f32 / rect.width as f32).clamp(0.0, 1.0)
    };
    let v = if rect.height == 0 {
        0.5
    } else {
        (row.saturating_sub(rect.y) as f32 / rect.height as f32).clamp(0.0, 1.0)
    };

    let s_old = 1.0 / old_zoom.max(1) as f32;
    let s_new = 1.0 / new_zoom as f32;

    let (fx_old, fy_old) = old_focal;
    let o_old_x = (fx_old - s_old / 2.0).clamp(0.0, 1.0 - s_old);
    let o_old_y = (fy_old - s_old / 2.0).clamp(0.0, 1.0 - s_old);

    let po_x = o_old_x + u * s_old;
    let po_y = o_old_y + v * s_old;

    let o_new_x = po_x - u * s_new;
    let o_new_y = po_y - v * s_new;

    let fx_new = (o_new_x + s_new / 2.0).clamp(s_new / 2.0, 1.0 - s_new / 2.0);
    let fy_new = (o_new_y + s_new / 2.0).clamp(s_new / 2.0, 1.0 - s_new / 2.0);

    (fx_new, fy_new)
}

pub fn zoom_center(img: &DynamicImage, zoom: u8, focal: (f32, f32)) -> DynamicImage {
    let zoom = zoom.clamp(1, 4);
    let (w, h) = img.dimensions();

    if zoom == 1 {
        return img.clone();
    }

    // `.max(1)` guards pathologically tiny images (dimension < zoom) from a
    // zero-size crop, which would panic the resampler below.
    let new_w = ((w as f32 / zoom as f32) as u32).max(1);
    let new_h = ((h as f32 / zoom as f32) as u32).max(1);

    // Crop top-left so the window is centered on the focal point, clamped in bounds.
    let cx = focal.0 * w as f32;
    let cy = focal.1 * h as f32;
    let x = (cx - new_w as f32 / 2.0)
        .round()
        .clamp(0.0, (w - new_w) as f32) as u32;
    let y = (cy - new_h as f32 / 2.0)
        .round()
        .clamp(0.0, (h - new_h) as f32) as u32;

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
                let focal = self.zoom_focal;

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
                    let image = zoom_center(&base_image, zoom_level, focal);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn rect() -> Rect {
        Rect { x: 10, y: 5, width: 100, height: 50 }
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {b}, got {a}");
    }

    #[test]
    fn new_zoom_one_resets_to_center() {
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 60, 30, 2, (0.75, 0.25), 1);
        approx(fx, 0.5);
        approx(fy, 0.5);
    }

    #[test]
    fn cursor_at_center_keeps_center() {
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 60, 30, 1, (0.5, 0.5), 2);
        approx(fx, 0.5);
        approx(fy, 0.5);
    }

    #[test]
    fn cursor_at_right_edge_anchors_right() {
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 110, 55, 1, (0.5, 0.5), 2);
        approx(fx, 0.75);
        approx(fy, 0.75);
    }

    #[test]
    fn repeated_zoom_keeps_right_edge_anchored() {
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 110, 55, 2, (0.75, 0.75), 3);
        approx(fx, 5.0 / 6.0);
        approx(fy, 5.0 / 6.0);
    }

    #[test]
    fn cursor_left_of_rect_clamps_to_left() {
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 0, 0, 1, (0.5, 0.5), 2);
        approx(fx, 0.25);
        approx(fy, 0.25);
    }

    #[test]
    fn zero_width_rect_uses_center_axis() {
        let r = Rect { x: 0, y: 0, width: 0, height: 50 };
        let (fx, _fy) = focal_for_zoom_at_cursor(r, 5, 25, 1, (0.5, 0.5), 2);
        approx(fx, 0.5);
    }

    #[test]
    fn zoom_one_returns_same_dimensions() {
        let img = DynamicImage::new_rgb8(64, 48);
        let out = zoom_center(&img, 1, (0.5, 0.5));
        assert_eq!(out.dimensions(), (64, 48));
    }

    #[test]
    fn off_center_focal_stays_in_bounds() {
        let img = DynamicImage::new_rgb8(64, 48);
        let out = zoom_center(&img, 3, (0.95, 0.05));
        assert_eq!(out.dimensions(), (64, 48));
    }
}
