//! Pure, UI-agnostic search state machine. Mirrors the React app's
//! `searchViewState.ts` + pagination behavior so it can be unit-tested
//! without the Slint runtime.

pub const PAGE_SIZE: usize = 80;

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub path: String,
    pub distance: f32,
    pub file_size: Option<i64>,
}

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
    pub results: Vec<SearchResult>,
    pub loading: bool,
    pub error: Option<String>,
    pub has_more: bool,
    has_searched: bool,
}

impl SearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a fresh (offset 0) search for `query`.
    pub fn start_search(&mut self, query: String) {
        self.committed_query = query;
        self.loading = true;
        self.error = None;
        self.has_searched = true;
    }

    /// Apply a returned page. `offset == 0` replaces; `offset > 0` appends.
    pub fn apply_page(&mut self, mut results: Vec<SearchResult>, offset: usize) {
        self.has_more = results.len() == PAGE_SIZE;
        if offset == 0 {
            self.results = results;
        } else {
            self.results.append(&mut results);
        }
        self.loading = false;
        self.error = None;
    }

    /// Record a failure. A first-page (offset 0) failure clears results.
    pub fn apply_error(&mut self, message: String, offset: usize) {
        self.error = Some(message);
        self.loading = false;
        if offset == 0 {
            self.results.clear();
        }
    }

    pub fn next_offset(&self) -> usize {
        self.results.len()
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
    use super::*;

    fn r(path: &str) -> SearchResult {
        SearchResult { path: path.into(), distance: 0.1, file_size: Some(1024) }
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
        s.apply_page(vec![], 0);
        assert_eq!(s.view_state(), ViewState::Empty);
    }

    #[test]
    fn nonempty_results_is_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        assert_eq!(s.view_state(), ViewState::Results);
    }

    #[test]
    fn error_state_takes_precedence() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_error("boom".into(), 0);
        assert_eq!(s.view_state(), ViewState::Error);
        assert_eq!(s.error.as_deref(), Some("boom"));
    }

    #[test]
    fn full_page_sets_has_more() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        let page: Vec<SearchResult> = (0..PAGE_SIZE).map(|i| r(&format!("{i}.jpg"))).collect();
        s.apply_page(page, 0);
        assert!(s.has_more);
        assert_eq!(s.next_offset(), PAGE_SIZE);
    }

    #[test]
    fn short_page_clears_has_more() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        assert!(!s.has_more);
    }

    #[test]
    fn load_more_appends_not_replaces() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        s.apply_page(vec![r("b.jpg")], 1);
        assert_eq!(s.results.len(), 2);
        assert_eq!(s.results[1].path, "b.jpg");
    }

    #[test]
    fn fresh_search_replaces_results() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        s.start_search("dog".into());
        s.apply_page(vec![r("b.jpg")], 0);
        assert_eq!(s.results.len(), 1);
        assert_eq!(s.results[0].path, "b.jpg");
    }

    #[test]
    fn error_on_first_page_clears_results_but_keeps_old_on_load_more() {
        let mut s = SearchState::new();
        s.start_search("cat".into());
        s.apply_page(vec![r("a.jpg")], 0);
        // error while loading more (offset > 0) keeps existing results
        s.apply_error("net".into(), 1);
        assert_eq!(s.results.len(), 1);
        // error on a fresh search (offset 0) clears
        s.start_search("dog".into());
        s.apply_error("net".into(), 0);
        assert!(s.results.is_empty());
    }
}
