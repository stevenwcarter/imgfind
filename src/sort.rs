//! Shared sort model for browse/search ordering (CLI + GUI).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    Name,
    Size,
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub key: SortKey,
    pub dir: SortDir,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            key: SortKey::Name,
            dir: SortDir::Asc,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowMeta {
    pub id: i64,
    pub path: String,
    pub size: Option<i64>,
    pub ext: String,
}

/// SQL expression extracting the lowercased file extension from `i.path`,
/// matching the Rust-side `rsplit_once('.')` used by `distinct_extensions`
/// (empty string when there is no dot).
pub fn ext_sql_expr() -> &'static str {
    // Reverse the path, take chars up to the first '.', reverse back, lowercase.
    // Equivalent to taking the substring after the last '.'.
    "lower(CASE WHEN instr(i.path, '.') = 0 THEN '' \
     ELSE replace(i.path, rtrim(i.path, replace(i.path, '.', '')), '') END)"
}

fn dir_kw(dir: SortDir) -> &'static str {
    match dir {
        SortDir::Asc => "ASC",
        SortDir::Desc => "DESC",
    }
}

/// Build the `ORDER BY` body (without the `ORDER BY` keyword) for a browse query.
/// Size/Type always tie-break on `i.path ASC`; Size sorts NULLs last.
pub fn order_by_clause(sort: &Sort) -> String {
    let d = dir_kw(sort.dir);
    match sort.key {
        SortKey::Name => format!("i.path {d}"),
        SortKey::Size => format!("m.file_size IS NULL, m.file_size {d}, i.path ASC"),
        SortKey::Type => format!("{} {d}, i.path ASC", ext_sql_expr()),
    }
}

use std::cmp::Ordering;

fn ord_dir(o: Ordering, asc: bool) -> Ordering {
    if asc { o } else { o.reverse() }
}

/// In-place stable sort of result rows matching `order_by_clause`.
///
/// Ordering rules mirror the SQL:
/// - `Name`: sort by path with direction.
/// - `Size`: NULLs (`None`) always last regardless of direction, then value
///   by direction, then path ASC as tiebreak.
/// - `Type`: ext by direction, then path ASC as tiebreak.
pub fn sort_rows(rows: &mut [RowMeta], sort: &Sort) {
    let asc = matches!(sort.dir, SortDir::Asc);
    rows.sort_by(|a, b| match sort.key {
        SortKey::Name => ord_dir(a.path.cmp(&b.path), asc),
        SortKey::Size => {
            let primary = match (a.size, b.size) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater, // NULL last regardless of dir
                (Some(_), None) => Ordering::Less,
                (Some(x), Some(y)) => ord_dir(x.cmp(&y), asc),
            };
            primary.then_with(|| a.path.cmp(&b.path))
        }
        SortKey::Type => ord_dir(a.ext.cmp(&b.ext), asc).then_with(|| a.path.cmp(&b.path)),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(key: SortKey, dir: SortDir) -> Sort {
        Sort { key, dir }
    }

    #[test]
    fn name_clause_uses_path_only() {
        assert_eq!(
            order_by_clause(&s(SortKey::Name, SortDir::Asc)),
            "i.path ASC"
        );
        assert_eq!(
            order_by_clause(&s(SortKey::Name, SortDir::Desc)),
            "i.path DESC"
        );
    }

    #[test]
    fn size_clause_nulls_last_then_path_tiebreak() {
        assert_eq!(
            order_by_clause(&s(SortKey::Size, SortDir::Asc)),
            "m.file_size IS NULL, m.file_size ASC, i.path ASC"
        );
        assert_eq!(
            order_by_clause(&s(SortKey::Size, SortDir::Desc)),
            "m.file_size IS NULL, m.file_size DESC, i.path ASC"
        );
    }

    #[test]
    fn type_clause_uses_ext_expr_then_path_tiebreak() {
        // ext_sql_expr() is the shared extension expression; secondary key is path ASC.
        let c = order_by_clause(&s(SortKey::Type, SortDir::Desc));
        assert_eq!(c, format!("{} DESC, i.path ASC", ext_sql_expr()));
    }

    #[test]
    fn serde_reprs_are_lowercase() {
        assert_eq!(serde_json::to_string(&SortKey::Type).unwrap(), "\"type\"");
        assert_eq!(serde_json::to_string(&SortDir::Asc).unwrap(), "\"asc\"");
    }

    #[test]
    fn sort_rows_by_size_nulls_last_path_tiebreak() {
        let mut rows = vec![
            RowMeta {
                id: 1,
                path: "b.jpg".into(),
                size: Some(10),
                ext: "jpg".into(),
            },
            RowMeta {
                id: 2,
                path: "a.jpg".into(),
                size: None,
                ext: "jpg".into(),
            },
            RowMeta {
                id: 3,
                path: "c.jpg".into(),
                size: Some(10),
                ext: "jpg".into(),
            },
            RowMeta {
                id: 4,
                path: "d.jpg".into(),
                size: Some(5),
                ext: "jpg".into(),
            },
        ];
        sort_rows(
            &mut rows,
            &Sort {
                key: SortKey::Size,
                dir: SortDir::Asc,
            },
        );
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![4, 1, 3, 2]
        );
    }

    #[test]
    fn sort_rows_by_type_then_name() {
        let mut rows = vec![
            RowMeta {
                id: 1,
                path: "z.png".into(),
                size: None,
                ext: "png".into(),
            },
            RowMeta {
                id: 2,
                path: "a.png".into(),
                size: None,
                ext: "png".into(),
            },
            RowMeta {
                id: 3,
                path: "m.jpg".into(),
                size: None,
                ext: "jpg".into(),
            },
        ];
        sort_rows(
            &mut rows,
            &Sort {
                key: SortKey::Type,
                dir: SortDir::Asc,
            },
        );
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 2, 1]);
    }

    #[test]
    fn sort_rows_by_name_desc() {
        let mut rows = vec![
            RowMeta {
                id: 1,
                path: "a.jpg".into(),
                size: None,
                ext: "jpg".into(),
            },
            RowMeta {
                id: 2,
                path: "b.jpg".into(),
                size: None,
                ext: "jpg".into(),
            },
        ];
        sort_rows(
            &mut rows,
            &Sort {
                key: SortKey::Name,
                dir: SortDir::Desc,
            },
        );
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 1]);
    }
}
