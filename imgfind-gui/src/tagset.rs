//! Pure helpers for editing ordered, de-duplicated tag lists (brushes and the
//! "Most Recent" buffer).

/// Add whitespace-separated words to `list`, trimming and skipping duplicates,
/// preserving existing order and append order of new words.
pub fn add_words(list: &mut Vec<String>, text: &str) {
    for w in text.split_whitespace() {
        let w = w.trim();
        if !w.is_empty() && !list.iter().any(|t| t == w) {
            list.push(w.to_string());
        }
    }
}

/// Replace the list contents with the whitespace-separated words of `text`
/// (trim, dedupe, preserve order).
pub fn set_words(list: &mut Vec<String>, text: &str) {
    list.clear();
    add_words(list, text);
}

/// Remove `tag` if present.
pub fn remove(list: &mut Vec<String>, tag: &str) {
    list.retain(|t| t != tag);
}

/// Approximate per-character advance as a fraction of the font size. Tuned so
/// the estimate slightly over-reads typical proportional text (the `clip: true`
/// on the editor root is the real backstop if it under-reads).
const CHAR_WIDTH_RATIO: f32 = 0.62;
/// Fixed horizontal padding inside a pill, matching the `+ 18px` in the pill
/// markup (`width: pill-text.preferred-width + 18px`).
const PILL_PADDING: f32 = 18.0;
/// Gap between pills in a row (the inner `HorizontalLayout`'s `spacing`).
const PILL_SPACING: f32 = 4.0;
/// Padding on each side of the editor's layout (the `padding` on the layouts).
const LAYOUT_PADDING: f32 = 4.0;

/// Estimated rendered width of a single pill, in pixels.
fn pill_width(tag: &str, font_size: f32) -> f32 {
    tag.chars().count() as f32 * font_size * CHAR_WIDTH_RATIO + PILL_PADDING
}

/// Pack `tags` into rows that fit within `width_px`, estimating each pill's
/// rendered width from its character count and `font_size`. Greedy: fill a row
/// until the next pill would overflow, then start a new row. A single tag wider
/// than `width_px` gets its own row (never dropped). `width_px <= 0.0` (width not
/// yet known at first layout) returns all tags in one row.
pub fn wrap_tag_rows(tags: &[String], width_px: f32, font_size: f32) -> Vec<Vec<String>> {
    if tags.is_empty() {
        return Vec::new();
    }
    if width_px <= 0.0 {
        return vec![tags.to_vec()];
    }

    let usable = width_px - 2.0 * LAYOUT_PADDING;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_width = 0.0_f32;

    for tag in tags {
        let w = pill_width(tag, font_size);
        // Width this pill would add to the current row: its own width plus a
        // leading spacing gap when the row already has pills.
        if !current.is_empty() && current_width + PILL_SPACING + w > usable {
            rows.push(std::mem::take(&mut current));
            current_width = 0.0;
        }
        current_width += if current.is_empty() {
            w
        } else {
            w + PILL_SPACING
        };
        current.push(tag.clone());
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_words_dedupes_and_trims() {
        let mut v = vec!["beach".to_string()];
        add_words(&mut v, "  sunset beach   john ");
        assert_eq!(v, vec!["beach", "sunset", "john"]);
    }

    #[test]
    fn set_words_replaces() {
        let mut v = vec!["old".to_string()];
        set_words(&mut v, "a b a");
        assert_eq!(v, vec!["a", "b"]);
    }

    #[test]
    fn remove_drops_one() {
        let mut v = vec!["a".to_string(), "b".to_string()];
        remove(&mut v, "a");
        assert_eq!(v, vec!["b"]);
    }

    const FONT: f32 = 11.0;

    fn owned(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn wrap_empty_is_empty() {
        assert!(wrap_tag_rows(&[], 200.0, FONT).is_empty());
    }

    #[test]
    fn wrap_all_fits_single_row() {
        let tags = owned(&["aaa", "bbb"]);
        // Each 3-char pill ~= 38.46px; with spacing fits well within 500px.
        assert_eq!(
            wrap_tag_rows(&tags, 500.0, FONT),
            vec![owned(&["aaa", "bbb"])]
        );
    }

    #[test]
    fn wrap_overflow_greedy_multiple_rows() {
        // width 100 -> usable 92. Each 3-char pill ~=38.46; "aaa"+"bbb" = 80.92
        // (<=92) but adding "ccc" (+42.46) exceeds 92, so it starts a new row.
        let tags = owned(&["aaa", "bbb", "ccc"]);
        assert_eq!(
            wrap_tag_rows(&tags, 100.0, FONT),
            vec![owned(&["aaa", "bbb"]), owned(&["ccc"])]
        );
    }

    #[test]
    fn wrap_oversized_tag_gets_own_row_not_dropped() {
        // "x" (~24.82) fits; the long tag alone exceeds usable width but must
        // still appear on its own row; "y" follows on a third row.
        let long = "supercalifragilisticexpialidocious";
        let tags = owned(&["x", long, "y"]);
        let rows = wrap_tag_rows(&tags, 50.0, FONT);
        assert_eq!(rows, vec![owned(&["x"]), owned(&[long]), owned(&["y"])]);
        // No tag is dropped.
        let total: usize = rows.iter().map(Vec::len).sum();
        assert_eq!(total, 3);
    }

    #[test]
    fn wrap_unknown_width_single_row() {
        let tags = owned(&["a", "b", "c"]);
        assert_eq!(
            wrap_tag_rows(&tags, 0.0, FONT),
            vec![owned(&["a", "b", "c"])]
        );
        assert_eq!(
            wrap_tag_rows(&tags, -5.0, FONT),
            vec![owned(&["a", "b", "c"])]
        );
    }

    #[test]
    fn wrap_rows_respect_usable_width_except_lone_oversized() {
        let tags = owned(&["alpha", "beta", "gamma", "delta", "epsilon", "zeta"]);
        let width = 160.0_f32;
        let usable = width - 2.0 * LAYOUT_PADDING;
        for row in wrap_tag_rows(&tags, width, FONT) {
            let est: f32 = row.iter().map(|t| pill_width(t, FONT)).sum::<f32>()
                + PILL_SPACING * (row.len().saturating_sub(1)) as f32;
            if row.len() > 1 {
                assert!(est <= usable, "row {row:?} est {est} > usable {usable}");
            }
        }
    }
}
