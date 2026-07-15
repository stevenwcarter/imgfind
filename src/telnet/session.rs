//! Per-connection telnet session: negotiate, login, search, render.

/// Which screen the client is currently looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    SearchBox,
    Results,
    NoResults,
}

/// Map a cosine distance in [0, 2] to a 0-100 "match" percentage.
pub fn match_percent(distance: f32) -> u8 {
    let pct = ((1.0 - distance / 2.0) * 100.0).round();
    pct.clamp(0.0, 100.0) as u8
}

/// Given the current screen and a pressed byte, decide the next screen.
/// `has_art` is whether a result is currently rendered (affects Esc).
pub fn next_screen_on_key(current: Screen, byte: u8, has_art: bool) -> Screen {
    const ESC: u8 = 0x1b;
    match current {
        Screen::Results => Screen::SearchBox,
        Screen::NoResults => Screen::SearchBox,
        Screen::SearchBox => {
            if byte == ESC {
                if has_art { Screen::Results } else { Screen::SearchBox }
            } else {
                Screen::SearchBox
            }
        }
        Screen::Login => Screen::Login,
    }
}

/// One-line caption under the art.
pub fn caption(filename: &str, percent: u8) -> String {
    format!("{filename} \u{00b7} {percent}% match")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_percent_maps_distance_to_0_100() {
        assert_eq!(match_percent(0.0), 100); // identical
        assert_eq!(match_percent(2.0), 0); // opposite
        assert_eq!(match_percent(1.0), 50); // orthogonal
        // Clamps out-of-range distances.
        assert_eq!(match_percent(-0.5), 100);
        assert_eq!(match_percent(3.0), 0);
    }

    #[test]
    fn any_key_on_results_opens_search_box() {
        assert_eq!(next_screen_on_key(Screen::Results, b'x', true), Screen::SearchBox);
        assert_eq!(next_screen_on_key(Screen::Results, b' ', true), Screen::SearchBox);
    }

    #[test]
    fn esc_in_search_box_returns_to_results_when_art_exists() {
        // ESC = 0x1b
        assert_eq!(next_screen_on_key(Screen::SearchBox, 0x1b, true), Screen::Results);
    }

    #[test]
    fn esc_in_search_box_with_no_art_stays_in_search_box() {
        assert_eq!(next_screen_on_key(Screen::SearchBox, 0x1b, false), Screen::SearchBox);
    }

    #[test]
    fn any_key_on_no_results_opens_search_box() {
        assert_eq!(next_screen_on_key(Screen::NoResults, b'k', false), Screen::SearchBox);
    }

    #[test]
    fn caption_includes_filename_and_percent() {
        let c = caption("beach.jpg", 92);
        assert!(c.contains("beach.jpg"));
        assert!(c.contains("92%"));
    }
}
