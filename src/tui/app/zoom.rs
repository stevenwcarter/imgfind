use image::{DynamicImage, GenericImageView};
use ratatui::layout::Rect;
use ratatui_image::thread::ThreadProtocol;
use std::time::Instant;
use tokio::sync::mpsc::unbounded_channel;
use tracing::debug;

use crate::tui::{app::App, app::ImageEntry};

// Zoom configuration constants
const MAX_ZOOM: f32 = 4.0;
const MIN_ZOOM: f32 = 1.0;
const ZOOM_BASE: f32 = 1.1;
const SCROLL_DEBOUNCE_MS: u64 = 50;

/// Calculate where an image is displayed within a terminal area
/// Returns (display_x, display_y, display_w, display_h) in terminal coordinates
fn calculate_image_display_area(
    img_w: u32,
    img_h: u32,
    terminal_area: Rect,
) -> (u16, u16, u16, u16) {
    // Calculate terminal aspect ratio accounting for 2:1 cell ratio
    let terminal_aspect = (terminal_area.width as f32) / (terminal_area.height as f32 * 2.0);
    debug!("Terminal aspect ratio is {}", terminal_aspect);
    let image_aspect = img_w as f32 / img_h as f32;
    debug!("Image aspect ratio is {}", image_aspect);

    let (display_w, display_h) = if image_aspect > terminal_aspect {
        // Image is wider than terminal - constrain by width
        (
            terminal_area.width,
            (terminal_area.width as f32 / image_aspect) as u16,
        )
    } else {
        // Image is taller than terminal - constrain by height
        (
            (terminal_area.height as f32 * 2.0 * image_aspect) as u16,
            terminal_area.height,
        )
    };

    // Center the display area within the terminal area
    let display_x = terminal_area.x + (terminal_area.width - display_w) / 2;
    let display_y = terminal_area.y + (terminal_area.height - display_h) / 2;

    (display_x, display_y, display_w, display_h)
}

/// Zoom into an image with optional mouse position for centering
/// Properly maps mouse coordinates to image coordinates accounting for display area
pub fn zoom_with_mouse_position(
    img: &DynamicImage,
    zoom: f32,
    _pan_x: f32,
    _pan_y: f32,
    mouse_pos: Option<(u16, u16)>,
    area: Option<Rect>,
    current_view_area: Option<(u32, u32, u32, u32)>,
) -> (DynamicImage, f32, f32, (u32, u32, u32, u32)) {
    let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    let (img_w, img_h) = img.dimensions();

    // At zoom level 1.0, return the full image without any cropping
    if (zoom - 1.0).abs() < 0.01 {
        debug!("Zoom level 1.0 - returning full image");
        return (img.clone(), 0.0, 0.0, (0, 0, img_w, img_h));
    }

    // Get the current view area or default to full image
    let (view_x, view_y, view_w, view_h) = current_view_area.unwrap_or((0, 0, img_w, img_h));

    // Calculate target coordinates for zoom center
    let (target_x, target_y) = if let Some((mouse_x, mouse_y)) = mouse_pos {
        if let Some(terminal_area) = area {
            if current_view_area.is_none() {
                // First zoom: need to map from terminal display area to image coordinates
                debug!("First zoom - mapping from display area to image coordinates");
                let (display_x, display_y, display_w, display_h) =
                    calculate_image_display_area(img_w, img_h, terminal_area);

                // Check if mouse is within the image display area
                if mouse_x >= display_x
                    && mouse_x < display_x + display_w
                    && mouse_y >= display_y
                    && mouse_y < display_y + display_h
                {
                    // Map mouse position to image coordinates
                    let rel_x = (mouse_x - display_x) as f32 / display_w as f32;
                    let rel_y = (mouse_y - display_y) as f32 / display_h as f32;

                    let target_x = rel_x * img_w as f32;
                    let target_y = rel_y * img_h as f32;

                    (target_x, target_y)
                } else {
                    // Mouse outside image, default to center
                    (img_w as f32 / 2.0, img_h as f32 / 2.0)
                }
            } else {
                // Subsequent zooms: mouse maps directly to current view area
                // Since the cropped image fills the terminal area
                debug!(
                    "Subsequent zoom - mapping directly to current view area: {:?}",
                    current_view_area
                );
                let rel_x = mouse_x as f32 / terminal_area.width as f32;
                let rel_y = mouse_y as f32 / terminal_area.height as f32;

                let target_x = view_x as f32 + rel_x * view_w as f32;
                let target_y = view_y as f32 + rel_y * view_h as f32;

                (target_x, target_y)
            }
        } else {
            // No terminal area provided, default to center
            (
                view_x as f32 + view_w as f32 / 2.0,
                view_y as f32 + view_h as f32 / 2.0,
            )
        }
    } else {
        // No mouse position, default to center of current view area
        (
            view_x as f32 + view_w as f32 / 2.0,
            view_y as f32 + view_h as f32 / 2.0,
        )
    };

    // Calculate crop dimensions that will fill the terminal area
    let (crop_w, crop_h) = if let Some(terminal_area) = area {
        // Calculate crop size that matches terminal aspect ratio
        let terminal_aspect = (terminal_area.width as f32) / (terminal_area.height as f32 * 2.0);

        // Base crop area from zoom level
        let base_area = (view_w * view_h) as f32 / (zoom * zoom);

        // Calculate dimensions matching terminal aspect ratio
        let crop_h = (base_area / terminal_aspect).sqrt();
        let crop_w = crop_h * terminal_aspect;

        (crop_w as u32, crop_h as u32)
    } else {
        // Fallback: simple center crop
        ((view_w as f32 / zoom) as u32, (view_h as f32 / zoom) as u32)
    };

    // Calculate new crop area centered on target position
    let half_crop_w = crop_w as f32 / 2.0;
    let half_crop_h = crop_h as f32 / 2.0;

    let new_crop_x = (target_x - half_crop_w)
        .max(view_x as f32)
        .min((view_x + view_w) as f32 - crop_w as f32);
    let new_crop_y = (target_y - half_crop_h)
        .max(view_y as f32)
        .min((view_y + view_h) as f32 - crop_h as f32);

    // Ensure crop doesn't exceed original image bounds
    let final_crop_x = (new_crop_x as u32).clamp(0, img_w.saturating_sub(crop_w));
    let final_crop_y = (new_crop_y as u32).clamp(0, img_h.saturating_sub(crop_h));
    let final_crop_w = crop_w.min(img_w - final_crop_x);
    let final_crop_h = crop_h.min(img_h - final_crop_y);

    let cropped = img.crop_imm(final_crop_x, final_crop_y, final_crop_w, final_crop_h);

    // Calculate pan offsets (for display purposes, though we're not using them much now)
    let actual_center_x = final_crop_x as f32 + final_crop_w as f32 / 2.0;
    let actual_center_y = final_crop_y as f32 + final_crop_h as f32 / 2.0;

    let pan_x = if view_w > final_crop_w {
        2.0 * (actual_center_x - (view_x as f32 + view_w as f32 / 2.0))
            / (view_w - final_crop_w) as f32
    } else {
        0.0
    }
    .clamp(-1.0, 1.0);

    let pan_y = if view_h > final_crop_h {
        2.0 * (actual_center_y - (view_y as f32 + view_h as f32 / 2.0))
            / (view_h - final_crop_h) as f32
    } else {
        0.0
    }
    .clamp(-1.0, 1.0);

    (
        cropped,
        pan_x,
        pan_y,
        (final_crop_x, final_crop_y, final_crop_w, final_crop_h),
    )
}

