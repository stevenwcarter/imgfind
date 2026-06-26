//! Per-image, non-destructive adjustments (currently exposure in EV stops).
//!
//! Edits live only in the DB and are baked into thumbnails at the generation
//! seam; the original file is never modified. See
//! `docs/superpowers/specs/2026-06-24-lightbox-image-adjustments-design.md`.

use image::DynamicImage;

/// Adjustments applied to a single image. Identity (`exposure == 0`) is a no-op.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageEdits {
    /// Exposure in photographic EV stops; output channel = input * 2^exposure.
    pub exposure: f32,
}

impl ImageEdits {
    pub const EXPOSURE_MIN: f32 = -3.0;
    pub const EXPOSURE_MAX: f32 = 3.0;

    pub fn identity() -> Self {
        Self { exposure: 0.0 }
    }

    /// True when no adjustment would alter the image.
    pub fn is_identity(&self) -> bool {
        self.exposure.abs() < f32::EPSILON
    }

    /// Clamp exposure into the supported range.
    pub fn clamped(self) -> Self {
        Self {
            exposure: self.exposure.clamp(Self::EXPOSURE_MIN, Self::EXPOSURE_MAX),
        }
    }
}

impl Default for ImageEdits {
    fn default() -> Self {
        Self::identity()
    }
}

/// Apply `edits` to `img`, returning the adjusted image.
///
/// Pure and deterministic. Identity edits return `img` unchanged (no copy).
pub fn apply_adjustments(img: DynamicImage, edits: &ImageEdits) -> DynamicImage {
    let edits = edits.clamped();
    if edits.is_identity() {
        return img;
    }
    let factor = 2f32.powf(edits.exposure);
    let mut buf = img.to_rgba8();
    for px in buf.pixels_mut() {
        for c in 0..3 {
            px.0[c] = (px.0[c] as f32 * factor).round().clamp(0.0, 255.0) as u8;
        }
    }
    DynamicImage::ImageRgba8(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgba, RgbaImage};

    fn solid(w: u32, h: u32, px: [u8; 4]) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba(px)))
    }

    #[test]
    fn identity_is_noop() {
        let img = solid(2, 2, [100, 110, 120, 255]);
        let out = apply_adjustments(img.clone(), &ImageEdits::identity());
        assert_eq!(img.to_rgba8(), out.to_rgba8());
    }

    #[test]
    fn is_identity_detects_zero() {
        assert!(ImageEdits::identity().is_identity());
        assert!(!ImageEdits { exposure: 0.5 }.is_identity());
    }

    #[test]
    fn plus_one_ev_doubles_midtone() {
        // 2^1 = 2, so 100 -> 200, alpha unchanged.
        let out = apply_adjustments(
            solid(1, 1, [100, 50, 25, 200]),
            &ImageEdits { exposure: 1.0 },
        );
        assert_eq!(
            out.to_rgba8().get_pixel(0, 0),
            &image::Rgba([200, 100, 50, 200])
        );
    }

    #[test]
    fn plus_ev_clamps_to_255() {
        let out = apply_adjustments(
            solid(1, 1, [200, 200, 200, 255]),
            &ImageEdits { exposure: 2.0 },
        );
        assert_eq!(
            out.to_rgba8().get_pixel(0, 0),
            &image::Rgba([255, 255, 255, 255])
        );
    }

    #[test]
    fn minus_one_ev_halves_midtone() {
        let out = apply_adjustments(
            solid(1, 1, [100, 80, 40, 255]),
            &ImageEdits { exposure: -1.0 },
        );
        assert_eq!(
            out.to_rgba8().get_pixel(0, 0),
            &image::Rgba([50, 40, 20, 255])
        );
    }

    #[test]
    fn clamped_bounds_exposure() {
        assert_eq!(
            ImageEdits { exposure: 9.0 }.clamped().exposure,
            ImageEdits::EXPOSURE_MAX
        );
        assert_eq!(
            ImageEdits { exposure: -9.0 }.clamped().exposure,
            ImageEdits::EXPOSURE_MIN
        );
    }
}
