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
}