/// Calculate next zoom level using exponential scaling
pub fn calculate_next_zoom_level(current: f32, zoom_in: bool) -> f32 {
    if zoom_in {
        (current * ZOOM_BASE).clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        (current / ZOOM_BASE).clamp(MIN_ZOOM, MAX_ZOOM)
    }
}

/// Check if enough time has passed since last scroll event for debouncing
pub fn should_process_scroll(last_scroll_time: Option<Instant>) -> bool {
    match last_scroll_time {
        Some(last_time) => last_time.elapsed().as_millis() >= SCROLL_DEBOUNCE_MS as u128,
        None => true,
    }
}

/// Handle zoom image from App
pub fn handle_zoom_image(app: &mut App, zoom: Option<u8>) {
    handle_zoom_image_with_mouse(app, zoom, None)
}

/// Handle zoom image with mouse position from App
pub fn handle_zoom_image_with_mouse(
    app: &mut App,
    zoom: Option<u8>,
    mouse_pos: Option<(u16, u16)>,
) {
    if zoom.is_none() {
        app.zoomed_image_index = None;
        app.zoomed_image = None;
    } else {
        app.zoomed_image_index = zoom;
        if let Some(zoom_index) = zoom {
            let image_entry = app
                .images
                .get(zoom_index as usize)
                .expect("image not found");
            let image_score = image_entry.score;
            let zoom_level = app.zoom_level;
            let pan_x = if let Some(zoomed) = &app.zoomed_image {
                zoomed.pan_x
            } else {
                0.0
            };
            let pan_y = if let Some(zoomed) = &app.zoomed_image {
                zoomed.pan_y
            } else {
                0.0
            };

            let base_image = image_entry.image.clone().unwrap();

            // Get current area for aspect ratio calculations from the main layout
            // We'll approximate the main image area as the full area minus margins
            let area = get_main_image_area(app);

            // Get current view area from existing zoomed image if available
            let current_view_area = if let Some(zoomed) = &app.zoomed_image {
                zoomed.current_view_area
            } else {
                None
            };

            let (image, new_pan_x, new_pan_y, new_view_area) = zoom_with_mouse_position(
                &base_image,
                zoom_level,
                pan_x,
                pan_y,
                mouse_pos,
                area,
                current_view_area,
            );

            debug!("Image zoomed successfully to level {}", zoom_level);

            let (image_tx, image_rx) = unbounded_channel();
            let protocol = app.picker.new_resize_protocol(image.clone());
            let image_entry = ImageEntry {
                path: image_entry.path.clone(),
                score: image_score,
                rx: image_rx,
                current_zoom: zoom_level,
                pan_x: new_pan_x,
                pan_y: new_pan_y,
                last_scroll_time: None,
                // Only set current_view_area if this is not the first zoom (zoom > 1.0)
                // This ensures the first zoom uses display area mapping
                current_view_area: if (zoom_level - 1.0).abs() < 0.01 {
                    None
                } else {
                    Some(new_view_area)
                },
                image: Some(base_image),
                protocol: ThreadProtocol::new(image_tx, Some(protocol)),
            };

            app.zoomed_image = Some(image_entry);
        }
    }
}

/// Get the main image area from the app's stored render area
fn get_main_image_area(app: &App) -> Option<Rect> {
    app.current_render_area
}
