# GUI Virtualized Scroll, Sorting, Thumbnail Persistence, Preload & Session State — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `imgfind-gui` a virtualized moving-window infinite-scroll grid, a Name/Size/Type sort selector, thumbnail persistence across all GUI surfaces, neighbor preloading, browse-all-on-startup with a configurable default sort, and exact session restore persisted in the `.imgfind` DB.

**Architecture:** Approach C — the full filtered+sorted result set is held in memory as a lightweight `Vec<RowMeta>`; the grid virtualizes only thumbnail decode/render via a bounded window over a Slint `Flickable`. New core-crate types (`Sort`, `RowMeta`, `UiState`) are shared by CLI and GUI. Session state persists once on exit into a new single-row `ui_state` table (migration 003) and rehydrates by image-id reference on startup.

**Tech Stack:** Rust edition 2024, `rusqlite` + `r2d2`, `sqlite-vec`, `serde`/`serde_json`/`toml`, Slint 1.x, `tracing`, `anyhow`.

## Global Constraints

- Rust edition 2024; all code clippy- and rustfmt-clean (dispatch Rust coding to the `rust-developer` agent).
- Errors use `anyhow` with `Context`/`with_context`; logging via `tracing`.
- DB stores **relative** paths (relative to the dir containing `.imgfind`); convert with `relative_to_abs_path`/`abs_to_relative_path` at every filesystem boundary.
- Slint button/label text must be ASCII / Latin-1 only (no symbol glyphs — they render as tofu). Use ASCII for the sort direction toggle.
- Thumbnail table is keyed by `(image_hash, size)` and already supports arbitrary sizes — no schema change for the 2048 size.
- Migrations are idempotent (`IF NOT EXISTS`) and bump `PRAGMA user_version`.
- Run `cargo test --workspace` and `cargo clippy --workspace --all-targets` clean before each commit; run `cargo build -p imgfind-gui` for GUI-wiring tasks (no unit tests).
- Spec: `docs/superpowers/specs/2026-06-19-gui-virtualized-scroll-sort-state-design.md`.

---

## File Structure

**Core crate (`imgfind`):**
- Create `src/sort.rs` — `SortKey`, `SortDir`, `Sort`, `order_by_clause`, `sort_rows`, `RowMeta`.
- Create `src/ui_state.rs` — `UiState`, `PersistedMode` (serde).
- Modify `src/lib.rs` — register `mod sort;`, `mod ui_state;` and re-export.
- Modify `src/database.rs` — `browse_all`, `rehydrate_rows`, search-meta→`RowMeta`, migration 003, `get_ui_state`/`set_ui_state`.
- Modify `src/filters.rs` — (only if `build_filter_clause` needs the extension expression shared; otherwise untouched).
- Modify `src/config.rs` — `GuiConfig` + `[gui]` section.
- Modify `src/thumbnail.rs` — `LIGHTBOX_SIZE`, `GUI_THUMBNAIL_SIZES`.
- Modify `src/main.rs` — `imgfind thumbnails` multi-size + help text; size-resolution helper.

**GUI crate (`imgfind-gui`):**
- Create `src/window.rs` — pure virtualization math (`visible_range`, `window_range`, `need_slide`).
- Create `src/preload.rs` — pure `preload_arc`.
- Modify `src/state.rs` — switch result model to `Vec<RowMeta>` + sort/mode fields.
- Modify `src/main.rs` — windowed loader, sort wiring, lightbox size, preload, startup restore, exit persistence.
- Modify `src/backend.rs` — expose row-meta browse/search + rehydrate passthroughs as needed.
- Modify `ui/app.slint` — virtualized `Flickable` grid, sort selector, remove Load-more.

---

## Task 1: `Sort` types + `order_by_clause` (pure)

**Files:**
- Create: `src/sort.rs`
- Modify: `src/lib.rs` (add `pub mod sort;`)
- Test: in `src/sort.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `SortKey::{Name,Size,Type}`, `SortDir::{Asc,Desc}`, `Sort{key,dir}`, `fn order_by_clause(sort: &Sort) -> String`. `RowMeta{ id:i64, path:String, size:Option<i64>, ext:String }`. All `#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]` (RowMeta: `Clone,Debug,PartialEq`). Enums use lowercase serde rename (`name|size|type`, `asc|desc`).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn s(key: SortKey, dir: SortDir) -> Sort { Sort { key, dir } }

    #[test]
    fn name_clause_uses_path_only() {
        assert_eq!(order_by_clause(&s(SortKey::Name, SortDir::Asc)), "i.path ASC");
        assert_eq!(order_by_clause(&s(SortKey::Name, SortDir::Desc)), "i.path DESC");
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
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind sort::`
Expected: FAIL — `sort` module / items not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Shared sort model for browse/search ordering (CLI + GUI).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey { Name, Size, Type }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDir { Asc, Desc }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort { pub key: SortKey, pub dir: SortDir }

impl Default for Sort {
    fn default() -> Self { Sort { key: SortKey::Name, dir: SortDir::Asc } }
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
    match dir { SortDir::Asc => "ASC", SortDir::Desc => "DESC" }
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
```

Add to `src/lib.rs`: `pub mod sort;`

> **Note for implementer:** verify `ext_sql_expr()` yields the substring after the last `.`. If the `rtrim`/`replace` trick proves brittle for your SQLite version, substitute an equivalent expression (e.g. using `instr` on the reversed string via a recursive expression) — but the test `type_clause_uses_ext_expr_then_path_tiebreak` only checks composition, so also add a `browse_all` integration assertion in Task 3 that two files `a.PNG` and `b.jpg` order by `jpg` < `png`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind sort::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/sort.rs src/lib.rs
git commit -m "feat(core): add Sort/SortKey/SortDir + order_by_clause + RowMeta"
```

---

## Task 2: `sort_rows` in-memory comparator (pure)

**Files:**
- Modify: `src/sort.rs`
- Test: in `src/sort.rs`

**Interfaces:**
- Consumes: `Sort`, `RowMeta`, `SortKey`, `SortDir`.
- Produces: `fn sort_rows(rows: &mut [RowMeta], sort: &Sort)` — stable in-place sort matching `order_by_clause`'s ordering (Size NULLs last; Size/Type tie-break on path asc; Name = path).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn sort_rows_by_size_nulls_last_path_tiebreak() {
    let mut rows = vec![
        RowMeta { id: 1, path: "b.jpg".into(), size: Some(10), ext: "jpg".into() },
        RowMeta { id: 2, path: "a.jpg".into(), size: None, ext: "jpg".into() },
        RowMeta { id: 3, path: "c.jpg".into(), size: Some(10), ext: "jpg".into() },
        RowMeta { id: 4, path: "d.jpg".into(), size: Some(5), ext: "jpg".into() },
    ];
    sort_rows(&mut rows, &Sort { key: SortKey::Size, dir: SortDir::Asc });
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![4, 1, 3, 2]);
}

#[test]
fn sort_rows_by_type_then_name() {
    let mut rows = vec![
        RowMeta { id: 1, path: "z.png".into(), size: None, ext: "png".into() },
        RowMeta { id: 2, path: "a.png".into(), size: None, ext: "png".into() },
        RowMeta { id: 3, path: "m.jpg".into(), size: None, ext: "jpg".into() },
    ];
    sort_rows(&mut rows, &Sort { key: SortKey::Type, dir: SortDir::Asc });
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 2, 1]);
}

#[test]
fn sort_rows_by_name_desc() {
    let mut rows = vec![
        RowMeta { id: 1, path: "a.jpg".into(), size: None, ext: "jpg".into() },
        RowMeta { id: 2, path: "b.jpg".into(), size: None, ext: "jpg".into() },
    ];
    sort_rows(&mut rows, &Sort { key: SortKey::Name, dir: SortDir::Desc });
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 1]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind sort::`
Expected: FAIL — `sort_rows` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
use std::cmp::Ordering;

/// In-place stable sort of result rows matching `order_by_clause`.
pub fn sort_rows(rows: &mut [RowMeta], sort: &Sort) {
    let asc = matches!(sort.dir, SortDir::Asc);
    rows.sort_by(|a, b| {
        let primary = match sort.key {
            SortKey::Name => a.path.cmp(&b.path),
            SortKey::Size => match (a.size, b.size) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater, // NULL last regardless of dir
                (Some(_), None) => Ordering::Less,
                (Some(x), Some(y)) => ord_dir(x.cmp(&y), asc),
            },
            SortKey::Type => ord_dir(a.ext.cmp(&b.ext), asc),
        };
        // Name's own primary must honor direction; Size/Type tie-break on path ASC.
        match sort.key {
            SortKey::Name => ord_dir(a.path.cmp(&b.path), asc),
            _ => primary.then_with(|| a.path.cmp(&b.path)),
        }
    });
}

