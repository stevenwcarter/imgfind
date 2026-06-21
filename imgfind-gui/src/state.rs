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

#[derive(Debug, Default)]
pub struct SearchState {
    pub committed_query: String,
    pub results: Vec<RowMeta>,
    pub loading: bool,
    pub error: Option<String>,
    /// Active sort order applied to `results`. Defaults to `Sort::default()`.
    pub sort: Sort,
    /// Snapshot of `results` in relevance (backend) order, captured when a
    /// search/similar result arrives. Used to restore the original ranking
    /// when the user selects "Relevance" in the sort selector.
    pub relevance_order: Vec<RowMeta>,
    has_searched: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a fresh search for `query`.
    pub fn start_search(&mut self, query: String) {
        self.committed_query = query;
        self.loading = true;
        self.error = None;
        self.has_searched = true;
    }

    /// Replace the full result set with `rows` and clear loading/error.
    ///
    /// `rows` is the complete ordered list for the query/browse (relevance order
    /// for searches, sort order for browse); the view becomes `Results` when it
    /// is non-empty and `Empty` otherwise (after a search has been started).
    pub fn apply_results(&mut self, rows: Vec<RowMeta>) {
        self.results = rows;
        self.loading = false;
        self.error = None;
    }

    /// Record a failure, clearing the (now stale) results.
    pub fn apply_error(&mut self, message: String) {
        self.error = Some(message);
        self.loading = false;
        self.results.clear();
    }

    /// Re-sort the current results in memory and remember the new sort order.
    pub fn resort(&mut self, sort: &Sort) {
        sort_rows(&mut self.results, sort);
        self.sort = *sort;
    }

    /// Restore `results` to the original relevance (backend) order captured
    /// when the search/similar result last arrived.
    pub fn resort_to_relevance(&mut self) {
        self.results = self.relevance_order.clone();
    }

    pub fn view_state(&self) -> ViewState {
        if self.loading {
            ViewState::Loading
        } else if self.error.is_some() {
            ViewState::Error
        } else if !self.has_searched {
            ViewState::Idle
        } else if self.results.is_empty() {
            ViewState::Empty
        } else {
            ViewState::Results
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
        assert_eq!(s.error.as_deref(), Some("boom"));
    }

    #[test]
    fn fresh_search_replaces_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_results(vec![rm(1, "a.jpg", Some(1))]);
        s.start_search("dog".into());
        s.apply_results(vec![rm(2, "b.jpg", Some(2))]);
        assert_eq!(s.results.len(), 1);
        assert_eq!(s.results[0].path, "b.jpg");
    }

    #[test]
    fn error_clears_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_results(vec![rm(1, "a.jpg", Some(1))]);
        s.start_search("dog".into());
        s.apply_error("net".into());
        assert!(s.results.is_empty());
    }

    #[test]
    fn apply_results_replaces_and_sets_state() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        let rows = vec![rm(1, "b.jpg", Some(2)), rm(2, "a.jpg", Some(1))];
        s.apply_results(rows.clone());
        assert_eq!(s.results, rows);
        assert!(!s.loading);
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
            s.results.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ImageId(2), ImageId(1)]
        );
    }

    #[test]
    fn resort_to_relevance_restores_original_order() {
        let mut s = SearchState::new();
        let rows = vec![rm(1, "z.jpg", Some(2)), rm(2, "a.jpg", Some(1))];
        s.relevance_order = rows.clone();
        s.results = rows.clone();
        s.resort(&Sort {
            key: SortKey::Name,
            dir: SortDir::Asc,
        });
        // After sort by name ascending: a.jpg (id=2) before z.jpg (id=1)
        assert_eq!(
            s.results.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ImageId(2), ImageId(1)]
        );
        // Restore to relevance order (original: z.jpg=1, a.jpg=2)
        s.resort_to_relevance();
        assert_eq!(
            s.results.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ImageId(1), ImageId(2)]
        );
    }
}
