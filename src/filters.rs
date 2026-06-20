//! UI-agnostic image filters and their SQL translation. Shared by the
//! non-vector `browse` query and the filtered vector search so both apply
//! identical predicates. Designed to extend: add a field + a clause arm.

use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Filters {
    /// Inclusive file-size bounds in bytes; `None` = unbounded on that side.
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    /// Lowercased extensions without the dot (e.g. "jpg"); empty = all types.
    pub extensions: Vec<String>,
    pub gps: GpsFilter,
    /// Tag names to filter by; empty = no tag filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Whether all tags must match (AND) or any (OR).
    #[serde(default)]
    pub tag_match: TagMatch,
    /// Master enable for the tag filter (`ft`); when false, tags are ignored
    /// but retained.
    #[serde(default)]
    pub tags_enabled: bool,
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

/// Build the SQL predicate fragment + ordered bound params for `f`.
/// The fragment is either empty or starts with " AND " so it can be appended
/// after an existing `WHERE <something>`. Column aliases assumed: `i` = images,
/// `m` = image_metadata.
pub fn build_filter_clause(f: &Filters) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(min) = f.size_min {
        clauses.push("m.file_size >= ?".into());
        params.push(Value::Integer(min));
    }
    if let Some(max) = f.size_max {
        clauses.push("m.file_size <= ?".into());
        params.push(Value::Integer(max));
    }
    if !f.extensions.is_empty() {
        let mut ors = Vec::new();
        for ext in &f.extensions {
            ors.push("lower(i.path) LIKE ?".to_string());
            params.push(Value::Text(format!("%.{}", ext.to_lowercase())));
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

    if f.tags_enabled && !f.tags.is_empty() {
        match f.tag_match {
            TagMatch::AllOf => {
                for tag in &f.tags {
                    clauses.push(
                        "EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                         WHERE it.image_id = i.id AND t.name = ?)"
                            .into(),
                    );
                    params.push(Value::Text(tag.clone()));
                }
            }
            TagMatch::AnyOf => {
                let placeholders = vec!["?"; f.tags.len()].join(", ");
                clauses.push(format!(
                    "EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                     WHERE it.image_id = i.id AND t.name IN ({placeholders}))"
                ));
                for tag in &f.tags {
                    params.push(Value::Text(tag.clone()));
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
    fn empty_filters_yield_no_clause() {
        let (sql, params) = build_filter_clause(&Filters::default());
        assert_eq!(sql, "");
        assert!(params.is_empty());
    }

    #[test]
    fn size_both_bounds() {
        let f = Filters {
            size_min: Some(100),
            size_max: Some(5000),
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(sql, " AND m.file_size >= ? AND m.file_size <= ?");
        assert_eq!(params, vec![Value::Integer(100), Value::Integer(5000)]);
    }

    #[test]
    fn size_one_sided() {
        let f = Filters {
            size_min: Some(100),
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(sql, " AND m.file_size >= ?");
        assert_eq!(params, vec![Value::Integer(100)]);
    }

    #[test]
    fn extensions_become_lowercased_like_params() {
        let f = Filters {
            extensions: vec!["JPG".into(), "png".into()],
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(sql, " AND (lower(i.path) LIKE ? OR lower(i.path) LIKE ?)");
        assert_eq!(
            params,
            vec![Value::Text("%.jpg".into()), Value::Text("%.png".into())]
        );
    }

    #[test]
    fn gps_has_and_no() {
        let has = build_filter_clause(&Filters {
            gps: GpsFilter::HasGps,
            ..Default::default()
        })
        .0;
        assert_eq!(
            has,
            " AND (m.latitude IS NOT NULL AND m.longitude IS NOT NULL)"
        );
        let no = build_filter_clause(&Filters {
            gps: GpsFilter::NoGps,
            ..Default::default()
        })
        .0;
        assert_eq!(no, " AND (m.latitude IS NULL OR m.longitude IS NULL)");
    }

    #[test]
    fn combined_filters_join_with_and() {
        let f = Filters {
            size_min: Some(10),
            size_max: None,
            extensions: vec!["nef".into()],
            gps: GpsFilter::HasGps,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND m.file_size >= ? AND (lower(i.path) LIKE ?) AND (m.latitude IS NOT NULL AND m.longitude IS NOT NULL)"
        );
        assert_eq!(
            params,
            vec![Value::Integer(10), Value::Text("%.nef".into())]
        );
    }

    #[test]
    fn tags_disabled_yields_no_clause() {
        let f = Filters {
            tags: vec!["a".into(), "b".into()],
            tags_enabled: false,
            ..Default::default()
        };
        assert_eq!(build_filter_clause(&f).0, "");
    }

    #[test]
    fn tags_all_of_emits_exists_per_tag() {
        let f = Filters {
            tags: vec!["a".into(), "b".into()],
            tag_match: TagMatch::AllOf,
            tags_enabled: true,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?) AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?)"
        );
        assert_eq!(
            params,
            vec![Value::Text("a".into()), Value::Text("b".into())]
        );
    }

    #[test]
    fn tags_any_of_emits_single_in_clause() {
        let f = Filters {
            tags: vec!["a".into(), "b".into()],
            tag_match: TagMatch::AnyOf,
            tags_enabled: true,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name IN (?, ?))"
        );
        assert_eq!(
            params,
            vec![Value::Text("a".into()), Value::Text("b".into())]
        );
    }

    #[test]
    fn tags_combine_after_size() {
        let f = Filters {
            size_min: Some(5),
            tags: vec!["x".into()],
            tags_enabled: true,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND m.file_size >= ? AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?)"
        );
        assert_eq!(params, vec![Value::Integer(5), Value::Text("x".into())]);
    }
}