fn ord_dir(o: Ordering, asc: bool) -> Ordering {
    if asc { o } else { o.reverse() }
}
```

> Note: for `SortKey::Size`/`Type`, the NULL/primary comparison above already applies direction to the value comparison while keeping NULLs last and the path tiebreak ascending — matching the SQL. The `primary` binding for `Name` is unused; the `match` recomputes it cleanly. Simplify if clippy flags the dead binding (e.g. compute primary only in the non-Name arms).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind sort::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/sort.rs
git commit -m "feat(core): add sort_rows in-memory comparator matching SQL order"
```

---

## Task 3: `browse_all` returning ordered `Vec<RowMeta>` (DB)

**Files:**
- Modify: `src/database.rs` (add `browse_all`; keep `browse` or refactor it to delegate)
- Test: `src/database.rs` `#[cfg(test)]` (follow existing DB test setup — look for the current `mod tests` that builds a temp DB)

**Interfaces:**
- Consumes: `Filters`, `Sort`, `order_by_clause`, `RowMeta`.
- Produces: `pub fn browse_all(&self, f: &Filters, sort: &Sort) -> Result<Vec<RowMeta>>` — every matching row in sorted order, no LIMIT/OFFSET. Selects `i.id, i.path, m.file_size` and derives `ext` Rust-side (lowercased substring after last `.`).

- [ ] **Step 1: Write the failing test**

Use the crate's existing DB test harness (temp dir + `Database::new`, insert images + metadata). Add:

```rust
#[test]
fn browse_all_sorts_by_size_then_name_nulls_last() {
    let (db, _tmp) = test_db_with_rows(&[
        // (path, file_size)
        ("b.jpg", Some(10)),
        ("a.jpg", None),
        ("c.jpg", Some(10)),
        ("d.jpg", Some(5)),
    ]);
    let rows = db.browse_all(&Filters::default(),
        &Sort { key: SortKey::Size, dir: SortDir::Asc }).unwrap();
    assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["d.jpg", "b.jpg", "c.jpg", "a.jpg"]);
}

#[test]
fn browse_all_sorts_by_type_then_name() {
    let (db, _tmp) = test_db_with_rows(&[
        ("z.PNG", None), ("a.png", None), ("m.jpg", None),
    ]);
    let rows = db.browse_all(&Filters::default(),
        &Sort { key: SortKey::Type, dir: SortDir::Asc }).unwrap();
    // jpg < png; within png, path asc
    assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["m.jpg", "a.png", "z.PNG"]);
    // ext is lowercased
    assert_eq!(rows[2].ext, "png");
}

#[test]
fn browse_all_name_desc() {
    let (db, _tmp) = test_db_with_rows(&[("a.jpg", None), ("b.jpg", None)]);
    let rows = db.browse_all(&Filters::default(),
        &Sort { key: SortKey::Name, dir: SortDir::Desc }).unwrap();
    assert_eq!(rows[0].path, "b.jpg");
}
```

