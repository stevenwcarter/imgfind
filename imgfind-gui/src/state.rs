//! Pure, UI-agnostic search state machine. Holds the full ordered result set
//! for the current query/browse (set once per query — no paging) so it can be
//! unit-tested without the Slint runtime.

use imgfind::sort::{RowMeta, Sort, sort_rows};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewState {
    Idle,
    Loading,
    Error,
    Empty,
    Results,
}

/// Lifecycle of the current query/browse. Modeling it as an enum makes the old
/// four-field truth table's illegal combinations unrepresentable: there is no
/// way to be `loading && error` or `loading && results` because `Loading`
/// carries neither, and a finished query is exactly one `Complete` carrying its
/// results *and* an optional error (never a `Loading` overlap). See
/// `complete_cannot_be_loading` in the tests.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Phase {
    /// No search has been started yet (fresh state).
    #[default]
    Idle,
    /// A search is in flight; no results or error are observable yet.
    Loading,
    /// A search finished: `results` is the full ordered set (empty on a
    /// no-match), and `error` is `Some` only on failure.
    Complete {
        results: Vec<RowMeta>,
        error: Option<String>,
    },
}

#[derive(Debug, Default)]
pub struct SearchState {
    pub committed_query: String,
    /// Lifecycle of the current query, holding its results and any error.
    pub phase: Phase,
    /// Active sort order applied to the results. Defaults to `Sort::default()`.
    pub sort: Sort,
    /// Snapshot of the results in relevance (backend) order, captured when a
    /// search/similar result arrives. Used to restore the original ranking
    /// when the user selects "Relevance" in the sort selector.
    pub relevance_order: Vec<RowMeta>,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current result set, or an empty slice when idle/loading.
    pub fn results(&self) -> &[RowMeta] {
        match &self.phase {
            Phase::Complete { results, .. } => results,
            Phase::Idle | Phase::Loading => &[],
        }
    }

    /// The current error message, if the last completed search failed.
    pub fn error(&self) -> Option<&str> {
        match &self.phase {
            Phase::Complete { error, .. } => error.as_deref(),
            Phase::Idle | Phase::Loading => None,
        }
    }

    /// Begin a fresh search for `query`.
    pub fn start_search(&mut self, query: String) {
        self.committed_query = query;
        self.phase = Phase::Loading;
    }

    /// Replace the full result set with `rows` and clear loading/error.
    ///
    /// `rows` is the complete ordered list for the query/browse (relevance order
    /// for searches, sort order for browse); the view becomes `Results` when it
    /// is non-empty and `Empty` otherwise (after a search has been started).
    pub fn apply_results(&mut self, rows: Vec<RowMeta>) {
        self.phase = Phase::Complete {
            results: rows,
            error: None,
        };
    }

    /// Record a failure, clearing the (now stale) results.
    pub fn apply_error(&mut self, message: String) {
        self.phase = Phase::Complete {
            results: Vec::new(),
            error: Some(message),
        };
    }

    /// Re-sort the current results in memory and remember the new sort order.
    /// A no-op unless a search has completed.
    pub fn resort(&mut self, sort: &Sort) {
        if let Phase::Complete { results, .. } = &mut self.phase {
            sort_rows(results, sort);
        }
        self.sort = *sort;
    }

    /// Restore the results to the original relevance (backend) order captured
    /// when the search/similar result last arrived. A no-op unless a search has
    /// completed.
    pub fn resort_to_relevance(&mut self) {
        if let Phase::Complete { results, .. } = &mut self.phase {
            *results = self.relevance_order.clone();
        }
    }

    pub fn view_state(&self) -> ViewState {
        match &self.phase {
            Phase::Idle => ViewState::Idle,
            Phase::Loading => ViewState::Loading,
            Phase::Complete { error: Some(_), .. } => ViewState::Error,
            Phase::Complete {
                results,
                error: None,
            } if results.is_empty() => ViewState::Empty,
            Phase::Complete { .. } => ViewState::Results,
        }
    }
}

