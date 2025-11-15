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
/// current_view_area tracks the current crop area in original image coordinates
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
        return (img.clone(), 0.0, 0.0, (0, 0, img_w, img_h));
    }
    
    // Get the current view area or default to full image
    let (view_x, view_y, view_w, view_h) = current_view_area.unwrap_or((0, 0, img_w, img_h));
    
    // Calculate crop dimensions based on terminal aspect ratio and zoom level
    // This determines how much of the current view area we want to see
    let (crop_w, crop_h) = calculate_terminal_aware_crop_size(view_w, view_h, zoom, area);
    
    // Calculate mouse position relative to current view area if provided
    let (target_x, target_y) = if let Some((mouse_x, mouse_y)) = mouse_pos {
        if let Some(area) = area {
            // Convert mouse position to coordinates within the current view area
            let rel_x = mouse_x as f32 / area.width as f32;
            let rel_y = mouse_y as f32 / area.height as f32;
            
            // Map to actual coordinates within current view area
            let target_x = view_x as f32 + rel_x * view_w as f32;
            let target_y = view_y as f32 + rel_y * view_h as f32;
            
            (target_x, target_y)
        } else {
            // Default to center of current view area
            (view_x as f32 + view_w as f32 / 2.0, view_y as f32 + view_h as f32 / 2.0)
        }
    } else {
        // Default to center of current view area
        (view_x as f32 + view_w as f32 / 2.0, view_y as f32 + view_h as f32 / 2.0)
    };
    
    // Calculate new crop area centered on target position
    let new_crop_x = (target_x - crop_w as f32 / 2.0).clamp(view_x as f32, (view_x + view_w - crop_w) as f32) as u32;
    let new_crop_y = (target_y - crop_h as f32 / 2.0).clamp(view_y as f32, (view_y + view_h - crop_h) as f32) as u32;
    
    // Ensure crop doesn't exceed original image bounds
    let final_crop_x = new_crop_x.clamp(0, img_w.saturating_sub(crop_w));
    let final_crop_y = new_crop_y.clamp(0, img_h.saturating_sub(crop_h));
    let final_crop_w = crop_w.min(img_w - final_crop_x);
    let final_crop_h = crop_h.min(img_h - final_crop_y);
    
    let cropped = img.crop_imm(final_crop_x, final_crop_y, final_crop_w, final_crop_h);
    
    // Calculate new pan offsets based on where we actually cropped
    let center_offset_x = (target_x - (final_crop_x as f32 + final_crop_w as f32 / 2.0)) / (view_w as f32 / 2.0);
    let center_offset_y = (target_y - (final_crop_y as f32 + final_crop_h as f32 / 2.0)) / (view_h as f32 / 2.0);
    
    let new_pan_x = center_offset_x.clamp(-1.0, 1.0);
    let new_pan_y = center_offset_y.clamp(-1.0, 1.0);
    
    // Return cropped image, pan offsets, and new view area in original image coordinates
    (cropped, new_pan_x, new_pan_y, (final_crop_x, final_crop_y, final_crop_w, final_crop_h))
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
                    current_view_area
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
                    current_view_area: Some(new_view_area),
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
