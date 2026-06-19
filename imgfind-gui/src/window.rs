//! Pure virtualization math for the moving-window grid (Approach C).
//!
//! This module is wiring-pending (used by T13–T15); the `dead_code` allow is
//! removed once those tasks wire it into `main.rs`.
#![allow(dead_code)]

use std::ops::Range;

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
pub fn window_range(first_row: usize, last_row: usize, cols: usize, total: usize) -> Range<usize> {
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

/// Returns `true` when the current render window should slide to follow the
/// scroll position.
///
/// A slide is triggered when the set of visible items comes within
/// `SLIDE_TRIGGER_ROWS * cols` items of either end of `current`, **and**
/// `current` is not already clamped at that boundary — i.e. `current.start > 0`
/// for the near-start edge, and `current.end < total` for the near-end edge.
///
/// `total` is the absolute item count; passing `current.end == total` signals
/// that the window already covers the far end and no further slide is needed.
pub fn need_slide(
    current: &Range<usize>,
    visible_first_idx: usize,
    visible_last_idx: usize,
    cols: usize,
    total: usize,
) -> bool {
    let buf = SLIDE_TRIGGER_ROWS * cols;
    // Near-start: visible window approaching the start of the render window,
    // and render window does not already begin at item 0.
    let near_start = visible_first_idx < current.start + buf && current.start > 0;
    // Near-end: visible window approaching the end of the render window,
    // and the render window has not yet reached the absolute end of the list.
    let near_end = visible_last_idx + buf > current.end && current.end < total;
    near_start || near_end
}

/// Total number of rows required to display `total_items` items in a grid
/// with `cols` columns.  Returns 0 when either argument is 0.
pub fn total_rows(total_items: usize, cols: usize) -> usize {
    if cols == 0 {
        return 0;
    }
    total_items.div_ceil(cols)
}

/// Total scrollable height in pixels for a grid with `total_items` items,
/// `cols` columns, and a row pitch of `pitch_y` pixels.
pub fn viewport_height(total_items: usize, cols: usize, pitch_y: f32) -> f32 {
    total_rows(total_items, cols) as f32 * pitch_y
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
        let r = window_range(10, 15, 4, 1000);
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
        let r = window_range(0, 3, 4, 10);
        assert_eq!(r, 0..10);
    }

    #[test]
    fn window_range_zero_total() {
        assert_eq!(window_range(0, 5, 4, 0), 0..0);
    }

    #[test]
    fn window_range_zero_cols() {
        assert_eq!(window_range(0, 5, 0, 100), 0..0);
    }

    #[test]
    fn window_range_enforces_window_max() {
        // With enough items, the window should never exceed WINDOW_MAX.
        let r = window_range(1000, 1010, 10, 100_000);
        assert!(r.len() <= WINDOW_MAX, "len={}", r.len());
    }

    // --- total_rows ---

    #[test]
    fn total_rows_rounds_up() {
        assert_eq!(total_rows(10, 4), 3); // ceil(10/4)=3
        assert_eq!(total_rows(0, 4), 0);
        assert_eq!(total_rows(8, 4), 2); // exact
        assert_eq!(total_rows(1, 4), 1); // one item fills one row
    }

    #[test]
    fn total_rows_zero_cols() {
        assert_eq!(total_rows(100, 0), 0);
    }

    // --- viewport_height ---

    #[test]
    fn viewport_height_matches_rows_times_pitch() {
        // 10 items, 4 cols → 3 rows; 3 * 208.0 = 624.0
        assert!((viewport_height(10, 4, TILE_PITCH_Y) - 624.0).abs() < f32::EPSILON);
    }

    // --- need_slide ---
    //
    // Semantics: slide when the visible item range comes within
    // SLIDE_TRIGGER_ROWS*cols items of either end of `current`, AND `current`
    // is not already clamped at that boundary.
    //
    // With SLIDE_TRIGGER_ROWS=4 and cols=4, buf = 16 items.
    //
    // Setup: current = 100..500, total = 1000 (not at either clamp).
    // Near-end trigger: visible_last_idx + buf > current.end AND current.end < total
    //   → visible_last_idx > 500-16 = 484.
    //   → e.g. visible_last at 485 → 485+16=501 > 500, and 500 < 1000 → trigger.
    // Near-start trigger: visible_first_idx < current.start + buf AND current.start > 0
    //   → visible_first_idx < 100+16 = 116.
    //   → e.g. visible_first at 115 → 115 < 116, and start(100) > 0 → trigger.

    #[test]
    fn need_slide_near_end_triggers() {
        let cur = 100..500_usize;
        // visible_last=485 → 485+16=501 > 500, and current.end(500) < total(1000) → true.
        assert!(need_slide(&cur, 460, 485, 4, 1000));
    }

    #[test]
    fn need_slide_near_start_triggers() {
        let cur = 100..500_usize;
        // visible_first=115 < 116, and current.start(100) > 0 → true.
        assert!(need_slide(&cur, 115, 140, 4, 1000));
    }

    #[test]
    fn need_slide_mid_window_no_trigger() {
        let cur = 100..500_usize;
        // visible_first=200, visible_last=220: not near either edge.
        // near_start: 200 < 116? No. near_end: 220+16=236 > 500? No.
        assert!(!need_slide(&cur, 200, 220, 4, 1000));
    }

    #[test]
    fn need_slide_at_start_clamp_no_trigger() {
        // current.start == 0 → already at the near-start clamp.
        let cur = 0..200_usize;
        // visible_first=5 would normally be near-start, but start==0 blocks it.
        assert!(!need_slide(&cur, 5, 20, 4, 200));
    }

    #[test]
    fn need_slide_at_end_clamp_no_trigger() {
        // current.end == total → already at the far-end clamp; no slide needed.
        let cur = 800..1000_usize;
        // visible_last=1000, current.end=total=1000 → current.end < total? No → false.
        assert!(!need_slide(&cur, 980, 1000, 4, 1000));
    }

    /// Regression test: window already at absolute end, user near-but-not-at
    /// the last item.  The old guard (`current.end > visible_last_idx`) was
    /// satisfied here (1000 > 999), causing a spurious re-slide on every tick.
    /// The fix uses `current.end < total` which is false (1000 == 1000).
    #[test]
    fn need_slide_at_end_clamp_near_last_item_no_spurious_slide() {
        let cur = 800..1000_usize;
        // visible_last=999 < total=1000, but current.end==total → no slide.
        assert!(!need_slide(&cur, 980, 999, 4, 1000));
    }

    /// Positive companion: when the window is NOT at the absolute end and the
    /// user is near the bottom, a slide must fire.
    #[test]
    fn need_slide_near_end_not_at_total_triggers() {
        let cur = 0..200_usize;
        // visible_last=185, buf=16 → 185+16=201 > 200, current.end(200) < total(1000) → true.
        assert!(need_slide(&cur, 180, 185, 4, 1000));
    }

    #[test]
    fn need_slide_far_from_end_no_trigger() {
        let cur = 0..200_usize;
        // visible_last=50: 50+16=66 > 200? No → false. start==0 → near_start blocked.
        assert!(!need_slide(&cur, 30, 50, 4, 200));
    }
}
