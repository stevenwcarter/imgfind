//! UI-agnostic image filters and their SQL translation. Shared by the
//! non-vector `browse` query and the filtered vector search so both apply
//! identical predicates. Designed to extend: add a field + a clause arm.

use serde::{Deserialize, Serialize};

use crate::units::FileSize;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    /// Inclusive file-size bounds in bytes; `None` = unbounded on that side.
    pub size_min: Option<FileSize>,
    pub size_max: Option<FileSize>,
    /// Lowercased extensions without the dot (e.g. "jpg"); empty = all types.
    pub extensions: Vec<String>,
    pub gps: GpsFilter,
    /// Tag filter: an `Active`/`Inactive` sum type that makes the old
    /// "enabled-but-empty" state unrepresentable. The (possibly disabled) tag
    /// set and match mode are retained either way for fast re-activation.
    pub tag_filter: TagFilter,
    /// When true, exclude images that have a thumbnail-failure marker.
    pub hide_failed: bool,
}

/// Tag-filter state. Replaces the historical `tags`/`tag_match`/`tags_enabled`
/// triple: `Active` is the master-enabled state, `Inactive` is disabled. Both
/// carry the tag set and match mode so toggling never loses the user's tags.
#[derive(Clone, Debug, PartialEq)]
pub enum TagFilter {
    Inactive {
        tags: Vec<String>,
        match_mode: TagMatch,
    },
    Active {
        tags: Vec<String>,
        match_mode: TagMatch,
    },
}

impl Default for TagFilter {
    fn default() -> Self {
        TagFilter::Inactive {
            tags: Vec::new(),
            match_mode: TagMatch::default(),
        }
    }
}

impl TagFilter {
    /// The tags + match mode to apply, or `None` when the filter is inactive or
    /// has no tags (the only two cases that produce no SQL clause).
    pub fn active_tags(&self) -> Option<(&[String], TagMatch)> {
        match self {
            TagFilter::Active { tags, match_mode } if !tags.is_empty() => Some((tags, *match_mode)),
            _ => None,
        }
    }

    /// Whether the master enable is on (`Active`), regardless of tag count.
    pub fn is_enabled(&self) -> bool {
        matches!(self, TagFilter::Active { .. })
    }

    /// The retained tag set, regardless of enabled state.
    pub fn tags(&self) -> &[String] {
        match self {
            TagFilter::Active { tags, .. } | TagFilter::Inactive { tags, .. } => tags,
        }
    }

    /// Mutable access to the retained tag set, regardless of enabled state.
    pub fn tags_mut(&mut self) -> &mut Vec<String> {
        match self {
            TagFilter::Active { tags, .. } | TagFilter::Inactive { tags, .. } => tags,
        }
    }

    /// The retained match mode, regardless of enabled state.
    pub fn match_mode(&self) -> TagMatch {
        match self {
            TagFilter::Active { match_mode, .. } | TagFilter::Inactive { match_mode, .. } => {
                *match_mode
            }
        }
    }

    /// Replace the tag set in place, keeping the enabled state and match mode.
    pub fn set_tags(&mut self, new_tags: Vec<String>) {
        *self.tags_mut() = new_tags;
    }

    /// Toggle AND/OR match mode in place, keeping the enabled state and tags.
    pub fn toggle_match_mode(&mut self) {
        let (TagFilter::Active { match_mode, .. } | TagFilter::Inactive { match_mode, .. }) = self;
        *match_mode = match *match_mode {
            TagMatch::AllOf => TagMatch::AnyOf,
            TagMatch::AnyOf => TagMatch::AllOf,
        };
    }

    /// Flip between `Active` and `Inactive`, preserving the tag set and mode.
    pub fn toggle_enabled(&mut self) {
        *self = match std::mem::take(self) {
            TagFilter::Active { tags, match_mode } => TagFilter::Inactive { tags, match_mode },
            TagFilter::Inactive { tags, match_mode } => TagFilter::Active { tags, match_mode },
        };
    }
}

/// On-disk representation of [`Filters`]: the historical fully-flat key set.
/// Hand-rolling the repr (rather than `#[serde(flatten)]`) keeps `size_min` /
/// `size_max` serializing as bare integers — a flattened map would coerce them
/// to floats — and pins the legacy `ui_state` JSON shape so older saved
/// sessions still load (`tags` / `tag_match` / `tags_enabled` stay top-level).
#[derive(Serialize, Deserialize)]
struct FiltersRepr {
    size_min: Option<FileSize>,
    size_max: Option<FileSize>,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    gps: GpsFilter,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    tag_match: TagMatch,
    #[serde(default)]
    tags_enabled: bool,
    #[serde(default)]
    hide_failed: bool,
}

