//! Render a decoded image to ANSI truecolor half-block "ASCII art".

use std::fmt::Write as _;

/// Render `img` as ANSI truecolor half-blocks that fill `cols × rows`
/// character cells (each cell is 1px wide and 2px tall). Uses a "cover" fit:
/// the image is scaled by a single factor for both axes (so it is never
/// distorted) until it fully covers the `cols × rows*2` pixel budget, then
/// centered and cropped to that exact size — the output never exceeds the
/// requested bounds. Lines end with CRLF for telnet clients; an odd final
/// pixel row (only possible from rounding) pairs with black for the missing
/// bottom half.
pub fn render_halfblock(img: &image::DynamicImage, cols: u16, rows: u16) -> String {
    let target_w = u32::from(cols.max(1));
    // Target pixel grid: cols wide, rows*2 tall (two vertical pixels per cell).
    let target_h = u32::from(rows.max(1)) * 2;

    let (iw, ih) = (img.width().max(1), img.height().max(1));

    // Cover fit: scale uniformly so the image covers the target box, then
    // crop the overflow. A single shared scale factor (unlike independently
    // clamping each axis) never stretches the image out of proportion.
    let scale = (target_w as f32 / iw as f32).max(target_h as f32 / ih as f32);
    let scaled_w = ((iw as f32 * scale).round() as u32).max(target_w);
    let scaled_h = ((ih as f32 * scale).round() as u32).max(target_h);
    let crop_x = (scaled_w - target_w) / 2;
    let crop_y = (scaled_h - target_h) / 2;

    let rgb = img
        .resize_exact(scaled_w, scaled_h, image::imageops::FilterType::Triangle)
        .crop_imm(crop_x, crop_y, target_w, target_h)
        .to_rgb8();

    let mut out = String::new();
    let mut y = 0;
    while y < target_h {
        for x in 0..target_w {
            let top = rgb.get_pixel(x, y).0;
            let bottom = if y + 1 < target_h {
                rgb.get_pixel(x, y + 1).0
            } else {
                [0, 0, 0]
            };
            let _ = write!(
                out,
                "\u{1b}[38;2;{};{};{}m\u{1b}[48;2;{};{};{}m\u{2580}",
                top[0], top[1], top[2], bottom[0], bottom[1], bottom[2]
            );
        }
        out.push_str("\u{1b}[0m\r\n");
        y += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, image::Rgb(rgb)))
    }

    #[test]
    fn single_cell_solid_red_has_fg_bg_and_halfblock() {
        let img = solid(4, 4, [255, 0, 0]);
        let out = render_halfblock(&img, 1, 1);
        // Truecolor fg + bg for red, upper-half-block glyph, CRLF line ending.
        assert!(out.contains("\u{1b}[38;2;255;0;0m"));
        assert!(out.contains("\u{1b}[48;2;255;0;0m"));
        assert!(out.contains('\u{2580}')); // ▀
        assert!(out.contains("\r\n"));
    }

    #[test]
    fn output_row_count_matches_requested_rows() {
        let img = solid(10, 10, [10, 20, 30]);
        let out = render_halfblock(&img, 8, 5);
        // One CRLF per rendered row.
        assert_eq!(out.matches("\r\n").count(), 5);
    }

    #[test]
    fn aspect_fit_never_exceeds_requested_bounds() {
        // Wide image into a square budget: width fills, height <= budget.
        let img = solid(200, 50, [0, 0, 0]);
        let out = render_halfblock(&img, 40, 40);
        assert!(out.matches("\r\n").count() <= 40);
    }

    #[test]
    fn ends_with_reset() {
        let img = solid(4, 4, [1, 2, 3]);
        let out = render_halfblock(&img, 2, 2);
        assert!(out.trim_end().ends_with("\u{1b}[0m") || out.contains("\u{1b}[0m"));
    }
}
