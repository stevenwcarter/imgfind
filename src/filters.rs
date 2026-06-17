//! UI-agnostic image filters and their SQL translation. Shared by the
//! non-vector `browse` query and the filtered vector search so both apply
//! identical predicates. Designed to extend: add a field + a clause arm.

use rusqlite::types::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    /// Inclusive file-size bounds in bytes; `None` = unbounded on that side.
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    /// Lowercased extensions without the dot (e.g. "jpg"); empty = all types.
    pub extensions: Vec<String>,
    pub gps: GpsFilter,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpsFilter {
    #[default]
    Any,
    HasGps,
    NoGps,
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
        let f = Filters { size_min: Some(100), size_max: Some(5000), ..Default::default() };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(sql, " AND m.file_size >= ? AND m.file_size <= ?");
        assert_eq!(params, vec![Value::Integer(100), Value::Integer(5000)]);
    }

    #[test]
    fn size_one_sided() {
        let f = Filters { size_min: Some(100), ..Default::default() };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(sql, " AND m.file_size >= ?");
        assert_eq!(params, vec![Value::Integer(100)]);
    }

    #[test]
    fn extensions_become_lowercased_like_params() {
        let f = Filters { extensions: vec!["JPG".into(), "png".into()], ..Default::default() };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(sql, " AND (lower(i.path) LIKE ? OR lower(i.path) LIKE ?)");
        assert_eq!(params, vec![Value::Text("%.jpg".into()), Value::Text("%.png".into())]);
    }

    #[test]
    fn gps_has_and_no() {
        let has = build_filter_clause(&Filters { gps: GpsFilter::HasGps, ..Default::default() }).0;
        assert_eq!(has, " AND (m.latitude IS NOT NULL AND m.longitude IS NOT NULL)");
        let no = build_filter_clause(&Filters { gps: GpsFilter::NoGps, ..Default::default() }).0;
        assert_eq!(no, " AND (m.latitude IS NULL OR m.longitude IS NULL)");
    }

    #[test]
    fn combined_filters_join_with_and() {
        let f = Filters {
            size_min: Some(10),
            size_max: None,
            extensions: vec!["nef".into()],
            gps: GpsFilter::HasGps,
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND m.file_size >= ? AND (lower(i.path) LIKE ?) AND (m.latitude IS NOT NULL AND m.longitude IS NOT NULL)"
        );
        assert_eq!(params, vec![Value::Integer(10), Value::Text("%.nef".into())]);
    }
}
