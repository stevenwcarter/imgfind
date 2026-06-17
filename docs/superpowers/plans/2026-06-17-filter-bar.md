# Filter Bar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a filter bar (file-size range slider, file-type multi-select, GPS-present tri-state) beneath the Slint search bar, with live-updating results that work standalone (browse) or as a refinement of a text/vector search.

**Architecture:** A pure `Filters` struct + a shared SQL WHERE-clause builder in the `imgfind` library drive two query paths: a new non-vector `Database::browse` and the existing vector search (which gains the filter clauses + a raised `k`). The Slint GUI vendors a two-handle `RangeSlider`, mirrors filter state into a Rust `Filters`, and re-runs the active query (debounced) off the UI thread on any change.

**Tech Stack:** Rust edition 2024, `imgfind` library (`rusqlite` 0.37 / `sqlite-vec` vec0), Slint 1.x (`imgfind-gui`), `image` 0.25.

## Global Constraints

- Rust edition 2024; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt -p imgfind -p imgfind-gui` (NOT `cargo fmt --all` — it touches the sibling `../clipper` repo). Run `cargo fmt -p imgfind -p imgfind-gui --check` before each commit.
- anyhow `Context`/`with_context` at every fallible boundary.
- Filter `LIKE` patterns and size bounds are ALWAYS bound params (`rusqlite::types::Value` via `params_from_iter`), NEVER interpolated. (`k`/distance in the vec query stay interpolated as today — they're trusted numerics.)
- Reuse `SearchConfig::default()` (distance ≤ 1.3, max_k 100) and `PAGE_SIZE` (80). Filtered vector search raises `k` to `max_k` so a page survives post-MATCH filtering; total filtered results stay bounded by `max_k` (documented v1 limit).
- `image_metadata` is a LEFT JOIN — images with no metadata row still appear in browse when filters permit (GPS=Any, no size bound).
- Type filter derives extension from `images.path`, case-insensitive (`lower(i.path) LIKE '%.ext'`).
- GUI queries run OFF the UI thread (`std::thread::spawn` + `slint::invoke_from_event_loop` + `Weak<MainWindow>`); `slint::Image` built only inside the closure; closures `Send + 'static`.
- Glyphs on Slint widgets stay ASCII/Latin-1 (project memory: the default font tofus other symbols).

## Verified facts

- `Database::search_similar_images_meta(query_embedding: &[f32], limit, offset, distance_threshold, max_k) -> Result<Vec<(String,f32,Option<i64>)>>` (src/database.rs:766): `SELECT i.path, v.distance, m.file_size FROM {vt} v JOIN images i ON i.id=v.rowid LEFT JOIN image_metadata m ON m.image_id=i.id WHERE v.embedding MATCH ?1 AND k={k} AND v.distance <= {threshold} ORDER BY v.distance LIMIT {limit} OFFSET {offset}`, params `[embedding.as_bytes()]`. `vectors_table()` private; `k=(offset+limit).clamp(1,max_k)`.
- `SearchEngine::search_meta(query_embedding: Vec<f32>, limit, offset, distance_threshold, max_k)` (src/search.rs) normalizes then calls the DB method.
- `image_metadata(file_size, width, height, latitude, longitude, camera_make, camera_model, datetime_taken)` per `image_id`.
- `src/database.rs` imports `use rusqlite::{..., params};` — add `params_from_iter` and `types::Value` as needed. Tests use `temp_db_path()` (src/database.rs:1435) and `db.pool.get()`.
- `imgfind-gui/src/backend.rs` `Backend` (Clone): `search(query, offset)`, `search_similar(rel, offset)`, `browse`(new), `thumbnail`, `abs_path`, `metadata`. `imgfind-gui/src/state.rs`: `SearchResult { path, distance, file_size }`, `PAGE_SIZE`.
- `imgfind-gui/src/main.rs`: `state`, `search_mode: Arc<Mutex<SearchMode>>` (`Text`/`Similar`), `detail`, `lb_index`; `spawn_search`, `spawn_similar`, `build_tiles_model`; off-thread marshal pattern.
- utmost RangeSlider: `~/src/utmost/crates/utmost-gui/ui/range_slider.slint` — `in-out property <float> lo/hi` (0..1), `callback range-changed(float, float)`. Vendor verbatim.

---

## File Structure

