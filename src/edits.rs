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

/// sRGB transfer function (IEC 61966-2-1) on normalized [0,1]: encoded -> linear.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB transfer function on normalized [0,1]: linear -> encoded.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Knee above which highlights roll off toward 1.0 instead of clipping.
pub const HIGHLIGHT_KNEE: f32 = 0.8;

/// Soft-knee highlight roll-off in linear light. Identity below the knee;
/// above it, a rational shoulder that is continuous in value and slope at the
/// knee and asymptotes to 1.0 (never hard-clips), so highlight gradients survive.
fn highlight_rolloff(v: f32) -> f32 {
    let k = HIGHLIGHT_KNEE;
    if v <= k {
        v
    } else {
        1.0 - (1.0 - k) * (1.0 - k) / (v - 2.0 * k + 1.0)
    }
}

/// Map one linear channel through exposure -> highlight roll-off -> sRGB gamma -> 8-bit.
pub fn tonemap_channel(linear: f32, ev: f32) -> u8 {
    let exposed = linear.max(0.0) * 2f32.powf(ev);
    let rolled = highlight_rolloff(exposed).clamp(0.0, 1.0);
    (linear_to_srgb(rolled) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// A linear-light, scene-referred RGB image (sRGB primaries). Values are nominally
/// in [0,1] with sensor white at ~1.0; RAW highlight headroom lives just below 1.0.
pub struct LinearRgb(pub image::Rgb32FImage);

impl LinearRgb {
    /// Convert an 8-bit sRGB image to linear light.
    pub fn from_srgb8(img: &image::RgbImage) -> LinearRgb {
        let mut out = image::Rgb32FImage::new(img.width(), img.height());
        for (o, p) in out.pixels_mut().zip(img.pixels()) {
            *o = image::Rgb([
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
            ]);
        }
        LinearRgb(out)
    }

    /// Wrap a *linear* 16-bit RGB image (e.g. rawler develop without the sRGB step).
    pub fn from_linear_u16(img: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>) -> LinearRgb {
        let mut out = image::Rgb32FImage::new(img.width(), img.height());
        for (o, p) in out.pixels_mut().zip(img.pixels()) {
            *o = image::Rgb([
                p[0] as f32 / 65535.0,
                p[1] as f32 / 65535.0,
                p[2] as f32 / 65535.0,
            ]);
        }
        LinearRgb(out)
    }

    /// Downscale so the longest edge is at most `max_edge` (preserve aspect, never upscale).
    pub fn downscale(&self, max_edge: u32) -> LinearRgb {
        let (w, h) = (self.0.width(), self.0.height());
        let long = w.max(h);
        if long <= max_edge || long == 0 {
            return LinearRgb(self.0.clone());
        }
        let scale = max_edge as f32 / long as f32;
        let nw = (w as f32 * scale).round().max(1.0) as u32;
        let nh = (h as f32 * scale).round().max(1.0) as u32;
        LinearRgb(image::imageops::resize(
            &self.0,
            nw,
            nh,
            image::imageops::FilterType::Lanczos3,
        ))
    }

    /// Apply exposure + highlight roll-off + sRGB gamma, producing an 8-bit image.
    pub fn render(&self, edits: &ImageEdits) -> image::RgbImage {
        let ev = edits.clamped().exposure;
        let mut out = image::RgbImage::new(self.0.width(), self.0.height());
        for (o, p) in out.pixels_mut().zip(self.0.pixels()) {
            *o = image::Rgb([
                tonemap_channel(p[0], ev),
                tonemap_channel(p[1], ev),
                tonemap_channel(p[2], ev),
            ]);
        }
        out
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
mod linear_tests {
    use super::*;

    #[test]
    fn srgb_roundtrips() {
        for &x in &[0.0f32, 0.02, 0.2, 0.5, 0.8, 1.0] {
            let r = linear_to_srgb(srgb_to_linear(x));
            assert!((r - x).abs() < 1e-4, "roundtrip {x} -> {r}");
        }
    }

    #[test]
    fn tonemap_zero_ev_is_srgb_encode() {
        // Below the knee, 0 EV is just linear->sRGB->8bit.
        let got = tonemap_channel(0.2, 0.0);
        let want = (linear_to_srgb(0.2) * 255.0).round() as u8;
        assert_eq!(got, want);
    }

    #[test]
    fn tonemap_highlights_do_not_flatten() {
        // The anti-blowout guarantee: bright values pushed +1 EV stay below 255
        // and remain a strictly increasing gradient (no hard clamp to a flat 255).
        let a = tonemap_channel(0.85, 1.0);
        let b = tonemap_channel(1.5, 1.0);
        let c = tonemap_channel(5.0, 1.0);
        assert!(a < b && b <= c, "monotonic highlights: {a} {b} {c}");
        assert!(a < 255, "0.85 lin @ +1EV must not be pure white, got {a}");
        assert!(b < 255, "1.5 lin @ +1EV must keep headroom, got {b}");
    }

    #[test]
    fn tonemap_knee_is_continuous() {
        // No visible step right at the knee at 0 EV.
        let below = tonemap_channel(HIGHLIGHT_KNEE - 0.01, 0.0) as i32;
        let above = tonemap_channel(HIGHLIGHT_KNEE + 0.01, 0.0) as i32;
        assert!((above - below).abs() <= 2, "knee jump {below}->{above}");
    }

    #[test]
    fn render_brightens_with_exposure() {
        let mut buf = image::Rgb32FImage::new(2, 2);
        for p in buf.pixels_mut() {
            *p = image::Rgb([0.2, 0.2, 0.2]);
        }
        let lin = LinearRgb(buf);
        let dark = lin.render(&ImageEdits { exposure: 0.0 });
        let bright = lin.render(&ImageEdits { exposure: 1.0 });
        assert!(bright.get_pixel(0, 0)[0] > dark.get_pixel(0, 0)[0]);
    }

    #[test]
    fn downscale_preserves_aspect_and_caps_edge() {
        let buf = image::Rgb32FImage::new(400, 200);
        let small = LinearRgb(buf).downscale(100);
        assert_eq!(small.0.width(), 100);
        assert_eq!(small.0.height(), 50);
    }

    #[test]
    fn from_srgb8_then_render_zero_ev_roundtrips() {
        // sRGB8 -> linear -> render(0 EV) returns approximately the input pixels.
        let mut img = image::RgbImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgb([50, 128, 200]));
        let out = LinearRgb::from_srgb8(&img).render(&ImageEdits { exposure: 0.0 });
        let p = out.get_pixel(0, 0);
        for c in 0..3 {
            assert!((p[c] as i32 - img.get_pixel(0, 0)[c] as i32).abs() <= 1);
        }
    }
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
