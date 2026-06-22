//! Persisted GUI session state (single row in the `ui_state` table).

use serde::{Deserialize, Serialize};

use crate::filters::Filters;
use crate::ids::ImageId;
use crate::sort::Sort;

/// Which search mode the session was in when last saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum PersistedMode {
    #[default]
    Browse,
    Text(String),
    /// Seed image id for a similarity search.
    Similar(ImageId),
}

/// One color brush: a curated set of tag names quick-applied as a unit. The
/// color is input-only; these tags are assigned to images as ordinary tags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TagBrush {
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Default `rail_visible` is `true` (rail shown on first launch).
fn default_true() -> bool {
    true
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
    pub result_ids: Vec<ImageId>,
    #[serde(default)]
    pub selected_index: Option<usize>,
    #[serde(default)]
    pub detail_open: bool,
    #[serde(default)]
    pub scroll_y: f32,
    /// Five color brushes, indexed by `colors::BrushColor::index`.
    #[serde(default)]
    pub brushes: [TagBrush; 5],
    /// The live, editable "Most Recent" (`mm`) staging set.
    #[serde(default)]
    pub recent_tags: Vec<String>,
    /// Left-rail visibility (defaults to shown).
    #[serde(default = "default_true")]
    pub rail_visible: bool,
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
            result_ids: vec![ImageId(3), ImageId(1), ImageId(2)],
            selected_index: Some(1),
            detail_open: true,
            scroll_y: 128.5,
            brushes: [
                TagBrush {
                    tags: vec!["beach".into(), "sunset".into()],
                },
                TagBrush::default(),
                TagBrush::default(),
                TagBrush::default(),
                TagBrush::default(),
            ],
            recent_tags: vec!["beach".into(), "sunset".into()],
            rail_visible: false,
        };
        let json = serde_json::to_string(&st).unwrap();
        // `#[serde(transparent)]` keeps the persisted id arrays bare integers.
        assert!(
            json.contains("\"result_ids\":[3,1,2]"),
            "result_ids must serialize as a bare-int array: {json}"
        );
        let back: UiState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, st);
    }

    #[test]
    fn persisted_mode_similar_serializes_with_bare_int_value() {
        let json = serde_json::to_string(&PersistedMode::Similar(ImageId(3))).unwrap();
        assert_eq!(json, r#"{"kind":"similar","value":3}"#);
    }

    #[test]
    fn old_blob_without_tag_fields_deserializes() {
        // A pre-tags JSON blob omits brushes/recent_tags/rail_visible.
        let json = r#"{"search_text":"x","result_ids":[1]}"#;
        let back: UiState = serde_json::from_str(json).unwrap();
        assert_eq!(back.brushes, <[TagBrush; 5]>::default());
        assert!(back.recent_tags.is_empty());
        // Absent rail_visible falls back to the serde default (true).
        assert!(back.rail_visible);
    }

    #[test]
    fn default_is_browse_empty() {
        let st = UiState::default();
        assert_eq!(st.mode, PersistedMode::Browse);
        assert!(st.result_ids.is_empty());
        assert_eq!(st.sort, Sort::default());
    }
}
