//! Pure keyboard-chord state machine for tag shortcuts. The Slint side forwards
//! a single key string per press (only when no text input has focus); this
//! module decides the resulting action and the next pending state. No I/O.

use imgfind::colors::BrushColor;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pending {
    #[default]
    None,
    AwaitM,
    AwaitF,
    AwaitG,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleRail,
    OpenTagModal,
    PaintBrush(BrushColor),
    RepeatLast,
    LoadBrushIntoFilter(BrushColor),
    ToggleTagFilter,
    JumpFirst,
    JumpLast,
}

/// Resolve a key press given the current pending prefix.
/// Returns the next pending state and an optional action to perform.
/// Any key that doesn't complete or start a chord cancels the prefix and yields
/// no action (the caller lets such keys fall through to existing navigation).
pub fn resolve(pending: Pending, key: &str) -> (Pending, Option<Action>) {
    match pending {
        Pending::None => match key {
            "`" => (Pending::None, Some(Action::ToggleRail)),
            "t" => (Pending::None, Some(Action::OpenTagModal)),
            "m" => (Pending::AwaitM, None),
            "f" => (Pending::AwaitF, None),
            "g" => (Pending::AwaitG, None),
            "G" => (Pending::None, Some(Action::JumpLast)),
            _ => (Pending::None, None),
        },
        Pending::AwaitM => match key {
            "m" => (Pending::None, Some(Action::RepeatLast)),
            other => match BrushColor::from_letter(other) {
                Some(c) => (Pending::None, Some(Action::PaintBrush(c))),
                None => (Pending::None, None),
            },
        },
        Pending::AwaitF => match key {
            "t" => (Pending::None, Some(Action::ToggleTagFilter)),
            other => match BrushColor::from_letter(other) {
                Some(c) => (Pending::None, Some(Action::LoadBrushIntoFilter(c))),
                None => (Pending::None, None),
            },
        },
        Pending::AwaitG => match key {
            "g" => (Pending::None, Some(Action::JumpFirst)),
            _ => (Pending::None, None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backtick_toggles_rail() {
        assert_eq!(
            resolve(Pending::None, "`"),
            (Pending::None, Some(Action::ToggleRail))
        );
    }

    #[test]
    fn t_opens_modal() {
        assert_eq!(
            resolve(Pending::None, "t"),
            (Pending::None, Some(Action::OpenTagModal))
        );
    }

    #[test]
    fn m_then_color_paints() {
        let (p, a) = resolve(Pending::None, "m");
        assert_eq!((p, a), (Pending::AwaitM, None));
        assert_eq!(
            resolve(p, "r"),
            (Pending::None, Some(Action::PaintBrush(BrushColor::Red)))
        );
    }

    #[test]
    fn mm_repeats_last() {
        let (p, _) = resolve(Pending::None, "m");
        assert_eq!(resolve(p, "m"), (Pending::None, Some(Action::RepeatLast)));
    }

    #[test]
    fn f_then_color_loads_filter() {
        let (p, _) = resolve(Pending::None, "f");
        assert_eq!(
            resolve(p, "g"),
            (
                Pending::None,
                Some(Action::LoadBrushIntoFilter(BrushColor::Green))
            )
        );
    }

    #[test]
    fn ft_toggles_filter() {
        let (p, _) = resolve(Pending::None, "f");
        assert_eq!(
            resolve(p, "t"),
            (Pending::None, Some(Action::ToggleTagFilter))
        );
    }

    #[test]
    fn unknown_after_prefix_cancels_no_action() {
        let (p, _) = resolve(Pending::None, "m");
        assert_eq!(resolve(p, "z"), (Pending::None, None));
        let (p, _) = resolve(Pending::None, "f");
        assert_eq!(resolve(p, "z"), (Pending::None, None));
    }

    #[test]
    fn plain_nav_key_no_chord() {
        assert_eq!(resolve(Pending::None, "j"), (Pending::None, None));
    }

    #[test]
    fn g_arms_prefix_no_action() {
        assert_eq!(resolve(Pending::None, "g"), (Pending::AwaitG, None));
    }

    #[test]
    fn gg_jumps_first() {
        let (p, _) = resolve(Pending::None, "g");
        assert_eq!(resolve(p, "g"), (Pending::None, Some(Action::JumpFirst)));
    }

    #[test]
    fn shift_g_jumps_last() {
        assert_eq!(
            resolve(Pending::None, "G"),
            (Pending::None, Some(Action::JumpLast))
        );
    }

    #[test]
    fn g_then_other_cancels_no_action() {
        let (p, _) = resolve(Pending::None, "g");
        assert_eq!(resolve(p, "j"), (Pending::None, None));
    }

    #[test]
    fn green_brush_still_paints_after_m() {
        // Regression: `g` after `m` must still mean the green brush.
        let (p, _) = resolve(Pending::None, "m");
        assert_eq!(
            resolve(p, "g"),
            (Pending::None, Some(Action::PaintBrush(BrushColor::Green)))
        );
    }
}
