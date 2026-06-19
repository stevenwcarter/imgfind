//! Pure grid-navigation index math for the GUI thumbnail grid.
//!
//! Mirrors the behaviour of utmost's `gallery_move`, EXCEPT Left/Right clamp at
//! the global first/last tile instead of wrapping (see the design spec
//! 2026-06-18-gui-keyboard-navigation-design.md).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

impl NavDir {
    #[allow(dead_code)]
    pub fn from_i32(v: i32) -> Option<NavDir> {
        match v {
            0 => Some(NavDir::Left),
            1 => Some(NavDir::Right),
            2 => Some(NavDir::Up),
            3 => Some(NavDir::Down),
            _ => None,
        }
    }
}

/// Compute the new selected index.
///
/// - `len == 0` -> `None`
/// - `cur == None` -> `Some(0)` (first key selects the first tile)
/// - Left/Right -> linear ±1, clamped to `[0, len-1]` (crosses rows, no global wrap)
/// - Up/Down -> ±cols, clamped so it never leaves the grid and never moves when
///   already in the top/bottom row for that column
///
/// `cols` is coerced to at least 1.
#[allow(dead_code)]
pub fn move_selection(cur: Option<usize>, dir: NavDir, cols: usize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cols = cols.max(1);
    let i = match cur {
        None => return Some(0),
        Some(i) => i.min(len - 1),
    };
    let new = match dir {
        NavDir::Left => i.saturating_sub(1),
        NavDir::Right => (i + 1).min(len - 1),
        NavDir::Up => {
            if i < cols {
                i
            } else {
                i - cols
            }
        }
        NavDir::Down => {
            if i + cols >= len {
                i
            } else {
                i + cols
            }
        }
    };
    Some(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 3-column grid with 8 tiles (indices 0..=7); bottom row [6,7] is partial.
    const COLS: usize = 3;
    const LEN: usize = 8;

    #[test]
    fn no_selection_any_direction_selects_first() {
        for dir in [NavDir::Left, NavDir::Right, NavDir::Up, NavDir::Down] {
            assert_eq!(move_selection(None, dir, COLS, LEN), Some(0));
        }
    }

    #[test]
    fn left_crosses_row_boundary() {
        // index 3 is the first tile of row 1; left -> 2, the last tile of row 0.
        assert_eq!(move_selection(Some(3), NavDir::Left, COLS, LEN), Some(2));
    }

    #[test]
    fn left_at_first_tile_stays_no_global_wrap() {
        assert_eq!(move_selection(Some(0), NavDir::Left, COLS, LEN), Some(0));
    }

    #[test]
    fn right_crosses_row_boundary() {
        // index 2 is the last tile of row 0; right -> 3, the first tile of row 1.
        assert_eq!(move_selection(Some(2), NavDir::Right, COLS, LEN), Some(3));
    }

    #[test]
    fn right_at_last_tile_stays_no_global_wrap() {
        assert_eq!(
            move_selection(Some(LEN - 1), NavDir::Right, COLS, LEN),
            Some(LEN - 1)
        );
    }

    #[test]
    fn up_from_top_row_stays() {
        assert_eq!(move_selection(Some(1), NavDir::Up, COLS, LEN), Some(1));
    }

    #[test]
    fn up_from_second_row_moves_up_one_row() {
        assert_eq!(move_selection(Some(4), NavDir::Up, COLS, LEN), Some(1));
    }

    #[test]
    fn down_from_bottom_row_stays() {
        // index 7 is in the bottom (partial) row; down stays.
        assert_eq!(move_selection(Some(7), NavDir::Down, COLS, LEN), Some(7));
    }

    #[test]
    fn down_into_partial_bottom_row_clamps() {
        // index 5 (row 1, col 2); +cols = 8 which is >= len, so it clamps (stays).
        assert_eq!(move_selection(Some(5), NavDir::Down, COLS, LEN), Some(5));
    }

    #[test]
    fn down_from_top_row_moves_down_one_row() {
        assert_eq!(move_selection(Some(0), NavDir::Down, COLS, LEN), Some(3));
    }

    #[test]
    fn zero_cols_treated_as_one() {
        // cols 0 must not panic / divide by zero; behaves as a single column.
        assert_eq!(move_selection(Some(0), NavDir::Down, 0, LEN), Some(1));
        assert_eq!(
            move_selection(Some(LEN - 1), NavDir::Down, 0, LEN),
            Some(LEN - 1)
        );
    }

    #[test]
    fn empty_grid_returns_none() {
        assert_eq!(move_selection(Some(0), NavDir::Right, COLS, 0), None);
        assert_eq!(move_selection(None, NavDir::Right, COLS, 0), None);
    }

    #[test]
    fn single_column_up_down_behave_like_prev_next() {
        // cols == 1: up/down move by one and clamp.
        assert_eq!(move_selection(Some(2), NavDir::Up, 1, 5), Some(1));
        assert_eq!(move_selection(Some(2), NavDir::Down, 1, 5), Some(3));
        assert_eq!(move_selection(Some(0), NavDir::Up, 1, 5), Some(0));
        assert_eq!(move_selection(Some(4), NavDir::Down, 1, 5), Some(4));
    }
}
