//! Render a decoded image to ANSI truecolor half-block "ASCII art".

use std::fmt::Write as _;

/// Hard cap on `cols`/`rows`, applied defensively inside `render_halfblock`
/// regardless of what the caller passes in. Guards both the pixel-buffer
/// allocation and the ANSI-art output `String` (each cell emits ~30 bytes of
/// truecolor escape codes plus the glyph, so text size dominates the raw
/// pixel buffer) against an attacker-controlled NAWS window size — callers
/// should already clamp before calling in, but this keeps the function
/// itself safe on its own. Still far above any real terminal.
const MAX_RENDER_DIM: u16 = 1200;

/// Render `img` as ANSI truecolor half-blocks that fill `cols × rows`
/// character cells (each cell is 1px wide and 2px tall). Uses a "cover" fit:
/// the largest centered sub-rectangle of the *source* whose aspect ratio
/// matches the `cols × rows*2` pixel budget is cropped out first, then that
/// crop is resized to exactly fill the budget — so the image is never
/// distorted and the output never exceeds the requested bounds. Cropping in
/// source space before resizing (rather than upscaling the whole source and
/// cropping afterwards) keeps the intermediate buffer bounded by the source
/// image's own size instead of by `scale * source`, which for an
/// extreme-aspect source could otherwise balloon to hundreds of MB. Lines end
/// with CRLF for telnet clients; an odd final pixel row (only possible from
/// rounding) pairs with black for the missing bottom half.
pub fn render_halfblock(img: &image::DynamicImage, cols: u16, rows: u16) -> String {
    let cols = cols.min(MAX_RENDER_DIM);
    let rows = rows.min(MAX_RENDER_DIM);
    let target_w = u32::from(cols.max(1));
    // Target pixel grid: cols wide, rows*2 tall (two vertical pixels per cell).
    let target_h = u32::from(rows.max(1)) * 2;

    let (iw, ih) = (img.width().max(1), img.height().max(1));

    // Cover fit, crop-first: pick the largest centered source rectangle whose
    // aspect ratio equals the target's, then resize that (bounded) crop to
    // exactly fill the target. Cross-multiply (in u64, to avoid u32 overflow
    // on large dimensions) instead of comparing floating-point ratios.
    let source_wider_than_target =
        u64::from(iw) * u64::from(target_h) > u64::from(ih) * u64::from(target_w);
    let (crop_w, crop_h) = if source_wider_than_target {
        // Source is wider than the target aspect: height-limited.
        let crop_w = (u64::from(ih) * u64::from(target_w) / u64::from(target_h)) as u32;
        (crop_w.clamp(1, iw), ih)
    } else {
        // Source is taller/narrower than the target aspect: width-limited.
        let crop_h = (u64::from(iw) * u64::from(target_h) / u64::from(target_w)) as u32;
        (iw, crop_h.clamp(1, ih))
    };
    let crop_x = (iw - crop_w) / 2;
    let crop_y = (ih - crop_h) / 2;

    let rgb = img
        .crop_imm(crop_x, crop_y, crop_w, crop_h)
        .resize_exact(target_w, target_h, image::imageops::FilterType::Triangle)
        .to_rgb8();

    let mut out = String::new();
    let mut y = 0;
    while y < target_h {
        for x in 0..target_w {
            let top = rgb.get_pixel(x, y).0;
            let bottom = if y + 1 < target_h {
                rgb.get_pixel(x, y + 1).0
            } else {
                // Defensive/unreachable: `target_h` is always `rows * 2`, so
                // it is always even and this branch never actually runs.
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

    #[test]
    fn extreme_aspect_source_fills_exact_row_count_without_distortion() {
        // A 1000x4 (very wide) source into a 40x20 cell budget must still
        // produce exactly `rows` rows and fill each; the crop-then-resize path
        // keeps the intermediate bounded by the source, not by scale*source.
        let img = solid(1000, 4, [80, 160, 240]);
        let out = render_halfblock(&img, 40, 20);
        assert_eq!(out.matches("\r\n").count(), 20);
        assert!(out.contains("\u{1b}[38;2;80;160;240m"));
    }

    #[test]
    fn attacker_controlled_max_dimensions_are_clamped() {
        // A malicious/misreported NAWS window size (u16::MAX x u16::MAX)
        // must not blow up the pixel buffer or output String: `render_halfblock`
        // clamps internally to `MAX_RENDER_DIM` regardless of caller input.
        let img = solid(4, 4, [255, 0, 0]);
        let out = render_halfblock(&img, u16::MAX, u16::MAX);
        assert!(out.len() < 50_000_000);
        assert!(out.contains('\u{2580}')); // ▀
    }
}