impl From<&Filters> for FiltersRepr {
    fn from(f: &Filters) -> Self {
        let (tags, tag_match, tags_enabled) = match &f.tag_filter {
            TagFilter::Active { tags, match_mode } => (tags.clone(), *match_mode, true),
            TagFilter::Inactive { tags, match_mode } => (tags.clone(), *match_mode, false),
        };
        FiltersRepr {
            size_min: f.size_min,
            size_max: f.size_max,
            extensions: f.extensions.clone(),
            gps: f.gps,
            tags,
            tag_match,
            tags_enabled,
            hide_failed: f.hide_failed,
        }
    }
}

impl From<FiltersRepr> for Filters {
    fn from(r: FiltersRepr) -> Self {
        let tag_filter = if r.tags_enabled && !r.tags.is_empty() {
            TagFilter::Active {
                tags: r.tags,
                match_mode: r.tag_match,
            }
        } else {
            TagFilter::Inactive {
                tags: r.tags,
                match_mode: r.tag_match,
            }
        };
        Filters {
            size_min: r.size_min,
            size_max: r.size_max,
            extensions: r.extensions,
            gps: r.gps,
            tag_filter,
            hide_failed: r.hide_failed,
        }
    }
}

impl Serialize for Filters {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        FiltersRepr::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Filters {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        FiltersRepr::deserialize(deserializer).map(Filters::from)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpsFilter {
    #[default]
    Any,
    HasGps,
    NoGps,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagMatch {
    /// Image must have every selected tag (AND).
    #[default]
    AllOf,
    /// Image must have at least one selected tag (OR).
    AnyOf,
}

impl Filters {
    /// Copy the whole tag filter (the possibly-disabled tag set + match mode)
    /// from `other` into `self`. Call this after rebuilding size/type/GPS
    /// fields from UI state so an active tag filter is preserved rather than
    /// reset to defaults.
    pub fn carry_tag_filter_from(&mut self, other: &Filters) {
        self.tag_filter = other.tag_filter.clone();
    }
}

/// Build the SQL predicate fragment + ordered [`turso::Value`] bound params for `f`.
///
/// The fragment is either empty or starts with " AND " so it can be appended
/// after an existing `WHERE <something>`. Column aliases assumed: `i` = images,
/// `m` = image_metadata.
pub fn build_filter_clause_turso(f: &Filters) -> (String, Vec<turso::Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<turso::Value> = Vec::new();

    if let Some(min) = f.size_min {
        clauses.push("m.file_size >= ?".into());
        params.push(turso::Value::Integer(min.bytes()));
    }
    if let Some(max) = f.size_max {
        clauses.push("m.file_size <= ?".into());
        params.push(turso::Value::Integer(max.bytes()));
    }
    if !f.extensions.is_empty() {
        let mut ors = Vec::new();
        for ext in &f.extensions {
            ors.push("lower(i.path) LIKE ?".to_string());
            params.push(turso::Value::Text(format!("%.{}", ext.to_lowercase())));
        }
        clauses.push(format!("({})", ors.join(" OR ")));
    }
    match f.gps {
        GpsFilter::Any => {}
        GpsFilter::HasGps => {
            clauses.push("(m.latitude IS NOT NULL AND m.longitude IS NOT NULL)".into());
        }
        GpsFilter::NoGps => {
            clauses.push("(m.latitude IS NULL OR m.longitude IS NULL)".into());
        }
    }

    if f.hide_failed {
        clauses.push(
            "NOT EXISTS (SELECT 1 FROM thumbnail_failures f WHERE f.image_hash = i.hash)".into(),
        );
    }

    if let Some((tags, match_mode)) = f.tag_filter.active_tags() {
        match match_mode {
            TagMatch::AllOf => {
                for tag in tags {
                    clauses.push(
                        "EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                         WHERE it.image_id = i.id AND t.name = ?)"
                            .into(),
                    );
                    params.push(turso::Value::Text(tag.clone()));
                }
            }
            TagMatch::AnyOf => {
                let placeholders = vec!["?"; tags.len()].join(", ");
                clauses.push(format!(
                    "EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                     WHERE it.image_id = i.id AND t.name IN ({placeholders}))"
                ));
                for tag in tags {
                    params.push(turso::Value::Text(tag.clone()));
                }
            }
        }
    }

    if clauses.is_empty() {
        (String::new(), params)
    } else {
        (format!(" AND {}", clauses.join(" AND ")), params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_flat_filters_json_deserializes_into_tag_filter() {
        // Pre-migration on-disk shape: flat tags/tag_match/tags_enabled.
        let json = r#"{"size_min":null,"size_max":null,"extensions":[],"gps":"any","tags":["a"],"tag_match":"anyof","tags_enabled":true}"#;
        let f: Filters = serde_json::from_str(json).unwrap();
        match &f.tag_filter {
            TagFilter::Active { tags, match_mode } => {
                assert_eq!(tags, &vec!["a".to_string()]);
                assert_eq!(*match_mode, TagMatch::AnyOf);
            }
            _ => panic!("expected Active"),
        }
    }

    #[test]
    fn disabled_with_tags_is_inactive_retaining_them() {
        let json = r#"{"size_min":null,"size_max":null,"extensions":[],"gps":"any","tags":["a","b"],"tag_match":"allof","tags_enabled":false}"#;
        let f: Filters = serde_json::from_str(json).unwrap();
        match &f.tag_filter {
            TagFilter::Inactive { tags, .. } => assert_eq!(tags.len(), 2),
            _ => panic!("expected Inactive retaining tags"),
        }
    }

    #[test]
    fn filters_serialize_to_flat_on_disk_shape() {
        // The in-memory sum type must persist as the historical flat triple so
        // a newer build's saved session is still readable by the same keys.
        let f = Filters {
            tag_filter: TagFilter::Active {
                tags: vec!["a".into()],
                match_mode: TagMatch::AnyOf,
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"tags\":[\"a\"]"), "{json}");
        assert!(json.contains("\"tag_match\":\"anyof\""), "{json}");
        assert!(json.contains("\"tags_enabled\":true"), "{json}");
        // Round-trip back to the same value.
        let back: Filters = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn filters_serialize_size_bounds_as_bare_integers() {
        // Pins the load-bearing invariant that the Filters serde seam (which crosses
        // to persisted ui_state JSON) emits size bounds as BARE INTEGERS, not floats.
        // A future #[serde(flatten)] refactor would coerce 1024 -> 1024.0 and silently
        // corrupt saved sessions; this test would catch it. (FileSize is #[serde(transparent)].)
        let f = Filters {
            size_min: Some(FileSize(1024)),
            size_max: Some(FileSize(5_000_000)),
            ..Default::default()
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(
            json.contains("\"size_min\":1024"),
            "size_min must be a bare int, got: {json}"
        );
        assert!(
            json.contains("\"size_max\":5000000"),
            "size_max must be a bare int, got: {json}"
        );
        // And it round-trips back to the same value (no precision/type drift).
        let back: Filters = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn carry_tag_filter_preserves_tags_and_size() {
        let mut rebuilt = Filters {
            size_min: Some(FileSize(1024)),
            size_max: Some(FileSize(10 * 1024 * 1024)),
            ..Default::default()
        };
        let existing = Filters {
            tag_filter: TagFilter::Active {
                tags: vec!["a".into(), "b".into()],
                match_mode: TagMatch::AnyOf,
            },
            ..Default::default()
        };
        rebuilt.carry_tag_filter_from(&existing);
        assert_eq!(
            rebuilt.tag_filter,
            TagFilter::Active {
                tags: vec!["a".to_string(), "b".to_string()],
                match_mode: TagMatch::AnyOf,
            }
        );
        // size fields must be untouched
        assert_eq!(rebuilt.size_min, Some(FileSize(1024)));
        assert_eq!(rebuilt.size_max, Some(FileSize(10 * 1024 * 1024)));
    }

    #[test]
    fn empty_filters_yield_no_clause() {
        let (sql, params) = build_filter_clause_turso(&Filters::default());
        assert_eq!(sql, "");
        assert!(params.is_empty());
    }

    #[test]
    fn size_both_bounds() {
        let f = Filters {
            size_min: Some(FileSize(100)),
            size_max: Some(FileSize(5000)),
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(sql, " AND m.file_size >= ? AND m.file_size <= ?");
        assert_eq!(
            params,
            vec![turso::Value::Integer(100), turso::Value::Integer(5000)]
        );
    }

    #[test]
    fn size_one_sided() {
        let f = Filters {
            size_min: Some(FileSize(100)),
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(sql, " AND m.file_size >= ?");
        assert_eq!(params, vec![turso::Value::Integer(100)]);
    }

    #[test]
    fn extensions_become_lowercased_like_params() {
        let f = Filters {
            extensions: vec!["JPG".into(), "png".into()],
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(sql, " AND (lower(i.path) LIKE ? OR lower(i.path) LIKE ?)");
        assert_eq!(
            params,
            vec![
                turso::Value::Text("%.jpg".into()),
                turso::Value::Text("%.png".into())
            ]
        );
    }

    #[test]
    fn gps_has_and_no() {
        let has = build_filter_clause_turso(&Filters {
            gps: GpsFilter::HasGps,
            ..Default::default()
        })
        .0;
        assert_eq!(
            has,
            " AND (m.latitude IS NOT NULL AND m.longitude IS NOT NULL)"
        );
        let no = build_filter_clause_turso(&Filters {
            gps: GpsFilter::NoGps,
            ..Default::default()
        })
        .0;
        assert_eq!(no, " AND (m.latitude IS NULL OR m.longitude IS NULL)");
    }

    #[test]
    fn combined_filters_join_with_and() {
        let f = Filters {
            size_min: Some(FileSize(10)),
            size_max: None,
            extensions: vec!["nef".into()],
            gps: GpsFilter::HasGps,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(
            sql,
            " AND m.file_size >= ? AND (lower(i.path) LIKE ?) AND (m.latitude IS NOT NULL AND m.longitude IS NOT NULL)"
        );
        assert_eq!(
            params,
            vec![
                turso::Value::Integer(10),
                turso::Value::Text("%.nef".into())
            ]
        );
    }

    #[test]
    fn tags_disabled_yields_no_clause() {
        let f = Filters {
            tag_filter: TagFilter::Inactive {
                tags: vec!["a".into(), "b".into()],
                match_mode: TagMatch::default(),
            },
            ..Default::default()
        };
        assert_eq!(build_filter_clause_turso(&f).0, "");
    }

    #[test]
    fn tags_all_of_emits_exists_per_tag() {
        let f = Filters {
            tag_filter: TagFilter::Active {
                tags: vec!["a".into(), "b".into()],
                match_mode: TagMatch::AllOf,
            },
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(
            sql,
            " AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?) AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?)"
        );
        assert_eq!(
            params,
            vec![
                turso::Value::Text("a".into()),
                turso::Value::Text("b".into())
            ]
        );
    }

    #[test]
    fn tags_any_of_emits_single_in_clause() {
        let f = Filters {
            tag_filter: TagFilter::Active {
                tags: vec!["a".into(), "b".into()],
                match_mode: TagMatch::AnyOf,
            },
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(
            sql,
            " AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name IN (?, ?))"
        );
        assert_eq!(
            params,
            vec![
                turso::Value::Text("a".into()),
                turso::Value::Text("b".into())
            ]
        );
    }

    #[test]
    fn tags_combine_after_size() {
        let f = Filters {
            size_min: Some(FileSize(5)),
            tag_filter: TagFilter::Active {
                tags: vec!["x".into()],
                match_mode: TagMatch::default(),
            },
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(
            sql,
            " AND m.file_size >= ? AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?)"
        );
        assert_eq!(
            params,
            vec![turso::Value::Integer(5), turso::Value::Text("x".into())]
        );
    }

    #[test]
    fn hide_failed_emits_not_exists_clause() {
        let f = Filters {
            hide_failed: true,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(
            sql,
            " AND NOT EXISTS (SELECT 1 FROM thumbnail_failures f WHERE f.image_hash = i.hash)"
        );
        assert!(params.is_empty());
    }

    #[test]
    fn hide_failed_default_false_emits_nothing() {
        let (sql, _) = build_filter_clause_turso(&Filters::default());
        assert!(!sql.contains("thumbnail_failures"));
    }

    #[test]
    fn hide_failed_round_trips_and_defaults_when_absent() {
        // New field round-trips.
        let f = Filters { hide_failed: true, ..Default::default() };
        let json = serde_json::to_string(&f).unwrap();
        let back: Filters = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
        // Old JSON lacking the field deserializes to false.
        let old = r#"{"size_min":null,"size_max":null,"extensions":[],"gps":"any","tags":[],"tag_match":"allof","tags_enabled":false}"#;
        let loaded: Filters = serde_json::from_str(old).unwrap();
        assert!(!loaded.hide_failed);
    }
}
