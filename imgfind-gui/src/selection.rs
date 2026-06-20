//! Pure selection state + grid-index math for the GUI multi-select modes.
//! Range mode materializes the linear contiguous index run between anchor and
//! cursor (crosses row boundaries — NOT a 2-D rectangle). Free mode toggles
//! individual indices. No I/O, no Slint, no locks.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectionMode {
    #[default]
    Normal,
    Range {
        anchor: usize,
    },
    Free,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    mode: SelectionMode,
    set: BTreeSet<usize>,
}

// wired in Task 3/4
#[allow(dead_code)]
impl Selection {
    pub fn enter_range(&mut self, cursor: usize) {
        self.mode = SelectionMode::Range { anchor: cursor };
        self.set.clear();
        self.set.insert(cursor);
    }

    pub fn enter_free(&mut self) {
        self.mode = SelectionMode::Free;
        self.set.clear();
    }

    pub fn cursor_moved(&mut self, cursor: usize) {
        if let SelectionMode::Range { anchor } = self.mode {
            let (lo, hi) = (anchor.min(cursor), anchor.max(cursor));
            self.set = (lo..=hi).collect();
        }
    }

    pub fn toggle(&mut self, cursor: usize) {
        if self.mode == SelectionMode::Free && !self.set.remove(&cursor) {
            self.set.insert(cursor);
        }
    }

    pub fn clear(&mut self) {
        self.mode = SelectionMode::Normal;
        self.set.clear();
    }

    pub fn is_active(&self) -> bool {
        self.mode != SelectionMode::Normal
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn contains(&self, i: usize) -> bool {
        self.set.contains(&i)
    }

    pub fn set(&self) -> &BTreeSet<usize> {
        &self.set
    }

    pub fn mode(&self) -> SelectionMode {
        self.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(s: &Selection) -> Vec<usize> {
        s.set().iter().copied().collect()
    }

    #[test]
    fn enter_range_seeds_anchor_only() {
        let mut s = Selection::default();
        s.enter_range(5);
        assert!(s.is_active());
        assert_eq!(s.mode(), SelectionMode::Range { anchor: 5 });
        assert_eq!(set_of(&s), vec![5]);
    }

    #[test]
    fn range_forward_is_contiguous_run() {
        let mut s = Selection::default();
        s.enter_range(2);
        s.cursor_moved(8);
        assert_eq!(set_of(&s), vec![2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn range_backward_same_set() {
        let mut s = Selection::default();
        s.enter_range(8);
        s.cursor_moved(2);
        assert_eq!(set_of(&s), vec![2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn range_back_onto_anchor_collapses() {
        let mut s = Selection::default();
        s.enter_range(4);
        s.cursor_moved(7);
        s.cursor_moved(4);
        assert_eq!(set_of(&s), vec![4]);
    }

    #[test]
    fn free_starts_empty_and_toggles() {
        let mut s = Selection::default();
        s.enter_free();
        assert!(s.is_active());
        assert!(s.is_empty());
        s.toggle(3);
        s.toggle(9);
        assert_eq!(set_of(&s), vec![3, 9]);
        s.toggle(3);
        assert_eq!(set_of(&s), vec![9]);
    }

    #[test]
    fn cursor_moved_noop_in_free_and_normal() {
        let mut s = Selection::default();
        s.cursor_moved(5); // Normal
        assert!(s.is_empty());
        s.enter_free();
        s.toggle(2);
        s.cursor_moved(7); // Free: must not change the set
        assert_eq!(set_of(&s), vec![2]);
    }

    #[test]
    fn toggle_noop_in_range_and_normal() {
        let mut s = Selection::default();
        s.toggle(1); // Normal
        assert!(s.is_empty());
        s.enter_range(4);
        s.toggle(9); // Range: must not add
        assert_eq!(set_of(&s), vec![4]);
    }

    #[test]
    fn clear_resets_to_normal_empty() {
        let mut s = Selection::default();
        s.enter_range(4);
        s.cursor_moved(6);
        s.clear();
        assert!(!s.is_active());
        assert_eq!(s.mode(), SelectionMode::Normal);
        assert!(s.is_empty());
    }

    #[test]
    fn re_entering_mode_resets_set() {
        let mut s = Selection::default();
        s.enter_free();
        s.toggle(1);
        s.toggle(2);
        s.enter_range(7); // re-enter: anchor only
        assert_eq!(set_of(&s), vec![7]);
    }

    #[test]
    fn contains_reflects_set() {
        let mut s = Selection::default();
        s.enter_range(2);
        s.cursor_moved(4);
        assert!(s.contains(3));
        assert!(!s.contains(5));
    }
}