- `src/filters.rs` (new) — `Filters`, `GpsFilter`, `build_filter_clause`; `pub mod filters;` in `src/lib.rs`.
- `src/database.rs` — `browse`, `distinct_extensions`, `file_size_bounds`; filter params spliced into `search_similar_images_meta`.
- `src/search.rs` — `SearchEngine::search_meta` gains a `&Filters` arg (passthrough).
- `imgfind-gui/src/backend.rs` — `browse`, `extensions`, `size_bounds`; `&Filters` on `search`/`search_similar`.
- `imgfind-gui/ui/range_slider.slint` (new, vendored) + `imgfind-gui/ui/app.slint` (filter row).
- `imgfind-gui/src/main.rs` — filter state, populate, debounced re-query, thread filters.
- `CLAUDE.md` — doc the filter bar.

---

## Task 1: `Filters` + `build_filter_clause` (pure, library)

**Files:**
- Create: `src/filters.rs`
- Modify: `src/lib.rs` (`pub mod filters;`)

**Interfaces:**
- Produces: `Filters { size_min: Option<i64>, size_max: Option<i64>, extensions: Vec<String>, gps: GpsFilter }` (Clone, Debug, Default, PartialEq); `enum GpsFilter { Any (default), HasGps, NoGps }`; `pub fn build_filter_clause(f: &Filters) -> (String, Vec<rusqlite::types::Value>)` returning a SQL fragment beginning with `" AND "` (or empty) plus ordered bound params.

- [ ] **Step 1: Write the failing tests**

Create `src/filters.rs`:

```rust
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
```

Add `pub mod filters;` to `src/lib.rs` (alphabetical-ish, near `config`).

- [ ] **Step 2: Run — fail then pass**

Run: `cargo test -p imgfind --lib filters::` → FAIL (module missing) → after creating, PASS (6 tests).

- [ ] **Step 3: clippy + commit**

```bash
cargo clippy -p imgfind --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add src/filters.rs src/lib.rs
git commit -m "feat(filters): Filters type + SQL clause builder (pure, tested)"
```

---

## Task 2: `Database::browse` + `distinct_extensions` + `file_size_bounds`

**Files:**
- Modify: `src/database.rs`

