//! Pure zoom/pan math for the lightbox. No Slint types so it unit-tests cleanly;
//! `app.slint` mirrors the same clamp formula for live pan during a drag.

pub const ZOOM_MIN: f32 = 0.1;
pub const ZOOM_MAX: f32 = 8.0;
pub const ZOOM_STEP: f32 = 1.25;

pub fn clamp_zoom(z: f32) -> f32 {
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

pub fn zoom_in(z: f32) -> f32 {
    clamp_zoom(z * ZOOM_STEP)
}

pub fn zoom_out(z: f32) -> f32 {
    clamp_zoom(z / ZOOM_STEP)
}

/// Wheel/trackpad zoom: ~60px per notch, 1.1 base for smooth perceived steps.
pub fn wheel_zoom(z: f32, delta_px: f32) -> f32 {
    clamp_zoom(z * 1.1f32.powf(delta_px / 60.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "{a} !~= {b}");
    }

    #[test]
    fn zoom_clamps_to_range() {
        approx(clamp_zoom(0.001), ZOOM_MIN);
        approx(clamp_zoom(100.0), ZOOM_MAX);
        approx(clamp_zoom(1.0), 1.0);
    }

    #[test]
    fn zoom_in_out_step_and_clamp() {
        approx(zoom_in(1.0), 1.25);
        approx(zoom_out(1.0), 0.8);
        approx(zoom_in(ZOOM_MAX), ZOOM_MAX); // already at ceiling
        approx(zoom_out(ZOOM_MIN), ZOOM_MIN); // already at floor
    }

    #[test]
    fn wheel_zoom_sign_and_clamp() {
        assert!(wheel_zoom(1.0, 60.0) > 1.0, "positive delta zooms in");
        assert!(wheel_zoom(1.0, -60.0) < 1.0, "negative delta zooms out");
        approx(wheel_zoom(1.0, 0.0), 1.0);
        approx(wheel_zoom(ZOOM_MAX, 600.0), ZOOM_MAX); // clamped
    }
}
