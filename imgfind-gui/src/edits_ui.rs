//! Pure helpers for the lightbox exposure control (UI-thread-free, unit-tested).
//!
//! These back the edit-mode slider/readout in `main.rs`: clamping the slider
//! value to the supported range and formatting the EV readout. Keeping them pure
//! lets the threading-heavy callbacks delegate the value logic to tested code.

use imgfind::edits::ImageEdits;

/// An edit control in the lightbox editor (Exposure, Saturation, Blacks, Whites, Brightness, Contrast).
/// Used to key generic slider/format/clamp callbacks by control index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditControl {
    Exposure,
    Saturation,
    Blacks,
    Whites,
    Brightness,
    Contrast,
}

impl EditControl {
    /// Convert to a control index (0=Exposure, 1=Saturation, 2=Blacks, 3=Whites, 4=Brightness, 5=Contrast).
    /// These indices MUST match the Slint callback dispatch order.
    pub fn to_i32(self) -> i32 {
        match self {
            EditControl::Exposure => 0,
            EditControl::Saturation => 1,
            EditControl::Blacks => 2,
            EditControl::Whites => 3,
            EditControl::Brightness => 4,
            EditControl::Contrast => 5,
        }
    }

    /// Reconstruct a control from its index, or None if the index is out of range.
    pub fn from_i32(i: i32) -> Option<Self> {
        Some(match i {
            0 => EditControl::Exposure,
            1 => EditControl::Saturation,
            2 => EditControl::Blacks,
            3 => EditControl::Whites,
            4 => EditControl::Brightness,
            5 => EditControl::Contrast,
            _ => return None,
        })
    }

    /// Return the neutral (identity) value for this control (always 0.0).
    pub fn neutral(self) -> f32 {
        0.0
    }

    /// Clamp a value to the valid range for this control.
    /// Exposure uses [`ImageEdits::EXPOSURE_MIN`, `ImageEdits::EXPOSURE_MAX`];
    /// all others use [`ImageEdits::ADJ_MIN`, `ImageEdits::ADJ_MAX`].
    pub fn clamp(self, v: f32) -> f32 {
        match self {
            EditControl::Exposure => v.clamp(ImageEdits::EXPOSURE_MIN, ImageEdits::EXPOSURE_MAX),
            _ => v.clamp(ImageEdits::ADJ_MIN, ImageEdits::ADJ_MAX),
        }
    }

    /// Format a value as a human-readable string.
    /// Exposure includes the " EV" unit; others are unitless integers with a leading `+` or `-`.
    pub fn format(self, v: f32) -> String {
        match self {
            EditControl::Exposure => format_exposure(v),
            _ => {
                let n = v.round() as i32;
                if n > 0 {
                    format!("+{n}")
                } else {
                    format!("{n}")
                }
            }
        }
    }
}

/// Clamp an exposure value to [`ImageEdits::EXPOSURE_MIN`, `ImageEdits::EXPOSURE_MAX`].
pub fn clamp_exposure(v: f32) -> f32 {
    EditControl::Exposure.clamp(v)
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

    #[test]
    fn control_index_roundtrip() {
        for c in [
            EditControl::Exposure,
            EditControl::Saturation,
            EditControl::Blacks,
            EditControl::Whites,
            EditControl::Brightness,
            EditControl::Contrast,
        ] {
            assert_eq!(EditControl::from_i32(c.to_i32()), Some(c));
        }
        assert_eq!(EditControl::from_i32(99), None);
    }

    #[test]
    fn clamp_per_control_bounds() {
        assert_eq!(EditControl::Exposure.clamp(9.0), 3.0);
        assert_eq!(EditControl::Exposure.clamp(-9.0), -3.0);
        assert_eq!(EditControl::Contrast.clamp(999.0), 100.0);
        assert_eq!(EditControl::Saturation.clamp(-999.0), -100.0);
    }

    #[test]
    fn format_exposure_has_ev_others_unitless() {
        assert_eq!(EditControl::Exposure.format(1.3), "+1.30 EV");
        assert_eq!(EditControl::Contrast.format(45.0), "+45");
        assert_eq!(EditControl::Blacks.format(-30.0), "-30");
        assert_eq!(EditControl::Whites.format(0.0), "0");
    }
}