**Interfaces:**
- Consumes: `crate::filters::{Filters, build_filter_clause}`.
- Produces:
  - `pub fn browse(&self, f: &Filters, limit: usize, offset: usize) -> Result<Vec<(String, Option<i64>)>>`
  - `pub fn distinct_extensions(&self) -> Result<Vec<String>>`
  - `pub fn file_size_bounds(&self) -> Result<(i64, i64)>`

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/database.rs` (use `temp_db_path()`):

```rust
#[test]
fn browse_filters_by_size_type_and_gps() {
    use crate::filters::{Filters, GpsFilter};
    let db_path = temp_db_path();
    let db = Database::new(&db_path).expect("db");
    {
        let conn = db.pool.get().expect("conn");
        // (id, path, size, lat, lon)
        let rows = [
            (1, "a.jpg", 1000i64, Some(1.0f64), Some(2.0f64)),
            (2, "b.png", 5000, None, None),
            (3, "c.jpg", 9000, Some(3.0), Some(4.0)),
            (4, "d.nef", 200, None, None),
        ];
        for (id, path, size, lat, lon) in rows {
            conn.execute("INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, path, format!("h{id}")]).unwrap();
            conn.execute(
                "INSERT INTO image_metadata (image_id, file_size, latitude, longitude) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, size, lat, lon]).unwrap();
        }
    }

    let all = db.browse(&Filters::default(), 100, 0).unwrap();
    assert_eq!(all.len(), 4);

    // jpg only
    let jpg = db.browse(&Filters { extensions: vec!["jpg".into()], ..Default::default() }, 100, 0).unwrap();
    let p: Vec<&str> = jpg.iter().map(|(x,_)| x.as_str()).collect();
    assert_eq!(p.len(), 2);
    assert!(p.contains(&"a.jpg") && p.contains(&"c.jpg"));

    // size 500..6000
    let sized = db.browse(&Filters { size_min: Some(500), size_max: Some(6000), ..Default::default() }, 100, 0).unwrap();
    let p: Vec<&str> = sized.iter().map(|(x,_)| x.as_str()).collect();
    assert!(p.contains(&"a.jpg") && p.contains(&"b.png") && !p.contains(&"c.jpg") && !p.contains(&"d.nef"));

    // has GPS
    let gps = db.browse(&Filters { gps: GpsFilter::HasGps, ..Default::default() }, 100, 0).unwrap();
    let p: Vec<&str> = gps.iter().map(|(x,_)| x.as_str()).collect();
    assert_eq!(p.len(), 2);
    assert!(p.contains(&"a.jpg") && p.contains(&"c.jpg"));

    let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
}

#[test]
fn distinct_extensions_and_size_bounds() {
    let db_path = temp_db_path();
    let db = Database::new(&db_path).expect("db");
    {
        let conn = db.pool.get().expect("conn");
        for (id, path, size) in [(1,"a.JPG",10i64),(2,"b.png",50),(3,"c.jpg",30)] {
            conn.execute("INSERT INTO images (id, path, hash) VALUES (?1,?2,?3)",
                rusqlite::params![id, path, format!("h{id}")]).unwrap();
            conn.execute("INSERT INTO image_metadata (image_id, file_size) VALUES (?1,?2)",
                rusqlite::params![id, size]).unwrap();
        }
    }
    let mut exts = db.distinct_extensions().unwrap();
    exts.sort();
    assert_eq!(exts, vec!["jpg".to_string(), "png".to_string()]); // lowercased, deduped
    assert_eq!(db.file_size_bounds().unwrap(), (10, 50));
    let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p imgfind --lib database::tests::browse_filters_by_size_type_and_gps database::tests::distinct_extensions_and_size_bounds`
Expected: FAIL — methods missing.

- [ ] **Step 3: Implement the three methods**

Add to `impl Database` (add `use rusqlite::params_from_iter;` and `use rusqlite::types::Value;` at the top of `database.rs` if absent; `crate::filters::{Filters, build_filter_clause}`):

```rust
/// Browse all indexed images matching `f` (no vector search), most-recent first.
pub fn browse(&self, f: &Filters, limit: usize, offset: usize) -> Result<Vec<(String, Option<i64>)>> {
    let (clause, mut values) = build_filter_clause(f);
    let sql = format!(
        "SELECT i.path, m.file_size
           FROM images i
           LEFT JOIN image_metadata m ON m.image_id = i.id
          WHERE 1=1{clause}
          ORDER BY m.datetime_taken DESC, i.id DESC
          LIMIT ? OFFSET ?"
    );
    values.push(Value::Integer(limit as i64));
    values.push(Value::Integer(offset as i64));
    let conn = self.pool.get().context("DB connection for browse")?;
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Distinct lowercased file extensions present across indexed image paths.
pub fn distinct_extensions(&self) -> Result<Vec<String>> {
    let conn = self.pool.get().context("DB connection for distinct_extensions")?;
    let mut stmt = conn.prepare("SELECT DISTINCT lower(path) FROM images")?;
    let paths = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut set = std::collections::BTreeSet::new();
    for p in paths {
        if let Some((_, ext)) = p.rsplit_once('.') {
            if !ext.is_empty() && !ext.contains('/') {
                set.insert(ext.to_string());
            }
        }
    }
    Ok(set.into_iter().collect())
}

/// (min, max) of non-null file sizes; (0, 0) when there are none.
pub fn file_size_bounds(&self) -> Result<(i64, i64)> {
    let conn = self.pool.get().context("DB connection for file_size_bounds")?;
    let (min, max): (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT MIN(file_size), MAX(file_size) FROM image_metadata WHERE file_size IS NOT NULL",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((min.unwrap_or(0), max.unwrap_or(0)))
}
```

> `ORDER BY m.datetime_taken DESC` puts NULLs last in SQLite only with `DESC`? SQLite sorts NULLs first for ASC and last for DESC by default — wait, SQLite sorts NULL as smallest, so `DESC` puts NULLs LAST. Good, that matches "recent first, undated last". Keep `ORDER BY m.datetime_taken DESC, i.id DESC`.

- [ ] **Step 4: Run — pass**

Run: `cargo test -p imgfind --lib database::tests::browse_filters_by_size_type_and_gps database::tests::distinct_extensions_and_size_bounds`
Expected: PASS.

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy -p imgfind --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add src/database.rs
git commit -m "feat(db): browse(filters) + distinct_extensions + file_size_bounds"
```

---

## Task 3: Filtered vector search

**Files:**
- Modify: `src/database.rs` (`search_similar_images_meta`), `src/search.rs` (`search_meta`)

**Interfaces:**
- Consumes: `Filters`, `build_filter_clause`.
- Produces: `search_similar_images_meta` gains a trailing `filters: &Filters` param; `SearchEngine::search_meta` gains a trailing `filters: &Filters` param (passthrough). Both keep the same return type.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/database.rs`:

```rust
#[test]
fn filtered_vector_search_excludes_nonmatching_types() {
    use crate::filters::{Filters, GpsFilter};
    use zerocopy::IntoBytes;
    let db_path = temp_db_path();
    let db = Database::new(&db_path).expect("db");
    {
        let conn = db.pool.get().expect("conn");
        // Two near-identical embeddings; different extensions.
        let mut a = vec![0.0f32; 512]; a[0] = 1.0;
        let b = a.clone();
        for (id, path, emb) in [(1,"a.jpg",&a),(2,"b.png",&b)] {
            conn.execute("INSERT INTO images (id, path, hash) VALUES (?1,?2,?3)",
                rusqlite::params![id, path, format!("h{id}")]).unwrap();
            conn.execute("INSERT INTO image_metadata (image_id, file_size) VALUES (?1, 1000)",
                rusqlite::params![id]).unwrap();
            conn.execute("INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![id, emb.as_bytes()]).unwrap();
        }
    }
    // Query close to both, filter to jpg only → only a.jpg.
    let mut q = vec![0.0f32; 512]; q[0] = 1.0;
    let jpg_only = Filters { extensions: vec!["jpg".into()], ..Default::default() };
    let rows = db.search_similar_images_meta(&q, 80, 0, 1.3, 100, &jpg_only).unwrap();
    let paths: Vec<&str> = rows.iter().map(|(p,_,_)| p.as_str()).collect();
    assert!(paths.contains(&"a.jpg"));
    assert!(!paths.contains(&"b.png"), "png filtered out of vector results");
    // No filter → both present.
    let both = db.search_similar_images_meta(&q, 80, 0, 1.3, 100, &Filters::default()).unwrap();
    assert_eq!(both.len(), 2);
    let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
}
```

- [ ] **Step 2: Run — fail (arity mismatch)**

Run: `cargo test -p imgfind --lib database::tests::filtered_vector_search_excludes_nonmatching_types`
Expected: FAIL — `search_similar_images_meta` takes 5 args, test passes 6.

- [ ] **Step 3: Thread filters into the vector query**

In `search_similar_images_meta`, add `filters: &Filters` as the last param. Raise `k` to cover filtering, build the clause, splice it after the existing predicates, and pass embedding + filter params via `params_from_iter`. Replace the body's query + exec:

```rust
// k must cover offset+limit AFTER filtering; raise to max_k so a full page
// can survive post-MATCH filtering.
let k = max_k.max(offset + limit).clamp(1, max_k);
let vt = self.vectors_table()?;
let (clause, fvalues) = crate::filters::build_filter_clause(filters);

let query = format!(
    "SELECT i.path, v.distance, m.file_size
       FROM {vt} v
       JOIN images i ON i.id = v.rowid
       LEFT JOIN image_metadata m ON m.image_id = i.id
      WHERE v.embedding MATCH ? AND k = {k}
        AND v.distance <= {distance_threshold:.6}{clause}
      ORDER BY v.distance LIMIT {limit} OFFSET {offset}"
);

let conn = self.pool.get().context("DB connection for filtered vector search")?;
let mut stmt = conn.prepare(&query)?;
// Param order: embedding blob first (the `?` in MATCH), then filter params.
let mut values: Vec<rusqlite::types::Value> =
    vec![rusqlite::types::Value::Blob(query_embedding.as_bytes().to_vec())];
values.extend(fvalues);
let results = stmt.query_map(params_from_iter(values), |row| {
    Ok((row.get::<_, String>(0)?, row.get::<_, f32>(1)?, row.get::<_, Option<i64>>(2)?))
})?;
let mut search_results = Vec::new();
for r in results { search_results.push(r?); }
Ok(search_results)
```

> Note the MATCH placeholder changed from `?1` to `?` so it composes with the appended filter `?`s under `params_from_iter` (positional + anonymous can't be mixed). Update `SearchEngine::search_meta` to accept `filters: &Filters` and pass it through to this method. Check ALL callers of both functions and update them (the GUI backend in Task 4, plus any CLI caller in `src/main.rs` — pass `&Filters::default()` at non-GUI call sites to preserve behavior).

- [ ] **Step 4: Run — pass + full suite**

Run: `cargo test -p imgfind --lib database::tests::filtered_vector_search_excludes_nonmatching_types`
Expected: PASS.
Run: `cargo test -p imgfind` (fix any caller arity breakage, e.g. CLI `search` passing `&Filters::default()`).
Expected: PASS.

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add src/database.rs src/search.rs src/main.rs
git commit -m "feat(db): filtered vector search (filter clauses + raised k)"
```

---

## Task 4: Backend filter surface

**Files:**
- Modify: `imgfind-gui/src/backend.rs`

**Interfaces:**
- Consumes: `Database::{browse, distinct_extensions, file_size_bounds}`, filtered `search_meta`, `imgfind::filters::Filters`.
- Produces (on `Backend`): `browse(&Filters, offset) -> Result<Vec<SearchResult>>`; `extensions() -> Result<Vec<String>>`; `size_bounds() -> Result<(i64,i64)>`; `search` and `search_similar` gain a trailing `filters: &Filters` param.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `imgfind-gui/src/backend.rs` (reuse `temp_db`/`backend_with`):

```rust
#[test]
fn backend_browse_applies_filters() {
    use imgfind::filters::{Filters, GpsFilter};
    let (db, root) = temp_db();
    {
        let conn = db.pool.get().unwrap();
        for (id, path, size, lat) in [(1,"a.jpg",1000i64,Some(1.0f64)),(2,"b.png",50,None::<f64>)] {
            conn.execute("INSERT INTO images (id,path,hash) VALUES (?1,?2,?3)",
                rusqlite::params![id, path, format!("h{id}")]).unwrap();
            conn.execute("INSERT INTO image_metadata (image_id,file_size,latitude,longitude) VALUES (?1,?2,?3,?3)",
                rusqlite::params![id, size, lat]).unwrap();
        }
    }
    let backend = backend_with(db);
    let jpg = backend.browse(&Filters { extensions: vec!["jpg".into()], ..Default::default() }, 0).unwrap();
    assert_eq!(jpg.len(), 1);
    assert_eq!(jpg[0].path, "a.jpg");
    let gps = backend.browse(&Filters { gps: GpsFilter::HasGps, ..Default::default() }, 0).unwrap();
    assert_eq!(gps.len(), 1);
    assert_eq!(gps[0].path, "a.jpg");
    let _ = std::fs::remove_dir_all(root);
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p imgfind-gui backend::tests::backend_browse_applies_filters`
Expected: FAIL — `browse` missing.

- [ ] **Step 3: Implement**

In `impl Backend` (import `use imgfind::filters::Filters;`):

```rust
pub fn browse(&self, filters: &Filters, offset: usize) -> Result<Vec<SearchResult>> {
    let rows = self.db.browse(filters, PAGE_SIZE, offset).context("Browse failed")?;
    Ok(rows.into_iter()
        .map(|(path, file_size)| SearchResult { path, distance: 0.0, file_size })
        .collect())
}

pub fn extensions(&self) -> Result<Vec<String>> {
    self.db.distinct_extensions().context("Failed to list extensions")
}

pub fn size_bounds(&self) -> Result<(i64, i64)> {
    self.db.file_size_bounds().context("Failed to read size bounds")
}
```

Update `search` and `search_similar` to take `filters: &Filters` and forward it: `engine.search_meta(embedding, PAGE_SIZE, offset, sc.distance_threshold, sc.max_k, filters)` and `find_similar_to_path(..)` — note `find_similar_to_path` also calls `search_similar_images_meta`, so it ALSO needs a `&Filters` param threaded through (add it, default at non-filter callers). Update the existing backend tests that call `search`/`search_similar` to pass `&Filters::default()`.

- [ ] **Step 4: Run — pass**

Run: `cargo test -p imgfind-gui` → all pass.

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add imgfind-gui/src/backend.rs src/database.rs
git commit -m "feat(gui): Backend browse/extensions/size_bounds + filters on search"
```

---

## Task 5: Slint filter-bar UI (vendored RangeSlider + controls)

**Files:**
- Create: `imgfind-gui/ui/range_slider.slint` (vendored from utmost, verbatim)
- Modify: `imgfind-gui/ui/app.slint`

**Interfaces:**
- Produces (on `MainWindow`): properties `size-lo: float` / `size-hi: float` (0..1), `size-label: string`, `available-extensions: [string]`, `selected-extensions: [string]`, `gps-mode: int` (0=Any,1=Has,2=No); callbacks `filters-changed()`, `ext-toggled(string)`, `gps-mode-changed(int)`. (Rust pushes options in; UI emits changes out.)

- [ ] **Step 1: Vendor the RangeSlider**

Copy `~/src/utmost/crates/utmost-gui/ui/range_slider.slint` to `imgfind-gui/ui/range_slider.slint` VERBATIM (it's self-contained: `export component RangeSlider` with `lo`/`hi` 0..1 and `range-changed(float,float)`).

- [ ] **Step 2: Add the filter row to `app.slint`**

`import { RangeSlider } from "range_slider.slint";`. Add a filter row beneath the search bar (inside the existing top `VerticalLayout`, after the search `HorizontalLayout`):
- `RangeSlider { lo <=> root.size-lo; hi <=> root.size-hi; range-changed(lo,hi) => { root.size-lo = lo; root.size-hi = hi; root.filters-changed(); } }` + a `Text { text: root.size-label; }`.
- A row of file-type toggle chips: `for ext in root.available-extensions: Rectangle { … TouchArea { clicked => { root.ext-toggled(ext); } } }` styled selected/unselected based on whether `ext` is in `selected-extensions` (compute membership in the chip via a helper property or by passing a parallel bool list — simplest: a small `is-selected(string)->bool` pure function isn't available in Slint, so instead expose `selected-extensions` and compare; if membership-in-array is awkward in Slint, pass `available-extensions` as a `[{name: string, on: bool}]` struct list set from Rust and toggle via `ext-toggled`).
- A 3-way GPS control: three toggle buttons (Any / Has GPS / No GPS) highlighting `gps-mode`, each `clicked => { root.gps-mode-changed(0|1|2); }`. Use ASCII/Latin-1 labels only (project memory: default font tofus symbols).

Keep glyphs/labels plain text. Behavior contract: changing any control ultimately calls a Rust-facing callback (`filters-changed` / `ext-toggled` / `gps-mode-changed`). Consult Slint 1.x docs via context7 for array-membership / struct-model patterns if needed.

- [ ] **Step 3: Build (headless cannot run the GUI)**

Run: `cargo build -p imgfind-gui` → compiles (callbacks can be temporarily registered as no-ops in main.rs to satisfy the build; Task 6 implements them).
Run: `cargo clippy --workspace --all-targets -- -D warnings` → clean.

- [ ] **Step 4: fmt + commit**

```bash
cargo fmt -p imgfind -p imgfind-gui
git add imgfind-gui/ui/range_slider.slint imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): filter-bar UI — vendored RangeSlider + type chips + GPS control"
```

---

## Task 6: main.rs wiring — populate, debounce, thread filters

**Files:**
- Modify: `imgfind-gui/src/main.rs`

**Interfaces:**
- Consumes: `Backend::{browse, extensions, size_bounds, search, search_similar}`, `imgfind::filters::{Filters, GpsFilter}`, the Task 5 callbacks/properties.

- [ ] **Step 1: Hold filter state + populate the UI at startup**

Add `let filters: Arc<Mutex<Filters>> = Arc::new(Mutex::new(Filters::default()));` and `let size_bounds = backend.size_bounds().unwrap_or((0, 0));`. At startup (after backend opens), set `available-extensions` from `backend.extensions()` and the initial `size-label`. Keep a copy of `size_bounds` for the [0,1]↔bytes mapping: `bytes = lo*(max-min)+min`. A `Filters::default()` (no extensions, gps Any, no size bounds) must mean "everything" — only set `size_min/size_max` when the slider is off its 0/1 extremes (so the default browse shows all).

- [ ] **Step 2: Implement a debounced re-query on `filters-changed`/`ext-toggled`/`gps-mode-changed`**

Maintain a single-shot `slint::Timer` restarted on each filter change; on fire, rebuild `*filters` from the current UI state ([0,1]→bytes for size with the "extremes = unbounded" rule; selected extensions; gps-mode→`GpsFilter`), set `size-label` ("X–Y MB"), then re-run the active query: read `search_mode` — `Text(q)` non-empty → `spawn_search(q, 0, &filters)`; otherwise → a new `spawn_browse(&filters, 0)` helper (mirrors `spawn_search`'s off-thread marshal but calls `backend.browse`). Debounce ~250 ms so dragging the slider coalesces into one query. `ext-toggled(ext)` flips membership in the selected set (held in `filters`/a parallel `Arc`), updates the chip model, then triggers the debounce; same for `gps-mode-changed`.

- [ ] **Step 3: Thread filters through every query path**

`spawn_search`/`spawn_similar` and the `on_search`/`on_search_similar`/`on_load_more` handlers all pass the CURRENT `*filters.lock()` snapshot to `backend.search`/`search_similar`/`browse`. When the text query is empty but filters are active, dispatch `browse` instead of returning early (the old empty-query early-return must now run a filtered browse if any filter is active; if NO filter is active and query empty, keep clearing the grid as before). Load-more uses the same filters at `next_offset()`.

- [ ] **Step 4: Build + verify (headless cannot run the GUI)**

Run: `cargo build -p imgfind-gui`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` → all clean/pass.
Document manual smoke steps in the report: drag size slider → grid live-updates (debounced) to in-range images; toggle a type chip → only those types; GPS Has/No → filtered; with a text query, filters narrow the vector results; clearing the query with filters active browses filtered; Load more respects filters.

- [ ] **Step 5: fmt + commit**

```bash
cargo fmt -p imgfind -p imgfind-gui
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): wire filters — populate, debounce, live re-query (browse + search)"
```

---

## Task 7: Docs

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1:** In the `Native GUI (imgfind-gui/)` bullet, add: a filter bar beneath the search bar (file-size range slider, file-type chips, GPS tri-state) live-updates results; with no query it browses all images matching the filters, with a query it narrows the vector results; the filter model (`imgfind::filters::Filters` + `build_filter_clause`) is built to extend. One-line pointer to `docs/superpowers/specs/2026-06-17-filter-bar-design.md`.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document the GUI filter bar"
```

---

## Self-Review

**Spec coverage:**
- `Filters` + extensible clause builder → Task 1. ✓
- Standalone browse query → Task 2 (`browse`); filter-bar options (`distinct_extensions`, `file_size_bounds`) → Task 2. ✓
- Filtered vector search (refine) + raised k → Task 3. ✓
- Backend surface + filters on search/similar → Task 4. ✓
- Range slider (vendored) + type chips + GPS control UI → Task 5. ✓
- Live-update debounce + thread filters through all paths + browse-on-empty-query → Task 6. ✓
- Docs → Task 7. ✓
- Testing: pure builder (T1), browse/exts/bounds integration (T2), filtered vector (T3), backend browse (T4), UI by running (T5/T6). ✓
- Invariants (LEFT JOIN metadata, case-insensitive ext, bound params, SearchConfig/PAGE_SIZE) → T1-T4 + constraints. ✓

**Placeholder scan:** No TBD/"handle edge cases". UI array-membership uncertainty (T5) carries an explicit fallback (struct-model `[{name,on}]`) + context7 instruction. The "extremes = unbounded" size rule is stated concretely (T6 steps 1-2).

**Type consistency:** `build_filter_clause(&Filters)->(String, Vec<Value>)` (T1) used by `browse`/`search_similar_images_meta` (T2/T3); `browse(&Filters,limit,offset)->Vec<(String,Option<i64>)>` (T2) wrapped by `Backend::browse(&Filters,offset)->Vec<SearchResult>` (T4); `search_meta`/`search_similar_images_meta`/`find_similar_to_path` all gain a trailing `&Filters` (T3/T4) — every existing caller updated to pass `&Filters::default()`. Slint callback/property names consistent between T5 and T6.

**Risk flags for implementer:** (1) The positional→anonymous placeholder change in the vec query (T3) — every existing caller of `search_similar_images_meta`/`search_meta`/`find_similar_to_path` must be updated for the new `&Filters` arg; `cargo build` will surface them. (2) Slint array-membership for chip selection (T5) — use a struct-model `[{name,on}]` if `in selected-extensions` checks are awkward; consult context7. (3) `find_similar_to_path` threads `&Filters` too (so search-similar honours filters) — don't miss it. The correctness-critical SQL/logic (T1-T4) is fully unit-tested; the GUI (T5/T6) is build + manual.
