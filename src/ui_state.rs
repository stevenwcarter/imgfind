//! Persisted GUI session state (single row in the `ui_state` table).

use serde::{Deserialize, Serialize};

use crate::filters::Filters;
use crate::sort::Sort;

/// Which search mode the session was in when last saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum PersistedMode {
    #[default]
    Browse,
    Text(String),
    /// Seed image id for a similarity search.
    Similar(i64),
}

/// Full GUI session state that can be serialised to JSON and stored in SQLite.
///
/// Every field carries `#[serde(default)]` so that forward- and
/// backward-compatible schema evolution is safe: unknown fields are ignored on
/// read, and fields absent from an older JSON blob fall back to their
/// `Default` values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UiState {
    #[serde(default)]
    pub search_text: String,
    #[serde(default)]
    pub mode: PersistedMode,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default)]
    pub filters: Filters,
    #[serde(default)]
    pub result_ids: Vec<i64>,
    #[serde(default)]
    pub selected_index: Option<usize>,
    #[serde(default)]
    pub detail_open: bool,
    #[serde(default)]
    pub scroll_y: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::{Sort, SortDir, SortKey};

    #[test]
    fn round_trips_through_json() {
        let st = UiState {
            search_text: "cat".into(),
            mode: PersistedMode::Text("cat".into()),
            sort: Sort {
                key: SortKey::Size,
                dir: SortDir::Desc,
            },
            filters: crate::filters::Filters::default(),
            result_ids: vec![3, 1, 2],
            selected_index: Some(1),
            detail_open: true,
            scroll_y: 128.5,
        };
        let json = serde_json::to_string(&st).unwrap();
        let back: UiState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, st);
    }

    #[test]
    fn default_is_browse_empty() {
        let st = UiState::default();
        assert_eq!(st.mode, PersistedMode::Browse);
        assert!(st.result_ids.is_empty());
        assert_eq!(st.sort, Sort::default());
    }
}
