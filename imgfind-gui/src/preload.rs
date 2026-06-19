//! Neighbor preload ordering: focus first, then outward in an increasing arc.

// Not yet wired into the preloading call site; suppress until Task 11 connects it.
#[allow(dead_code)]
/// Returns indices to preload starting at `i`, expanding outward by one step
/// in each direction up to distance `n`, clamped to `[0, len)` and
/// de-duplicated.
///
/// Example: `preload_arc(5, 2, 100)` → `[5, 6, 4, 7, 3]`.
pub fn preload_arc(i: usize, n: usize, len: usize) -> Vec<usize> {
    if len == 0 || i >= len {
        return Vec::new();
    }
    let mut out = vec![i];
    let mut seen = std::collections::HashSet::from([i]);
    for d in 1..=n {
        for cand in [i.checked_add(d), i.checked_sub(d)].into_iter().flatten() {
            if cand < len && seen.insert(cand) {
                out.push(cand);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_middle() {
        assert_eq!(preload_arc(5, 2, 100), vec![5, 6, 4, 7, 3]);
    }

    #[test]
    fn arc_near_start_clamps_and_dedups() {
        assert_eq!(preload_arc(0, 2, 100), vec![0, 1, 2]);
    }

    #[test]
    fn arc_near_end_clamps() {
        assert_eq!(preload_arc(99, 2, 100), vec![99, 98, 97]);
    }

    #[test]
    fn arc_zero_neighbors() {
        assert_eq!(preload_arc(5, 0, 100), vec![5]);
    }

    #[test]
    fn arc_empty_or_single() {
        assert_eq!(preload_arc(0, 2, 0), Vec::<usize>::new());
        assert_eq!(preload_arc(0, 2, 1), vec![0]);
    }
}