If no reusable `test_db_with_rows` helper exists, add one in the test module (open a temp `Database`, insert into `images` + `image_metadata`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind database::tests::browse_all`
Expected: FAIL — `browse_all` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Browse all matching images in `sort` order (no pagination). Lightweight rows.
pub fn browse_all(&self, f: &Filters, sort: &Sort) -> Result<Vec<RowMeta>> {
    let (clause, values) = build_filter_clause(f);
    let order = crate::sort::order_by_clause(sort);
    let sql = format!(
        "SELECT i.id, i.path, m.file_size
           FROM images i
           LEFT JOIN image_metadata m ON m.image_id = i.id
          WHERE 1=1{clause}
          ORDER BY {order}"
    );
    let conn = self.pool.get().context("DB connection for browse_all")?;
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            let id: i64 = row.get(0)?;
            let path: String = row.get(1)?;
            let size: Option<i64> = row.get(2)?;
            Ok((id, path, size))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(id, path, size)| {
            let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
            RowMeta { id, path, size, ext }
        })
        .collect();
    Ok(rows)
}
```

Import `RowMeta`, `Sort` at top of `database.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind database::tests::browse_all`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/database.rs
git commit -m "feat(core): add browse_all returning full ordered Vec<RowMeta>"
```

---

## Task 4: `rehydrate_rows` by ordered id list (DB)

**Files:**
- Modify: `src/database.rs`
- Test: `src/database.rs`

**Interfaces:**
- Produces: `pub fn rehydrate_rows(&self, ids: &[i64]) -> Result<Vec<RowMeta>>` — returns `RowMeta` for each id, in the **input order**, silently dropping ids absent from `images`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn rehydrate_preserves_order_and_drops_missing() {
    let (db, _tmp) = test_db_with_rows(&[
        ("a.jpg", Some(1)), ("b.jpg", Some(2)), ("c.jpg", Some(3)),
    ]);
    // ids are 1,2,3 in insert order; request reversed + a missing id 999
    let want = vec![3i64, 999, 1];
    let rows = db.rehydrate_rows(&want).unwrap();
    assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["c.jpg", "a.jpg"]); // 999 dropped, order preserved
}

#[test]
fn rehydrate_empty_is_empty() {
    let (db, _tmp) = test_db_with_rows(&[("a.jpg", Some(1))]);
    assert!(db.rehydrate_rows(&[]).unwrap().is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind database::tests::rehydrate`
Expected: FAIL — not found.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Fetch RowMeta for an explicit ordered id list, preserving input order and
/// dropping ids not present in `images`.
pub fn rehydrate_rows(&self, ids: &[i64]) -> Result<Vec<RowMeta>> {
    if ids.is_empty() { return Ok(Vec::new()); }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT i.id, i.path, m.file_size
           FROM images i
           LEFT JOIN image_metadata m ON m.image_id = i.id
          WHERE i.id IN ({placeholders})"
    );
    let conn = self.pool.get().context("DB connection for rehydrate_rows")?;
    let mut stmt = conn.prepare(&sql)?;
    let found: std::collections::HashMap<i64, RowMeta> = stmt
        .query_map(params_from_iter(ids.iter().map(|i| Value::Integer(*i))), |row| {
            let id: i64 = row.get(0)?;
            let path: String = row.get(1)?;
            let size: Option<i64> = row.get(2)?;
            Ok((id, path, size))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(id, path, size)| {
            let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
            (id, RowMeta { id, path, size, ext })
        })
        .collect();
    Ok(ids.iter().filter_map(|id| found.get(id).cloned()).collect())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind database::tests::rehydrate`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/database.rs
git commit -m "feat(core): add rehydrate_rows for ordered id-list session restore"
```

---

## Task 5: `UiState` struct + serde round-trip (pure, core crate)

**Files:**
- Create: `src/ui_state.rs`
- Modify: `src/lib.rs` (`pub mod ui_state;`)
- Test: in `src/ui_state.rs`

**Interfaces:**
- Consumes: `Sort`, `Filters`.
- Produces: `PersistedMode::{Browse, Text(String), Similar(i64)}`, `UiState{ search_text, mode, sort, filters, result_ids:Vec<i64>, selected_index:Option<usize>, detail_open:bool, scroll_y:f32 }`, all serde. `UiState::default()` = Browse, empty, `Sort::default()`, no selection, closed, scroll 0.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::{Sort, SortKey, SortDir};

    #[test]
    fn round_trips_through_json() {
        let st = UiState {
            search_text: "cat".into(),
            mode: PersistedMode::Text("cat".into()),
            sort: Sort { key: SortKey::Size, dir: SortDir::Desc },
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
```

(Ensure `UiState`, `PersistedMode`, `Filters`, `Sort` derive `PartialEq`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind ui_state::`
Expected: FAIL — module not found.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Persisted GUI session state (single row in the `ui_state` table).
use serde::{Deserialize, Serialize};
use crate::filters::Filters;
use crate::sort::Sort;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum PersistedMode {
    Browse,
    Text(String),
    Similar(i64), // seed image id
}

impl Default for PersistedMode {
    fn default() -> Self { PersistedMode::Browse }
}

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
```

Add to `src/lib.rs`: `pub mod ui_state;`. Ensure `Filters` derives `Default + PartialEq + Serialize + Deserialize` (check `src/filters.rs`; add derives if missing — they likely already serialize for the GUI).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind ui_state::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ui_state.rs src/lib.rs src/filters.rs
git commit -m "feat(core): add UiState/PersistedMode session struct (serde)"
```

---

## Task 6: Migration 003 + `get_ui_state`/`set_ui_state` (DB)

**Files:**
- Modify: `src/database.rs` (bump `LATEST_MIGRATION_VERSION` to 3; add `migration_003_ui_state`; wire into `run_migrations`; add methods)
- Test: `src/database.rs`

**Interfaces:**
- Consumes: `UiState`.
- Produces: `pub fn get_ui_state(&self) -> Result<Option<UiState>>` (malformed/old JSON → `Ok(None)` after a `tracing::warn!`), `pub fn set_ui_state(&self, state: &UiState) -> Result<()>` (UPSERT on `id=1`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn ui_state_round_trips_through_db() {
    let (db, _tmp) = test_db_with_rows(&[("a.jpg", Some(1))]);
    assert!(db.get_ui_state().unwrap().is_none());
    let mut st = UiState::default();
    st.search_text = "dog".into();
    st.result_ids = vec![1];
    st.selected_index = Some(0);
    db.set_ui_state(&st).unwrap();
    assert_eq!(db.get_ui_state().unwrap().unwrap(), st);
    // upsert overwrites the single row
    st.search_text = "cat".into();
    db.set_ui_state(&st).unwrap();
    assert_eq!(db.get_ui_state().unwrap().unwrap().search_text, "cat");
}

#[test]
fn malformed_ui_state_is_none() {
    let (db, _tmp) = test_db_with_rows(&[("a.jpg", Some(1))]);
    let conn = db.pool.get().unwrap();
    conn.execute("INSERT INTO ui_state (id, state_json) VALUES (1, '{not json')", []).unwrap();
    drop(conn);
    assert!(db.get_ui_state().unwrap().is_none());
}
```

(If `pool` is private, add the malformed row via a small test-only helper or skip the direct insert and assert the happy path only — but prefer exposing enough for the malformed test.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind database::tests::ui_state`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

In `run_migrations`, after the `current < 2` block and before the version-stamp:

```rust
if current < 3 {
    migration_003_ui_state(conn).context("migration 3 (ui_state)")?;
}
```

Bump `const LATEST_MIGRATION_VERSION: i32 = 3;`. Add:

```rust
/// Migration 3: single-row persisted GUI session state (JSON blob).
fn migration_003_ui_state(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            state_json TEXT NOT NULL,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );",
    )?;
    Ok(())
}
```

Methods on `impl Database`:

```rust
pub fn get_ui_state(&self) -> Result<Option<UiState>> {
    let conn = self.pool.get().context("DB connection for get_ui_state")?;
    let json: Option<String> = conn
        .query_row("SELECT state_json FROM ui_state WHERE id = 1", [], |r| r.get(0))
        .optional()?;
    match json {
        None => Ok(None),
        Some(s) => match serde_json::from_str::<UiState>(&s) {
            Ok(st) => Ok(Some(st)),
            Err(e) => {
                tracing::warn!("discarding unreadable ui_state: {e}");
                Ok(None)
            }
        },
    }
}

pub fn set_ui_state(&self, state: &UiState) -> Result<()> {
    let json = serde_json::to_string(state).context("serialize ui_state")?;
    let conn = self.pool.get().context("DB connection for set_ui_state")?;
    conn.execute(
        "INSERT INTO ui_state (id, state_json, updated_at)
         VALUES (1, ?1, CURRENT_TIMESTAMP)
         ON CONFLICT(id) DO UPDATE SET state_json = ?1, updated_at = CURRENT_TIMESTAMP",
        params![json],
    )?;
    Ok(())
}
```

Ensure `use rusqlite::OptionalExtension;` (for `.optional()`) and `serde_json` is a core-crate dependency (add to `Cargo.toml` if absent).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind database::tests::ui_state`
Expected: PASS. Also run `cargo test -p imgfind` to confirm existing migration tests still pass at version 3.

- [ ] **Step 5: Commit**

```bash
git add src/database.rs Cargo.toml
git commit -m "feat(core): migration 003 ui_state + get/set_ui_state"
```

---

## Task 7: `GuiConfig` + `[gui]` config section

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `SortKey`, `SortDir`.
- Produces: `GuiConfig{ preload_neighbors:usize, default_sort:SortKey, default_sort_direction:SortDir }`, added as `#[serde(default)] pub gui: GuiConfig` on `Config`. `GuiConfig::default()` = `{2, Name, Asc}`. Helper `GuiConfig::default_sort(&self) -> Sort`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn missing_gui_section_uses_defaults() {
    let cfg: Config = toml::from_str("ignore_patterns = []").unwrap();
    assert_eq!(cfg.gui.preload_neighbors, 2);
    assert_eq!(cfg.gui.default_sort, crate::sort::SortKey::Name);
    assert_eq!(cfg.gui.default_sort_direction, crate::sort::SortDir::Asc);
}

#[test]
fn explicit_gui_values_parse() {
    let cfg: Config = toml::from_str(
        "[gui]\npreload_neighbors = 5\ndefault_sort = \"size\"\ndefault_sort_direction = \"desc\"\n"
    ).unwrap();
    assert_eq!(cfg.gui.preload_neighbors, 5);
    assert_eq!(cfg.gui.default_sort, crate::sort::SortKey::Size);
    assert_eq!(cfg.gui.default_sort_direction, crate::sort::SortDir::Desc);
    assert_eq!(cfg.gui.default_sort(), crate::sort::Sort {
        key: crate::sort::SortKey::Size, dir: crate::sort::SortDir::Desc });
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind config::`
Expected: FAIL — `gui` field absent.

- [ ] **Step 3: Write minimal implementation**

```rust
use crate::sort::{Sort, SortDir, SortKey};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    #[serde(default = "default_preload_neighbors")]
    pub preload_neighbors: usize,
    #[serde(default = "default_gui_sort")]
    pub default_sort: SortKey,
    #[serde(default = "default_gui_sort_dir")]
    pub default_sort_direction: SortDir,
}

fn default_preload_neighbors() -> usize { 2 }
fn default_gui_sort() -> SortKey { SortKey::Name }
fn default_gui_sort_dir() -> SortDir { SortDir::Asc }

impl Default for GuiConfig {
    fn default() -> Self {
        GuiConfig {
            preload_neighbors: default_preload_neighbors(),
            default_sort: default_gui_sort(),
            default_sort_direction: default_gui_sort_dir(),
        }
    }
}

impl GuiConfig {
    pub fn default_sort(&self) -> Sort {
        Sort { key: self.default_sort, dir: self.default_sort_direction }
    }
}
```

Add to `Config`: `#[serde(default)] pub gui: GuiConfig,` and to its `Default` impl `gui: GuiConfig::default(),`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind config::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(core): add [gui] config (preload_neighbors, default sort)"
```

---

## Task 8: Thumbnail sizes (`LIGHTBOX_SIZE`, `GUI_THUMBNAIL_SIZES`) + persistence test

**Files:**
- Modify: `src/thumbnail.rs`
- Test: `src/thumbnail.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub const LIGHTBOX_SIZE: u32 = 2048;`, `pub const GUI_THUMBNAIL_SIZES: &[u32] = &[300, 512, 2048];`.

- [ ] **Step 1: Write the failing test**

A test that calling `get_or_generate_thumbnail` at each GUI size lands a `(hash,size)` row. Use the existing thumbnail test harness (there are tests in `backend.rs`/`thumbnail.rs` already — mirror their fixture image setup).

```rust
#[test]
fn gui_sizes_are_300_512_2048() {
    assert_eq!(GUI_THUMBNAIL_SIZES, &[300, 512, 2048]);
    assert_eq!(LIGHTBOX_SIZE, 2048);
}

#[test]
fn get_or_generate_persists_each_gui_size() {
    let (db, _tmp, rel_path, hash) = test_thumbnail_fixture(); // builds DB + one indexed image
    for &size in GUI_THUMBNAIL_SIZES {
        assert!(db.get_thumbnail(&hash, size).unwrap().is_none(), "size {size} should start absent");
        let _ = get_or_generate_thumbnail(&db, &rel_path, size).unwrap();
        assert!(db.get_thumbnail(&hash, size).unwrap().is_some(), "size {size} must persist");
    }
}
```

If a fixture helper doesn't exist, build one from an embedded small test image (reuse whatever the current thumbnail tests use; grep for `get_or_generate_thumbnail` test usage).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind thumbnail::`
Expected: FAIL — consts not found.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Long-edge target for the GUI lightbox/preview cached render.
pub const LIGHTBOX_SIZE: u32 = 2048;

/// Thumbnail sizes the GUI requests: grid (300), detail panel (512),
/// lightbox/preview (2048). Pre-generating these avoids first-view decoding.
pub const GUI_THUMBNAIL_SIZES: &[u32] = &[300, 512, LIGHTBOX_SIZE];
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind thumbnail::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/thumbnail.rs
git commit -m "feat(core): define LIGHTBOX_SIZE + GUI_THUMBNAIL_SIZES; pin persistence"
```

---

## Task 9: `imgfind thumbnails` multi-size + help text

**Files:**
- Modify: `src/main.rs` (the `Thumbnails` command variant, its dispatch, and `generate_thumbnails_batch`)
- Test: `src/main.rs` (a pure `resolve_thumbnail_sizes` helper) `#[cfg(test)]`

**Interfaces:**
- Produces: `fn resolve_thumbnail_sizes(sizes: &[u32], gui_sizes: bool) -> Vec<u32>` — returns `GUI_THUMBNAIL_SIZES` when `gui_sizes`; else the given `sizes`; else `[300]` default. De-duplicates, preserves order.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn resolve_sizes_default_is_300() {
    assert_eq!(resolve_thumbnail_sizes(&[], false), vec![300]);
}
#[test]
fn resolve_sizes_gui_flag_expands() {
    assert_eq!(resolve_thumbnail_sizes(&[], true), vec![300, 512, 2048]);
}
#[test]
fn resolve_sizes_explicit_dedup_preserves_order() {
    assert_eq!(resolve_thumbnail_sizes(&[512, 300, 512], false), vec![512, 300]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind resolve_thumbnail_sizes`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

Update the `Thumbnails` command (use clap `num_args`/multiple for `--size`, add `--gui-sizes`):

```rust
Thumbnails {
    /// Thumbnail size(s) to generate. Repeat for multiple, e.g. -s 300 -s 512.
    /// The GUI uses 300 (grid), 512 (detail panel), and 2048 (lightbox/preview);
    /// pass --gui-sizes to pre-generate exactly those.
    #[arg(short, long)]
    size: Vec<u32>,
    /// Generate the full set of sizes the GUI uses (300, 512, 2048).
    #[arg(long)]
    gui_sizes: bool,
    /// Number of thumbnails to generate per size in this batch.
    #[arg(short, long, default_value_t = 50)]
    count: usize,
},
```

Helper + dispatch:

```rust
fn resolve_thumbnail_sizes(sizes: &[u32], gui_sizes: bool) -> Vec<u32> {
    let raw: Vec<u32> = if gui_sizes {
        imgfind::thumbnail::GUI_THUMBNAIL_SIZES.to_vec()
    } else if sizes.is_empty() {
        vec![300]
    } else {
        sizes.to_vec()
    };
    let mut seen = std::collections::HashSet::new();
    raw.into_iter().filter(|s| seen.insert(*s)).collect()
}
```

In dispatch, loop `for size in resolve_thumbnail_sizes(&size, gui_sizes)` calling the existing batch generator per size, printing per-size progress.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind resolve_thumbnail_sizes` then `cargo build -p imgfind`.
Expected: PASS + builds. Smoke: `cargo run -p imgfind -- thumbnails --help` shows the new help.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): imgfind thumbnails accepts multiple sizes + --gui-sizes; document GUI sizes"
```

---

## Task 10: `preload_arc` ordering helper (pure, GUI crate)

**Files:**
- Create: `imgfind-gui/src/preload.rs`
- Modify: `imgfind-gui/src/main.rs` (add `mod preload;`)
- Test: in `imgfind-gui/src/preload.rs`

**Interfaces:**
- Produces: `pub fn preload_arc(i: usize, n: usize, len: usize) -> Vec<usize>` — focus first, then `i+1,i-1,i+2,i-2,…` up to distance `n`, clamped to `[0,len)`, de-duplicated.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arc_middle() {
        assert_eq!(preload_arc(5, 2, 100), vec![5, 6, 4, 7, 3]);
    }
    #[test]
    fn arc_near_start_clamps_and_dedups() {
        assert_eq!(preload_arc(0, 2, 100), vec![0, 1, 2]);
    }
    #[test]
    fn arc_near_end_clamps() {
        assert_eq!(preload_arc(99, 2, 100), vec![99, 98, 97]);
    }
    #[test]
    fn arc_zero_neighbors() {
        assert_eq!(preload_arc(5, 0, 100), vec![5]);
    }
    #[test]
    fn arc_empty_or_single() {
        assert_eq!(preload_arc(0, 2, 0), Vec::<usize>::new());
        assert_eq!(preload_arc(0, 2, 1), vec![0]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind-gui preload::`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Neighbor preload ordering: focus first, then outward in an increasing arc.
pub fn preload_arc(i: usize, n: usize, len: usize) -> Vec<usize> {
    if len == 0 || i >= len { return Vec::new(); }
    let mut out = vec![i];
    let mut seen = std::collections::HashSet::from([i]);
    for d in 1..=n {
        for cand in [i.checked_add(d), i.checked_sub(d)].into_iter().flatten() {
            if cand < len && seen.insert(cand) {
                out.push(cand);
            }
        }
    }
    out
}
```

Add `mod preload;` to `imgfind-gui/src/main.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind-gui preload::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/preload.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): add preload_arc neighbor-ordering helper"
```

---

## Task 11: Window virtualization math (pure, GUI crate)

**Files:**
- Create: `imgfind-gui/src/window.rs`
- Modify: `imgfind-gui/src/main.rs` (`mod window;`)
- Test: in `imgfind-gui/src/window.rs`

**Interfaces:**
- Produces:
  - `pub const TILE_PITCH_Y: f32 = 208.0;` (200px tile + 8px gap — confirm against `app.slint` tile size; adjust constant to match the actual layout).
  - `pub const SLIDE_TRIGGER_ROWS: usize = 4;`
  - `pub const WINDOW_MIN: usize = 200; pub const WINDOW_MAX: usize = 2000;`
  - `pub fn visible_rows(scroll_y: f32, viewport_h: f32, pitch_y: f32) -> (usize, usize)` → `(first_row, last_row_exclusive)`.
  - `pub fn window_range(first_row: usize, last_row: usize, cols: usize, total: usize) -> std::ops::Range<usize>` → item-index range to render (buffered by `SLIDE_TRIGGER_ROWS` rows, clamped, size-bounded to `[WINDOW_MIN, WINDOW_MAX]`).
  - `pub fn need_slide(current: &std::ops::Range<usize>, visible_first_idx: usize, visible_last_idx: usize, cols: usize) -> bool`.
  - `pub fn total_rows(total_items: usize, cols: usize) -> usize` and `pub fn viewport_height(total_items: usize, cols: usize, pitch_y: f32) -> f32`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_rows_from_scroll() {
        // pitch 200, scrolled 400px, viewport 800px -> rows 2..=6 (7 exclusive)
        let (f, l) = visible_rows(400.0, 800.0, 200.0);
        assert_eq!(f, 2);
        assert_eq!(l, 7); // ceil(800/200)+1 = 5 rows beyond first -> 2..7
    }

    #[test]
    fn visible_rows_top_clamps_to_zero() {
        let (f, _) = visible_rows(-10.0, 800.0, 200.0);
        assert_eq!(f, 0);
    }

    #[test]
    fn window_range_clamps_and_buffers() {
        // cols=4, total=1000, visible rows 10..15 -> buffered by 4 rows each side
        let r = window_range(10, 15, 4, 1000);
        assert!(r.start <= (10 - 4) * 4);
        assert!(r.end >= 15 * 4);
        assert!(r.end <= 1000);
        assert!(r.len() <= WINDOW_MAX);
    }

    #[test]
    fn window_range_total_smaller_than_window() {
        let r = window_range(0, 3, 4, 10);
        assert_eq!(r, 0..10);
    }

    #[test]
    fn total_rows_rounds_up() {
        assert_eq!(total_rows(10, 4), 3);
        assert_eq!(total_rows(0, 4), 0);
        assert_eq!(total_rows(8, 4), 2);
    }

    #[test]
    fn need_slide_true_when_entering_buffer() {
        let cur = 0..200usize; // current window
        // visible near the far edge of the window triggers a slide
        assert!(need_slide(&cur, 190 / 1, 199, 4) || need_slide(&cur, 0, 4, 4) == false);
    }
}
```

> Implementer: tighten the `need_slide` test to your final semantics; the intent is "slide when the visible index range comes within `SLIDE_TRIGGER_ROWS*cols` of either end of `current`, and `current` isn't already at a clamp boundary."

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind-gui window::`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Pure virtualization math for the moving-window grid (Approach C).
use std::ops::Range;

pub const TILE_PITCH_Y: f32 = 208.0; // adjust to match app.slint tile + gap
pub const SLIDE_TRIGGER_ROWS: usize = 4;
pub const WINDOW_MIN: usize = 200;
pub const WINDOW_MAX: usize = 2000;

pub fn total_rows(total_items: usize, cols: usize) -> usize {
    if cols == 0 { return 0; }
    total_items.div_ceil(cols)
}

pub fn viewport_height(total_items: usize, cols: usize, pitch_y: f32) -> f32 {
    total_rows(total_items, cols) as f32 * pitch_y
}

/// (first_row, last_row_exclusive) of rows intersecting the viewport.
pub fn visible_rows(scroll_y: f32, viewport_h: f32, pitch_y: f32) -> (usize, usize) {
    let top = scroll_y.max(0.0);
    let first = (top / pitch_y).floor() as usize;
    let count = (viewport_h / pitch_y).ceil() as usize + 1;
    (first, first + count)
}

/// Item-index range to render: visible rows expanded by SLIDE_TRIGGER_ROWS on
/// each side, clamped to [0,total) and bounded to [WINDOW_MIN, WINDOW_MAX] items.
pub fn window_range(first_row: usize, last_row: usize, cols: usize, total: usize) -> Range<usize> {
    if cols == 0 || total == 0 { return 0..0; }
    let buf = SLIDE_TRIGGER_ROWS;
    let start_row = first_row.saturating_sub(buf);
    let end_row = last_row + buf;
    let mut start = (start_row * cols).min(total);
    let mut end = (end_row * cols).min(total);
    // enforce minimum window size where possible
    if end - start < WINDOW_MIN {
        end = (start + WINDOW_MIN).min(total);
        start = end.saturating_sub(WINDOW_MAX).min(start);
    }
    if end - start > WINDOW_MAX {
        end = start + WINDOW_MAX;
    }
    start..end
}

pub fn need_slide(current: &Range<usize>, visible_first_idx: usize,
                  visible_last_idx: usize, cols: usize) -> bool {
    let buf = SLIDE_TRIGGER_ROWS * cols;
    let near_start = visible_first_idx < current.start + buf && current.start > 0;
    let near_end = visible_last_idx + buf > current.end && current.end != 0;
    near_start || near_end
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind-gui window::`
Expected: PASS (after the implementer finalizes `need_slide` semantics + its test).

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/window.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): add pure window virtualization math"
```

---

## Task 12: Backend + state: row-meta browse/search and result model

**Files:**
- Modify: `imgfind-gui/src/backend.rs` (add `browse_all`/`search_meta`/`rehydrate` passthroughs returning `RowMeta`)
- Modify: `imgfind-gui/src/state.rs` (switch `SearchState.results` to `Vec<RowMeta>`; add `sort: Sort`, `mode`; keep the state machine pure)
- Test: `imgfind-gui/src/state.rs` (update existing tests; add a sort-mode test)

**Interfaces:**
- Consumes: `imgfind::sort::{RowMeta, Sort}`, `imgfind::database::Database`.
- Produces (backend): `pub fn browse_all(&self, f:&Filters, sort:&Sort) -> Result<Vec<RowMeta>>`, `pub fn search_meta(&self, query:&str, ...) -> Result<Vec<RowMeta>>` (wraps `search_similar_images_meta`, joining size/ext), `pub fn rehydrate(&self, ids:&[i64]) -> Result<Vec<RowMeta>>`.
- Produces (state): `SearchState.results: Vec<RowMeta>`, `SearchState.sort: Sort`. The old `SearchResult{path,distance,file_size}` is removed in favor of `RowMeta`; relevance order is preserved by the order of the `results` Vec.

> **Note:** This task removes paging. `PAGE_SIZE`, `apply_page`, `has_more`, `next_offset`, "Load more" disappear from `state.rs`. The state now holds the full result list (set once per query/browse). Update or delete the page-oriented tests; add coverage that `apply_results(rows)` replaces the list and that `set_sort` + a re-sort path keeps `results` consistent (for search mode, call `imgfind::sort::sort_rows`).

- [ ] **Step 1: Write the failing test** (in `state.rs`)

```rust
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
    s.resort(&Sort { key: SortKey::Name, dir: SortDir::Asc });
    assert_eq!(s.results.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 1]);
}
```

(`rm` is a small `RowMeta` constructor helper in the test module.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind-gui state::`
Expected: FAIL (methods/types changed).

- [ ] **Step 3: Write minimal implementation**

- In `state.rs`: change `results` to `Vec<RowMeta>`, add `pub sort: Sort`. Replace `apply_page` with `apply_results(&mut self, rows: Vec<RowMeta>)` (sets `results`, clears `loading`/`error`). Add `resort(&mut self, sort:&Sort)` calling `imgfind::sort::sort_rows(&mut self.results, sort)` and storing `self.sort = *sort`. Remove `PAGE_SIZE`/`has_more`/`next_offset`/`apply_page` (and update `ViewState` derivation: `Empty` when `results.is_empty()` after a search).
- In `backend.rs`: add the three passthrough methods delegating to the new `Database` methods. For `search_meta`, adapt `search_similar_images_meta` output into `RowMeta` (it already yields path + distance + size per the map; derive `ext` from path; `id` from the row id — extend the query if id isn't currently returned).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p imgfind-gui state::` then `cargo build -p imgfind-gui`.
Expected: PASS (state tests) — build will still fail until `main.rs`/`app.slint` are updated in later tasks; that's acceptable for this task's commit **only if** the workspace still compiles. If `main.rs` references removed methods, this task must also stub/adjust those call sites minimally to keep the crate compiling. Prefer updating `main.rs` call sites here so the crate builds.

> **Right-sizing note:** keep this task's `main.rs` edits to the minimum needed to compile (swap `apply_page`→`apply_results`, drop Load-more references). The full grid rewrite is Task 13.

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/state.rs imgfind-gui/src/backend.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): result model uses RowMeta; backend browse_all/search_meta/rehydrate"
```

---

## Task 13: Virtualized `Flickable` grid markup + remove Load-more (Slint)

**Files:**
- Modify: `imgfind-gui/ui/app.slint`
- Modify: `imgfind-gui/src/main.rs` (expose `grid-viewport-y` (out), `grid-cols` (out/computed), `grid-viewport-height` (in), tile `absolute-row`/`absolute-col`; remove `show_load_more`/`set_show_load_more` and the Load-more button callback)

**Interfaces:**
- Produces (Slint properties): `in property <length> grid-viewport-height;`, `out property <length> grid-viewport-y;`, `out property <int> grid-cols;`, and `Tile` gains `absolute-row:int`, `absolute-col:int`. The grid is a `Flickable` with `viewport-height: root.grid-viewport-height` and tiles positioned at `x: absolute-col * <pitch_x>`, `y: absolute-row * <pitch_y>`.

This is GUI wiring — **no unit test**; verified by build + manual smoke.

- [ ] **Step 1: Edit `app.slint`** — wrap the tile grid in a `Flickable`; bind `viewport-height`; expose `viewport-y` and computed `cols` (from pane width / tile pitch); position each tile absolutely by `absolute-row/col`. Consult the Slint skill for `Flickable` + focus interactions (keep the existing grid `FocusScope` as parent so keyboard still works). Remove the "Load more" button block.

- [ ] **Step 2: Edit `main.rs`** — delete `set_show_load_more` calls and the Load-more callback; add setters/getters for the new properties; compute `grid-viewport-height` via `imgfind_gui::window::viewport_height(total, cols, TILE_PITCH_Y)` when results change.

- [ ] **Step 3: Build**

Run: `cargo build -p imgfind-gui`
Expected: compiles. (Grid won't be fully wired until Task 14.)

- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): virtualized Flickable grid markup; remove Load-more"
```

---

## Task 14: Windowed thumbnail loader + LRU eviction + sync timer

**Files:**
- Modify: `imgfind-gui/src/main.rs` (replace `spawn_search`'s page-fetch + `build_tiles_model` with a windowed loader driven by a ~100ms timer reading `grid-viewport-y`/`grid-cols`)

**Interfaces:**
- Consumes: `window::{visible_rows, window_range, need_slide, TILE_PITCH_Y}`, `backend.thumbnail(path, 300)`, generation guard pattern (existing `Arc<AtomicU64>`).
- Behavior: on each tick (or on results/scroll change), compute the window via `window_range`; build the `tiles` model for that index range (each `Tile` carries `absolute-row/col`); request missing 300px thumbnails on worker thread(s) (persisting via `get_or_generate_thumbnail`), feeding a bounded `LruCache<image_id|path, slint::Image>` (capacity e.g. 256); evict outside the window; push image updates via `invoke_from_event_loop`; drop stale results by generation/epoch.

GUI wiring — **no unit test**; build + manual smoke. (The math it relies on is unit-tested in Task 11.)

- [ ] **Step 1:** Add an `lru`-backed image cache (add `lru` to `imgfind-gui/Cargo.toml` if not present, or a simple bounded `HashMap` + insertion-order eviction). Implement `refresh_window()` that reads viewport-y/cols, computes the window, builds tiles, and dispatches missing-thumb decode requests.
- [ ] **Step 2:** Wire a `slint::Timer` (~100ms repeating) to call `refresh_window()`; also call it when `results`/sort/filters change. Use `need_slide` to avoid rebuilding the model when the window hasn't moved.
- [ ] **Step 3: Build + smoke**

Run: `cargo build -p imgfind-gui` then `cargo run -p imgfind-gui -- --dir <a-test-db-dir>` and scroll: tiles should populate progressively, memory stays flat over a large set.
Expected: smooth scrolling; no panics in logs (`RUST_LOG=imgfind_gui=debug`).

- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/src/main.rs imgfind-gui/Cargo.toml
git commit -m "feat(gui): moving-window thumbnail loader with LRU + 100ms sync"
```

---

## Task 15: Keyboard nav + scroll-into-view for the virtual grid

**Files:**
- Modify: `imgfind-gui/src/nav.rs` (if selection math needs total/cols awareness), `imgfind-gui/src/main.rs`, `imgfind-gui/ui/app.slint`

**Interfaces:**
- Behavior: `move_selection` still maps over the full `results` indices (h/j/k/l + arrows, clamp at global first/last, no wrap). When selection changes, set `grid-viewport-y` so the selected tile's row is visible (compute target scroll: if selected row < first visible row → scroll up to it; if > last → scroll down so it's the last visible row; else leave). Enter opens detail, Space opens lightbox, Esc closes panel keeping selection (unchanged semantics).

GUI wiring — keep any new pure math (e.g. `scroll_to_reveal(sel_row, first_visible_row, visible_row_count) -> Option<f32>`) in `window.rs` **with a unit test**; the Slint wiring itself is build + smoke.

- [ ] **Step 1: (test) add `scroll_to_reveal` to `window.rs`**

```rust
/// Target scroll_y (px) to reveal `sel_row`, or None if already visible.
pub fn scroll_to_reveal(sel_row: usize, first_visible_row: usize,
                        visible_rows: usize, pitch_y: f32) -> Option<f32> {
    if sel_row < first_visible_row {
        Some(sel_row as f32 * pitch_y)
    } else if sel_row >= first_visible_row + visible_rows {
        Some((sel_row + 1).saturating_sub(visible_rows) as f32 * pitch_y)
    } else {
        None
    }
}
```

Test top/bottom/already-visible cases. Run `cargo test -p imgfind-gui window::scroll_to_reveal` (fail→implement→pass).

- [ ] **Step 2:** Wire `scroll_to_reveal` into the selection-change handler in `main.rs`, setting `grid-viewport-y`.
- [ ] **Step 3: Build + smoke** — `cargo build -p imgfind-gui`; arrow/vim keys move selection and keep it on-screen.
- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/src/window.rs imgfind-gui/src/nav.rs imgfind-gui/src/main.rs imgfind-gui/ui/app.slint
git commit -m "feat(gui): virtual-grid keyboard nav + scroll-into-view"
```

---

## Task 16: Sort selector UI + wiring

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (sort dropdown + direction toggle), `imgfind-gui/src/main.rs` (callbacks)

**Interfaces:**
- Slint: `in-out property <int> sort-key;` (0=Name,1=Size,2=Type,3=Relevance), `in-out property <bool> sort-desc;`, `in property <bool> relevance-available;` (true while a query is active), callbacks `sort-changed()`, `sort-dir-toggled()`. Direction toggle label uses ASCII (e.g. text `"v"` when desc, `"^"` when asc).
- Behavior: on change — browse mode: re-run `backend.browse_all(filters, sort)` and rebuild window; search mode: `state.resort(&sort)` (Relevance restores the retained relevance order — keep the original relevance-ordered `Vec` so Relevance is reproducible; store it alongside, or re-fetch via `search_meta`). Default selection: Relevance while searching, else the config default sort.

GUI wiring — build + smoke (the ordering logic is already unit-tested in Tasks 1–3, 12).

- [ ] **Step 1:** Add the sort control to `app.slint` beside the filter bar (ASCII glyphs only).
- [ ] **Step 2:** Implement callbacks in `main.rs` mapping the int sort-key + bool dir → `imgfind::sort::Sort`; branch browse vs search.
- [ ] **Step 3:** To make `Relevance` reproducible in search mode, retain the relevance-ordered result list (e.g. `relevance_results: Vec<RowMeta>` in state) so switching back to Relevance restores it without re-querying.
- [ ] **Step 4: Build + smoke** — `cargo build -p imgfind-gui`; toggling sort reorders the grid; direction button flips order.
- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs imgfind-gui/src/state.rs
git commit -m "feat(gui): sort selector (name/size/type/relevance) + direction toggle"
```

---

## Task 17: Lightbox uses cached `LIGHTBOX_SIZE`

**Files:**
- Modify: `imgfind-gui/src/main.rs` (`load_lightbox_image`)

**Interfaces:**
- Behavior: replace `decode_full_image` with `backend.thumbnail(path, imgfind::thumbnail::LIGHTBOX_SIZE)` (persisting), decoded on a background thread, generation-guarded as today. The detail panel keeps 512.

GUI wiring — build + smoke.

- [ ] **Step 1:** Swap the lightbox decode path to the cached 2048 size.
- [ ] **Step 2: Build + smoke** — open lightbox: first open decodes+caches, subsequent opens are instant; RAW files no longer re-demosaic on every open.
- [ ] **Step 3: Commit**

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): lightbox renders + caches the 2048px size"
```

---

## Task 18: Neighbor preloading for lightbox + detail panel

**Files:**
- Modify: `imgfind-gui/src/main.rs` (lightbox open/navigate + detail open handlers)

**Interfaces:**
- Consumes: `preload::preload_arc`, `GuiConfig::preload_neighbors`, `backend.thumbnail`.
- Behavior: on open/navigate at index `i`, load focus first, then `for idx in preload_arc(i, n, results.len())` spawn background decodes at the surface size (2048 lightbox / 512 detail) via the persisting thumbnail path. Generation-guarded so navigation cancels stale loads. `n` from config (thread the loaded `GuiConfig` into the GUI state at startup — Task 19).

GUI wiring — build + smoke (arc order unit-tested in Task 10).

- [ ] **Step 1:** Implement preload dispatch in the lightbox nav handler and the detail-open handler.
- [ ] **Step 2: Build + smoke** — `RUST_LOG=imgfind_gui=debug`; navigating the lightbox shows neighbor loads firing focus-first then outward; next/prev is instant within the preloaded band.
- [ ] **Step 3: Commit**

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): preload n neighbors (arc order) for lightbox + detail"
```

---

## Task 19: Browse-all on startup with configurable default sort

**Files:**
- Modify: `imgfind-gui/src/main.rs` (startup wiring; load `Config`/`GuiConfig`; thread into state)

**Interfaces:**
- Behavior: at startup (after model/DB ready), if no persisted `UiState` (Task 20 handles the restore branch), browse-all via `backend.browse_all(&Filters::default(), &gui_config.default_sort())`, populate the window, select the first item, grab grid focus. Load `Config` once and store `GuiConfig` (incl. `preload_neighbors`) in a state holder for Task 18.

GUI wiring — build + smoke.

- [ ] **Step 1:** Load `Config::load()` early; keep `GuiConfig` in an `Arc` state holder.
- [ ] **Step 2:** On ready, perform the default browse-all + first-item selection.
- [ ] **Step 3: Build + smoke** — launching against a DB shows all images sorted name-asc by default; changing `~/.imgfind/config.toml` `[gui] default_sort` changes the startup order.
- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): browse-all on startup using config default sort"
```

---

## Task 20: Session restore on startup + persist on exit

**Files:**
- Modify: `imgfind-gui/src/main.rs` (startup restore branch; persist after `run()`)

**Interfaces:**
- Consumes: `Database::{get_ui_state,set_ui_state,rehydrate_rows}`, `UiState`, `PersistedMode`.
- Startup: if `get_ui_state()` is `Some(st)`: `rehydrate_rows(&st.result_ids)` → load into the grid; restore `search_text` into the box, `sort`/`filters` into controls, detail-panel open/seed, `selected_index` (clamped to rehydrated len), and `grid-viewport-y = st.scroll_y`. **No query runs.** If `None`, fall through to Task 19's default browse-all.
- Exit: after `app.run()` returns, assemble a `UiState` from the captured state holders (results→`result_ids` via each row's `id`, current `selected`, `detail` open + seed id, `sort`, `filters`, `search_text`, and the last `grid-viewport-y`) and call `db.set_ui_state(&st)`.

GUI wiring — build + smoke (DB methods unit-tested in Task 6; `UiState` serde in Task 5).

- [ ] **Step 1:** Implement the restore branch at startup (before/instead of default browse-all when state exists).
- [ ] **Step 2:** Capture state holders (clone `Arc`s) and the window weak handle; after `run()` build + persist `UiState`. Read the final `grid-viewport-y` from the window before it drops, or keep a mirrored `Arc<Mutex<f32>>` updated by the sync timer.
- [ ] **Step 3: Build + smoke** — set up a session (search text, a sort, select a tile, open detail, scroll), quit, relaunch against the same DB: the exact session is restored (thumbnails stream in). Verify a DB with deleted images since last run drops them and re-clamps selection without panic.
- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): restore session on startup, persist UiState on exit"
```

---

## Task 21: Documentation updates

**Files:**
- Modify: `CLAUDE.md`, `USAGE.md`

GUI wiring/docs — no test.

- [ ] **Step 1:** Update `CLAUDE.md`:
  - GUI section: grid is now virtualized moving-window infinite scroll (Approach C — window over a fully-loaded id list), not paged Load-more; sort selector (name/size/type + relevance, direction toggle); lightbox renders+caches the 2048px size; neighbor preload (config `preload_neighbors`, arc order); session persistence in `ui_state` (restored on launch, saved on exit).
  - Config section: document `[gui]` keys (`preload_neighbors`, `default_sort`, `default_sort_direction`).
  - Storage section: correct the stale "no migrations" note — there *is* a `PRAGMA user_version` migration runner; record migration 003 (`ui_state`). Note `LIGHTBOX_SIZE`/`GUI_THUMBNAIL_SIZES`.
  - Reference the spec: `docs/superpowers/specs/2026-06-19-gui-virtualized-scroll-sort-state-design.md`.
- [ ] **Step 2:** Update `USAGE.md`: `imgfind thumbnails` new `--size` (repeatable) / `--gui-sizes` flags + GUI sizes; `[gui]` config section.
- [ ] **Step 3:** Build docs sanity (no build) and commit.

```bash
git add CLAUDE.md USAGE.md
git commit -m "docs: virtualized grid, sort, thumbnail sizes, [gui] config, session state"
```

---

## Task 22: Final review + finish branch

- [ ] Dispatch a final code-reviewer over the whole branch diff (spec compliance + quality).
- [ ] Run full `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, and `cargo build --release --workspace`.
- [ ] Manual smoke of the GUI per the spec's acceptance points (scroll, sort, lightbox cache, preload, restore).
- [ ] Invoke `superpowers:finishing-a-development-branch`.

---

## Self-Review

**Spec coverage:**
- Infinite scroll / moving window → Tasks 11, 13, 14, 15. ✅
- Sort selector (type/size/name, name tiebreaker, direction) → Tasks 1, 2, 3, 12, 16. ✅
- Thumbnails stored whenever used + `imgfind thumbnails` help/sizes → Tasks 8, 9, 17 (lightbox now persists). ✅
- Preload n neighbors, config default 2, focus-first arc → Tasks 7, 10, 18. ✅
- Browse-all on startup + configurable default sort → Tasks 7, 19. ✅
- Session persistence in `.imgfind` DB, restore-by-id (no re-query), on exit → Tasks 5, 6, 20. ✅
- Lightbox large cached size → Tasks 8, 17. ✅
- TDD seams → unit/DB tests in Tasks 1–12, 15 (scroll_to_reveal), 11 (window math); GUI wiring explicitly build+smoke. ✅
- Docs → Task 21. ✅

**Placeholder scan:** GUI-wiring tasks (13–20) intentionally lack unit-test code because Slint UI isn't reliably testable (per spec); each still names exact files, interfaces, and build/smoke verification, and pushes every extractable bit of logic into unit-tested pure functions (Tasks 10, 11, 15). No "TBD"/"handle edge cases"/unspecified-behavior steps remain. The two `> Note` callouts (ext SQL expression, `need_slide` semantics) flag implementer judgment points with the verifying test named, not missing content.

**Type consistency:** `RowMeta`, `Sort`, `SortKey`, `SortDir`, `UiState`, `PersistedMode` defined in Tasks 1/5 and consumed consistently (`browse_all`, `rehydrate_rows`, `sort_rows`, `search_meta`, `get/set_ui_state`, state, backend). `order_by_clause`/`sort_rows` share one ordering definition (NULL-last, path tiebreak). `LIGHTBOX_SIZE`/`GUI_THUMBNAIL_SIZES` used by Tasks 9, 17, 18. Window constants/functions from Task 11 used by 13–15.
