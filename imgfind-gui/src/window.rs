//! Pure virtualization math for the moving-window grid (Approach C).

use std::ops::Range;

use crate::grid_index::{GridCols, ItemCount};

/// Vertical pitch per tile row: 200 px tile + 8 px gap, matching `app.slint`
/// `tile-stride` (tile-size + tile-gap = 200px + 8px = 208px).
pub const TILE_PITCH_Y: f32 = 208.0;

/// Number of buffer rows added on each side of the visible range when computing
/// the render window, and the proximity threshold that triggers a slide.
pub const SLIDE_TRIGGER_ROWS: usize = 4;

/// Minimum number of items in the render window.
pub const WINDOW_MIN: usize = 200;

/// Maximum number of items in the render window.
pub const WINDOW_MAX: usize = 2000;

/// Returns `(first_row, last_row_exclusive)` — the row range whose tiles
/// intersect the current viewport.
///
/// `scroll_y` is the number of pixels scrolled from the top (negative values
/// clamp to 0).  `viewport_h` is the visible height.  `pitch_y` is the height
/// of one row including its gap.
pub fn visible_rows(scroll_y: f32, viewport_h: f32, pitch_y: f32) -> (usize, usize) {
    let top = scroll_y.max(0.0);
    let first = (top / pitch_y).floor() as usize;
    let count = (viewport_h / pitch_y).ceil() as usize + 1;
    (first, first + count)
}

/// Returns the item-index `Range` that should be rendered, given the visible
/// row range and the total item count.
///
/// The range is expanded by `SLIDE_TRIGGER_ROWS` on each side as a read-ahead
/// buffer, clamped to `[0, total)`, and then bounded to
/// `[WINDOW_MIN, WINDOW_MAX]` items.
pub fn window_range(
    first_row: usize,
    last_row: usize,
    cols: GridCols,
    total: ItemCount,
) -> Range<usize> {
    let cols = cols.get();
    let total = total.get();
    if cols == 0 || total == 0 {
        return 0..0;
    }
    let buf = SLIDE_TRIGGER_ROWS;
    let start_row = first_row.saturating_sub(buf);
    let end_row = last_row + buf;
    let mut start = (start_row * cols).min(total);
    let mut end = (end_row * cols).min(total);

    // Enforce minimum window size where possible.
    if end - start < WINDOW_MIN {
        end = (start + WINDOW_MIN).min(total);
        // If end still cannot satisfy WINDOW_MIN, grow start backward.
        start = end.saturating_sub(WINDOW_MIN).min(start);
    }
    if end - start > WINDOW_MAX {
        end = start + WINDOW_MAX;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- visible_rows ---

    #[test]
    fn visible_rows_from_scroll() {
        // pitch=200, scrolled 400px: first row = floor(400/200) = 2.
        // count = ceil(800/200)+1 = 4+1 = 5 rows; last_exclusive = 2+5 = 7.
        let (f, l) = visible_rows(400.0, 800.0, 200.0);
        assert_eq!(f, 2);
        assert_eq!(l, 7);
    }

    #[test]
    fn visible_rows_top_clamps_to_zero() {
        let (f, _) = visible_rows(-10.0, 800.0, 200.0);
        assert_eq!(f, 0);
    }

    #[test]
    fn visible_rows_exact_boundary() {
        // Scrolled exactly one pitch: first row = 1, count = ceil(200/200)+1 = 2, last = 3.
        let (f, l) = visible_rows(200.0, 200.0, 200.0);
        assert_eq!(f, 1);
        assert_eq!(l, 3);
    }

    // --- window_range ---

    #[test]
    fn window_range_clamps_and_buffers() {
        // cols=4, total=1000, visible rows 10..15.
        // Buffered start row = 10-4=6 → start item = 24.
        // Buffered end row = 15+4=19 → end item = 76.
        // 76-24=52 < WINDOW_MIN=200, so the window expands: end = 24+200 = 224.
        let r = window_range(10, 15, GridCols(4), ItemCount(1000));
        // The brief's invariants: start ≤ buffered-start, end ≥ visible-end, clamped.
        assert!(r.start <= (10 - 4) * 4, "start={}", r.start);
        assert!(r.end >= 15 * 4, "end={}", r.end);
        assert!(r.end <= 1000);
        assert!(r.len() <= WINDOW_MAX);
        // Concrete values: WINDOW_MIN expansion wins.
        assert_eq!(r.start, 24);
        assert_eq!(r.end, 224); // 24 + WINDOW_MIN
    }

    #[test]
    fn window_range_total_smaller_than_window() {
        // total=10 < WINDOW_MIN=200: should return 0..10.
        let r = window_range(0, 3, GridCols(4), ItemCount(10));
        assert_eq!(r, 0..10);
    }

    #[test]
    fn window_range_zero_total() {
        assert_eq!(window_range(0, 5, GridCols(4), ItemCount(0)), 0..0);
    }

    #[test]
    fn window_range_zero_cols() {
        assert_eq!(window_range(0, 5, GridCols(0), ItemCount(100)), 0..0);
    }

    #[test]
    fn window_range_enforces_window_max() {
        // With enough items, the window should never exceed WINDOW_MAX.
        let r = window_range(1000, 1010, GridCols(10), ItemCount(100_000));
        assert!(r.len() <= WINDOW_MAX, "len={}", r.len());
    }
}
