//! Pure helpers for the lightbox exposure control (UI-thread-free, unit-tested).
//!
//! These back the edit-mode slider/readout in `main.rs`: clamping the slider
//! value to the supported range and formatting the EV readout. Keeping them pure
//! lets the threading-heavy callbacks delegate the value logic to tested code.

use imgfind::edits::ImageEdits;

/// Clamp an exposure value to [`ImageEdits::EXPOSURE_MIN`, `ImageEdits::EXPOSURE_MAX`].
pub fn clamp_exposure(v: f32) -> f32 {
    v.clamp(ImageEdits::EXPOSURE_MIN, ImageEdits::EXPOSURE_MAX)
}

/// Format an exposure value as a signed EV readout, e.g. `"+1.30 EV"`,
/// `"0.00 EV"`, `"-0.75 EV"`. Negative values already carry their sign.
pub fn format_exposure(v: f32) -> String {
    if v > 0.0 {
        format!("+{v:.2} EV")
    } else if v < 0.0 {
        format!("{v:.2} EV")
    } else {
        "0.00 EV".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_to_range() {
        assert_eq!(clamp_exposure(9.0), 3.0);
        assert_eq!(clamp_exposure(-9.0), -3.0);
        assert_eq!(clamp_exposure(1.2), 1.2);
    }

    #[test]
    fn format_has_sign_and_two_decimals() {
        assert_eq!(format_exposure(1.3), "+1.30 EV");
        assert_eq!(format_exposure(0.0), "0.00 EV");
        assert_eq!(format_exposure(-0.75), "-0.75 EV");
    }
}