#[cfg(test)]
mod tests {
    use imgfind::ids::ImageId;
    use imgfind::sort::{SortDir, SortKey};
    use imgfind::units::FileSize;

    use super::*;

    fn rm(id: i64, path: &str, size: Option<i64>) -> RowMeta {
        let ext = path
            .rsplit_once('.')
            .map(|(_, e)| e.to_lowercase())
            .unwrap_or_default();
        RowMeta {
            id: ImageId(id),
            path: path.into(),
            size: size.map(FileSize),
            ext,
        }
    }

    #[test]
    fn fresh_state_is_idle() {
        let s = SearchState::new();
        assert_eq!(s.view_state(), ViewState::Idle);
    }

    #[test]
    fn during_search_is_loading() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        assert_eq!(s.view_state(), ViewState::Loading);
        assert_eq!(s.committed_query, "cat");
    }

    #[test]
    fn empty_results_after_search_is_empty() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_results(vec![]);
        assert_eq!(s.view_state(), ViewState::Empty);
    }

    #[test]
    fn nonempty_results_is_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_results(vec![rm(1, "a.jpg", Some(1024))]);
        assert_eq!(s.view_state(), ViewState::Results);
    }

    #[test]
    fn error_state_takes_precedence() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_error("boom".into());
        assert_eq!(s.view_state(), ViewState::Error);
        assert_eq!(s.error(), Some("boom"));
    }

    #[test]
    fn complete_cannot_be_loading() {
        // The Phase enum makes `loading && error` and `loading && results`
        // unrepresentable: a search is either `Loading` (no results, no error)
        // or `Complete` (results plus an optional error) — never both. This
        // pins that invariant against the old four-field truth table.
        let mut s = SearchState::new();
        s.start_search("cat".into());
        assert!(matches!(s.phase, Phase::Loading));
        s.apply_error("boom".into());
        assert!(matches!(s.phase, Phase::Complete { .. }));
        assert_eq!(s.view_state(), ViewState::Error);
    }

    #[test]
    fn fresh_search_replaces_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_results(vec![rm(1, "a.jpg", Some(1))]);
        s.start_search("dog".into());
        s.apply_results(vec![rm(2, "b.jpg", Some(2))]);
        assert_eq!(s.results().len(), 1);
        assert_eq!(s.results()[0].path, "b.jpg");
    }

    #[test]
    fn error_clears_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_results(vec![rm(1, "a.jpg", Some(1))]);
        s.start_search("dog".into());
        s.apply_error("net".into());
        assert!(s.results().is_empty());
    }

    #[test]
    fn apply_results_replaces_and_sets_state() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        let rows = vec![rm(1, "b.jpg", Some(2)), rm(2, "a.jpg", Some(1))];
        s.apply_results(rows.clone());
        assert_eq!(s.results(), rows.as_slice());
        assert_ne!(s.view_state(), ViewState::Loading);
    }

    #[test]
    fn resort_search_results_in_memory() {
        let mut s = SearchState::new();
        s.apply_results(vec![rm(1, "b.jpg", Some(2)), rm(2, "a.jpg", Some(1))]);
        s.resort(&Sort {
            key: SortKey::Name,
            dir: SortDir::Asc,
        });
        assert_eq!(
            s.results().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ImageId(2), ImageId(1)]
        );
    }

    #[test]
    fn resort_to_relevance_restores_original_order() {
        let mut s = SearchState::new();
        let rows = vec![rm(1, "z.jpg", Some(2)), rm(2, "a.jpg", Some(1))];
        s.relevance_order = rows.clone();
        s.apply_results(rows);
        s.resort(&Sort {
            key: SortKey::Name,
            dir: SortDir::Asc,
        });
        // After sort by name ascending: a.jpg (id=2) before z.jpg (id=1)
        assert_eq!(
            s.results().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ImageId(2), ImageId(1)]
        );
        // Restore to relevance order (original: z.jpg=1, a.jpg=2)
        s.resort_to_relevance();
        assert_eq!(
            s.results().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ImageId(1), ImageId(2)]
        );
    }
}
