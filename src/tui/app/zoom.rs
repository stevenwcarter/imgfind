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
const ZOOM_BASE: f32 = 1.2;
const SCROLL_DEBOUNCE_MS: u64 = 50;

/// Zoom into an image with optional mouse position for centering
/// If mouse_pos is None, zooms into center. If Some((x, y)), zooms toward that position.
/// Area is used to calculate aspect ratio accounting for terminal cell dimensions (2:1 ratio)
pub fn zoom_with_mouse_position(
    img: &DynamicImage,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
    mouse_pos: Option<(u16, u16)>,
    area: Option<Rect>,
) -> (DynamicImage, f32, f32) {
    let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    let (img_w, img_h) = img.dimensions();
    
    // At zoom level 1.0, return the full image without any cropping
    if (zoom - 1.0).abs() < 0.01 {
        return (img.clone(), 0.0, 0.0);
    }
    
    // Calculate crop dimensions based on terminal aspect ratio and zoom level
    let (crop_w, crop_h) = calculate_terminal_aware_crop_size(img_w, img_h, zoom, area);
    
    // Calculate new pan offsets if mouse position is provided
    let (new_pan_x, new_pan_y) = if let Some((mouse_x, mouse_y)) = mouse_pos {
        if let Some(area) = area {
            // Convert mouse position to normalized coordinates (-1.0 to 1.0)
            let norm_x = (mouse_x as f32 / area.width as f32) * 2.0 - 1.0;
            let norm_y = (mouse_y as f32 / area.height as f32) * 2.0 - 1.0;
            
            // Blend current pan with mouse-targeted pan based on zoom level
            let blend_factor = 0.3; // How much to move toward mouse position
            (
                pan_x + (norm_x - pan_x) * blend_factor,
                pan_y + (norm_y - pan_y) * blend_factor,
            )
        } else {
            (pan_x, pan_y)
        }
    } else {
        (pan_x, pan_y)
    };
    
    // Smoothly clamp pan offsets to prevent showing empty space
    let (clamped_pan_x, clamped_pan_y) = smooth_clamp_pan_offsets(
        new_pan_x, new_pan_y, crop_w, crop_h, img_w, img_h
    );
    
    // Calculate crop position based on clamped pan offsets
    let center_x = img_w as f32 / 2.0;
    let center_y = img_h as f32 / 2.0;
    
    let max_offset_x = (img_w as f32 - crop_w as f32) / 2.0;
    let max_offset_y = (img_h as f32 - crop_h as f32) / 2.0;
    
    let crop_x = (center_x - crop_w as f32 / 2.0 + clamped_pan_x * max_offset_x)
        .clamp(0.0, (img_w - crop_w) as f32) as u32;
    let crop_y = (center_y - crop_h as f32 / 2.0 + clamped_pan_y * max_offset_y)
        .clamp(0.0, (img_h - crop_h) as f32) as u32;
    
    let cropped = img.crop_imm(crop_x, crop_y, crop_w, crop_h);
    
    // Don't resize back to original dimensions - let ratatui-image handle scaling
    // This prevents stretching and maintains aspect ratio
    (cropped, clamped_pan_x, clamped_pan_y)
}

/// Calculate crop dimensions that match terminal aspect ratio
/// Takes into account that terminal cells are 2:1 height/width ratio
fn calculate_terminal_aware_crop_size(
    img_w: u32,
    img_h: u32,
    zoom: f32,
    area: Option<Rect>,
) -> (u32, u32) {
    // If no area provided, fall back to simple center crop
    let Some(area) = area else {
        let crop_w = (img_w as f32 / zoom) as u32;
        let crop_h = (img_h as f32 / zoom) as u32;
        return (crop_w, crop_h);
    };
    
    // Calculate terminal aspect ratio accounting for 2:1 cell ratio
    // Terminal cells are typically 2x taller than wide, so we adjust
    let terminal_aspect = (area.width as f32) / (area.height as f32 * 2.0);
    
    // Start with the available area in the image at this zoom level
    // We want to maximize the use of terminal space while zooming
    let base_crop_area = (img_w * img_h) as f32 / (zoom * zoom);
    
    // Calculate crop dimensions that match terminal aspect ratio
    // while maintaining the desired zoom area
    let crop_h = (base_crop_area / terminal_aspect).sqrt();
    let crop_w = crop_h * terminal_aspect;
    
    // Ensure crop doesn't exceed image bounds
    let final_crop_w = (crop_w as u32).min(img_w);
    let final_crop_h = (crop_h as u32).min(img_h);
    
    // If the calculated crop is too big, scale it down proportionally
    if final_crop_w < crop_w as u32 {
        let scale = final_crop_w as f32 / crop_w;
        let scaled_crop_h = (crop_h * scale) as u32;
        (final_crop_w, scaled_crop_h.min(img_h))
    } else if final_crop_h < crop_h as u32 {
        let scale = final_crop_h as f32 / crop_h;
        let scaled_crop_w = (crop_w * scale) as u32;
        (scaled_crop_w.min(img_w), final_crop_h)
    } else {
        (final_crop_w, final_crop_h)
    }
}

/// Smoothly clamp pan offsets to prevent empty space around edges
fn smooth_clamp_pan_offsets(
    pan_x: f32,
    pan_y: f32,
    crop_w: u32,
    crop_h: u32,
    img_w: u32,
    img_h: u32,
) -> (f32, f32) {
    // If crop is larger than or equal to image, no panning needed
    if crop_w >= img_w || crop_h >= img_h {
        return (0.0, 0.0);
    }
    
    // Calculate maximum allowed pan based on crop and image dimensions
    let max_pan_x = (img_w - crop_w) as f32 / (2.0 * ((img_w - crop_w) as f32 / 2.0).max(1.0));
    let max_pan_y = (img_h - crop_h) as f32 / (2.0 * ((img_h - crop_h) as f32 / 2.0).max(1.0));
    
    let clamped_x = pan_x.clamp(-max_pan_x, max_pan_x);
    let clamped_y = pan_y.clamp(-max_pan_y, max_pan_y);
    
    (clamped_x, clamped_y)
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
pub fn handle_zoom_image_with_mouse(app: &mut App, zoom: Option<u8>, mouse_pos: Option<(u16, u16)>) {
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
                let area = if let Some(main_area) = get_main_image_area(app) {
                    Some(main_area)
                } else {
                    None
                };
                
                let (image, new_pan_x, new_pan_y) = zoom_with_mouse_position(
                    &base_image, 
                    zoom_level, 
                    pan_x, 
                    pan_y, 
                    mouse_pos,
                    area
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
