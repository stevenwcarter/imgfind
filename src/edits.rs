//! Per-image, non-destructive adjustments (exposure, saturation, blacks, whites, brightness, contrast).
//!
//! Edits live only in the DB and are baked into thumbnails at the generation
//! seam; the original file is never modified. See
//! `docs/superpowers/specs/2026-06-24-lightbox-image-adjustments-design.md`.

/// Adjustments applied to a single image. Identity (all fields 0) is a no-op.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageEdits {
    /// Exposure in photographic EV stops; linear gain = 2^exposure.
    pub exposure: f32,
    /// Saturation, -100..=100 (0 neutral; -100 grayscale, +100 doubles chroma).
    pub saturation: f32,
    /// Blacks, -100..=100 (shadow-weighted lift/drop).
    pub blacks: f32,
    /// Whites, -100..=100 (highlight-weighted lift/drop).
    pub whites: f32,
    /// Brightness, -100..=100 (midtone gamma lift).
    pub brightness: f32,
    /// Contrast, -100..=100 (S-pivot at mid-gray).
    pub contrast: f32,
}

impl ImageEdits {
    pub const EXPOSURE_MIN: f32 = -3.0;
    pub const EXPOSURE_MAX: f32 = 3.0;
    pub const ADJ_MIN: f32 = -100.0;
    pub const ADJ_MAX: f32 = 100.0;

    pub fn identity() -> Self {
        Self {
            exposure: 0.0,
            saturation: 0.0,
            blacks: 0.0,
            whites: 0.0,
            brightness: 0.0,
            contrast: 0.0,
        }
    }

    /// True when no adjustment would alter the image.
    pub fn is_identity(&self) -> bool {
        self.exposure.abs() < f32::EPSILON
            && self.saturation.abs() < f32::EPSILON
            && self.blacks.abs() < f32::EPSILON
            && self.whites.abs() < f32::EPSILON
            && self.brightness.abs() < f32::EPSILON
            && self.contrast.abs() < f32::EPSILON
    }

    /// Clamp all controls into their supported ranges.
    pub fn clamped(self) -> Self {
        Self {
            exposure: self.exposure.clamp(Self::EXPOSURE_MIN, Self::EXPOSURE_MAX),
            saturation: self.saturation.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
            blacks: self.blacks.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
            whites: self.whites.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
            brightness: self.brightness.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
            contrast: self.contrast.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
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
/// Full-slider display-space shift for Blacks at the extreme (tapers to 0 across mids).
pub const BLACK_STRENGTH: f32 = 0.5;
/// Full-slider display-space shift for Whites at the extreme (tapers to 0 across mids).
pub const WHITE_STRENGTH: f32 = 0.5;

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

/// GLSL-style smoothstep: 0 below `a`, 1 above `b`, smooth Hermite between.
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    if (b - a).abs() < f32::EPSILON {
        return if x < a { 0.0 } else { 1.0 };
    }
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 1.0 in deep shadow, tapering to 0.0 by mid-gray.
fn shadow_weight(d: f32) -> f32 {
    1.0 - smoothstep(0.0, 0.5, d)
}

/// 0.0 below mid-gray, rising to 1.0 at white.
fn highlight_weight(d: f32) -> f32 {
    smoothstep(0.5, 1.0, d)
}

/// Scale chroma around Rec.709 linear luma. `sat` in -100..=100.
pub fn apply_saturation(r: f32, g: f32, b: f32, sat: f32) -> (f32, f32, f32) {
    let f = 1.0 + sat / 100.0;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    (
        (y + f * (r - y)).max(0.0),
        (y + f * (g - y)).max(0.0),
        (y + f * (b - y)).max(0.0),
    )
}

/// Shadow-weighted lift/drop on a display value. `blacks` in -100..=100.
pub fn apply_blacks(d: f32, blacks: f32) -> f32 {
    d + (blacks / 100.0) * BLACK_STRENGTH * shadow_weight(d)
}

/// Highlight-weighted lift/drop on a display value. `whites` in -100..=100.
pub fn apply_whites(d: f32, whites: f32) -> f32 {
    d + (whites / 100.0) * WHITE_STRENGTH * highlight_weight(d)
}

/// Midtone gamma lift; endpoints 0 and 1 fixed. `brightness` in -100..=100.
pub fn apply_brightness(d: f32, brightness: f32) -> f32 {
    let gamma = 2f32.powf(-brightness / 100.0);
    d.clamp(0.0, 1.0).powf(gamma)
}

/// Linear contrast pivoted at mid-gray. `contrast` in -100..=100.
pub fn apply_contrast(d: f32, contrast: f32) -> f32 {
    0.5 + (d - 0.5) * (1.0 + contrast / 100.0)
}

/// Map one already-exposed linear channel through roll-off → gamma → the
/// display-space tone controls → 8-bit. Exposure and saturation are applied at
/// the pixel level before this (saturation is cross-channel).
pub fn channel_to_display(linear_exposed: f32, edits: &ImageEdits) -> u8 {
    let rolled = highlight_rolloff(linear_exposed.max(0.0)).clamp(0.0, 1.0);
    let mut d = linear_to_srgb(rolled);
    d = apply_blacks(d, edits.blacks);
    d = apply_whites(d, edits.whites);
    d = apply_brightness(d, edits.brightness);
    d = apply_contrast(d, edits.contrast);
    (d.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Exposure → highlight roll-off → sRGB gamma → 8-bit (no other controls).
pub fn tonemap_channel(linear: f32, ev: f32) -> u8 {
    channel_to_display(linear.max(0.0) * 2f32.powf(ev), &ImageEdits::identity())
}

/// A linear-light, scene-referred RGB image (sRGB primaries). Values are nominally
/// in [0,1] with sensor white at ~1.0; RAW highlight headroom lives just below 1.0.
#[derive(Clone)]
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

    /// Apply the full adjustment pipeline (exposure → saturation → roll-off →
    /// gamma → blacks → whites → brightness → contrast → 8-bit).
    pub fn render(&self, edits: &ImageEdits) -> image::RgbImage {
        let e = edits.clamped();
        let gain = 2f32.powf(e.exposure);
        let mut out = image::RgbImage::new(self.0.width(), self.0.height());
        for (o, p) in out.pixels_mut().zip(self.0.pixels()) {
            let (r, g, b) = apply_saturation(p[0] * gain, p[1] * gain, p[2] * gain, e.saturation);
            *o = image::Rgb([
                channel_to_display(r, &e),
                channel_to_display(g, &e),
                channel_to_display(b, &e),
            ]);
        }
        out
    }
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
        let dark = lin.render(&ImageEdits {
            exposure: 0.0,
            ..ImageEdits::identity()
        });
        let bright = lin.render(&ImageEdits {
            exposure: 1.0,
            ..ImageEdits::identity()
        });
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
        let out = LinearRgb::from_srgb8(&img).render(&ImageEdits {
            exposure: 0.0,
            ..ImageEdits::identity()
        });
        let p = out.get_pixel(0, 0);
        for c in 0..3 {
            assert!((p[c] as i32 - img.get_pixel(0, 0)[c] as i32).abs() <= 1);
        }
    }

    #[test]
    fn all_neutral_is_identity() {
        assert!(ImageEdits::identity().is_identity());
        assert!(
            ImageEdits {
                exposure: 0.0,
                saturation: 0.0,
                blacks: 0.0,
                whites: 0.0,
                brightness: 0.0,
                contrast: 0.0,
            }
            .is_identity()
        );
    }

    #[test]
    fn any_nonzero_control_is_not_identity() {
        for e in [
            ImageEdits {
                saturation: 10.0,
                ..ImageEdits::identity()
            },
            ImageEdits {
                blacks: -5.0,
                ..ImageEdits::identity()
            },
            ImageEdits {
                whites: 5.0,
                ..ImageEdits::identity()
            },
            ImageEdits {
                brightness: 1.0,
                ..ImageEdits::identity()
            },
            ImageEdits {
                contrast: -1.0,
                ..ImageEdits::identity()
            },
        ] {
            assert!(!e.is_identity());
        }
    }

    #[test]
    fn clamp_bounds_each_control() {
        let c = ImageEdits {
            exposure: 9.0,
            saturation: 999.0,
            blacks: -999.0,
            whites: 999.0,
            brightness: -999.0,
            contrast: 999.0,
        }
        .clamped();
        assert_eq!(c.exposure, 3.0);
        assert_eq!(c.saturation, 100.0);
        assert_eq!(c.blacks, -100.0);
        assert_eq!(c.whites, 100.0);
        assert_eq!(c.brightness, -100.0);
        assert_eq!(c.contrast, 100.0);
    }

    #[test]
    fn smoothstep_anchors() {
        assert_eq!(smoothstep(0.0, 0.5, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 0.5, 1.0), 1.0);
        let mid = smoothstep(0.0, 1.0, 0.5);
        assert!((mid - 0.5).abs() < 1e-6);
    }

    #[test]
    fn weights_in_range_and_at_anchors() {
        assert!((shadow_weight(0.0) - 1.0).abs() < 1e-6);
        assert!(shadow_weight(0.6) < 1e-6);
        assert!(highlight_weight(0.4) < 1e-6);
        assert!((highlight_weight(1.0) - 1.0).abs() < 1e-6);
        for d in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            assert!((0.0..=1.0).contains(&shadow_weight(d)));
            assert!((0.0..=1.0).contains(&highlight_weight(d)));
        }
    }

    #[test]
    fn display_controls_neutral_are_noops() {
        for d in [0.0f32, 0.2, 0.5, 0.8, 1.0] {
            assert!((apply_blacks(d, 0.0) - d).abs() < 1e-6);
            assert!((apply_whites(d, 0.0) - d).abs() < 1e-6);
            assert!((apply_brightness(d, 0.0) - d).abs() < 1e-6);
            assert!((apply_contrast(d, 0.0) - d).abs() < 1e-6);
        }
    }

    #[test]
    fn brightness_raises_midtones_endpoints_fixed() {
        assert!(apply_brightness(0.5, 50.0) > 0.5);
        assert!(apply_brightness(0.5, -50.0) < 0.5);
        assert!((apply_brightness(0.0, 50.0)).abs() < 1e-6);
        assert!((apply_brightness(1.0, 50.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn contrast_pivots_at_mid_gray() {
        assert!((apply_contrast(0.5, 80.0) - 0.5).abs() < 1e-6);
        assert!(apply_contrast(0.8, 80.0) > 0.8);
        assert!(apply_contrast(0.2, 80.0) < 0.2);
        // negative contrast pulls toward 0.5
        assert!(apply_contrast(0.8, -80.0) < 0.8 && apply_contrast(0.8, -80.0) > 0.5);
    }

    #[test]
    fn blacks_lift_shadows_whites_lift_highlights() {
        assert!(apply_blacks(0.05, 60.0) > 0.05);
        assert!(apply_blacks(0.05, -60.0) < 0.05);
        assert!(apply_whites(0.95, 60.0) > 0.95);
        assert!(apply_whites(0.95, -60.0) < 0.95);
        // midtone barely moved by either
        assert!((apply_blacks(0.5, 100.0) - 0.5).abs() < 0.05);
        assert!((apply_whites(0.5, 100.0) - 0.5).abs() < 0.05);
    }

    #[test]
    fn saturation_extremes() {
        // -100 => all channels collapse to luma (equal)
        let (r, g, b) = apply_saturation(0.8, 0.2, 0.1, -100.0);
        assert!((r - g).abs() < 1e-6 && (g - b).abs() < 1e-6);
        // +100 widens the spread vs neutral
        let (r1, _, b1) = apply_saturation(0.8, 0.2, 0.1, 0.0);
        let (r2, _, b2) = apply_saturation(0.8, 0.2, 0.1, 100.0);
        assert!((r2 - b2).abs() > (r1 - b1).abs());
    }

    #[test]
    fn render_all_neutral_roundtrips_srgb8() {
        let mut img = image::RgbImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgb([50, 128, 200]));
        let out = LinearRgb::from_srgb8(&img).render(&ImageEdits::identity());
        let p = out.get_pixel(0, 0);
        for c in 0..3 {
            assert!((p[c] as i32 - img.get_pixel(0, 0)[c] as i32).abs() <= 1);
        }
    }
}
