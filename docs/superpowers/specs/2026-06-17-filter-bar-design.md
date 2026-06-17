# Filter bar (size / type / GPS) with live-updating results — design

**Date:** 2026-06-17
**Branch:** `filter-bar`
**Status:** Approved (brainstorming → spec)

## Summary

Add a filter bar beneath the `imgfind-gui` search bar: a two-handle file-size
**range slider**, a **file-type** multi-select, and a **GPS-present** tri-state.
Filters work **standalone or as refinement** — with no text query they browse
all indexed images matching the filters; with a query they restrict the CLIP
vector-search results. Results **live-update** (debounced) as filters change. The
filter model is built to be **extended** with more filters later.

## Decisions (from brainstorming Q&A)

1. **Scope:** standalone + refine. No query → a non-vector "filtered browse"
   query over all images; query present → filters layered onto the vector search.
2. **Size control:** a custom two-handle `RangeSlider` vendored from
   `~/src/utmost/crates/utmost-gui/ui/range_slider.slint` (normalized [0,1] track,
   `range-changed(lo, hi)` callback). Map [0,1] ↔ byte bounds in Rust.
3. **Live-update:** re-run the active query on any filter change, debounced.

## Current state (verified)

- Search is vector-based: `Backend::search(query, offset)` → `SearchEngine::search_meta`
  → `Database::search_similar_images_meta` (`vec0 MATCH ?1 AND k={k} AND distance<=… ORDER BY distance`,
  `LEFT JOIN image_metadata m` for `file_size`). Empty query shows nothing today.
- `image_metadata`: `file_size, width, height, latitude, longitude, camera_make,
  camera_model, datetime_taken` (per `image_id`). File **type/extension is NOT
  stored** — derive from `images.path`.
- `imgfind-gui/src/main.rs`: holds `state: Arc<Mutex<SearchState>>`,
  `search_mode: Arc<Mutex<SearchMode>>` (`Text(String)` | `Similar(String)`),
  `detail: Arc<Mutex<Option<DetailState>>>`, `lb_index`. Queries run off-thread
  via `std::thread::spawn` + `slint::invoke_from_event_loop` + `Weak<MainWindow>`;
  `slint::Image` built only inside the closure. Helpers `spawn_search`,
  `spawn_similar`, `build_tiles_model`.
- `SearchResult { path: String, distance: f32, file_size: Option<i64> }`,
  `PAGE_SIZE = 80`, `SearchConfig::default()` (distance ≤ 1.3, max_k 100).
- `imgfind-gui/ui/app.slint`: search bar + grid + detail panel + lightbox.

## Architecture

### Filter model (extension-first)

A single source-of-truth struct + a shared SQL WHERE-clause builder used by BOTH
query paths. Adding a future filter = one field + one clause arm.

```rust
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filters {
    pub size_min: Option<i64>,   // bytes, inclusive; None = unbounded below
    pub size_max: Option<i64>,   // bytes, inclusive; None = unbounded above
    pub extensions: Vec<String>, // lowercased, no dot; empty = all types
    pub gps: GpsFilter,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpsFilter { #[default] Any, HasGps, NoGps }
```

Clause builder (pure, unit-tested), producing a SQL fragment + bound params:
- size → `m.file_size >= ?` and/or `m.file_size <= ?` (omit the unbounded side)
- type → `(lower(i.path) LIKE ? OR lower(i.path) LIKE ? …)`, one `LIKE` per
  selected extension with a bound `%.ext` pattern (e.g. `%.jpg`). Empty selection
  = all types → no clause. Patterns are bound params, never interpolated.
- gps → `HasGps`: `m.latitude IS NOT NULL AND m.longitude IS NOT NULL`;
  `NoGps`: `(m.latitude IS NULL OR m.longitude IS NULL)`; `Any`: no clause.

The builder returns `(where_sql, Vec<rusqlite param>)` where `where_sql` is either
empty or `" AND <clauses joined by AND>"`, safe to splice after an existing
`WHERE`. Extension `LIKE` patterns are bound params (never interpolated).

### Two query paths (share the builder)

1. **`Database::browse(filters, limit, offset)`** (no vector) →
   `SELECT i.path, m.file_size FROM images i LEFT JOIN image_metadata m ON m.image_id = i.id
    WHERE 1=1 <filter clauses> ORDER BY m.datetime_taken DESC NULLS LAST, i.id DESC
    LIMIT ? OFFSET ?`. Standard pagination. Returns `Vec<(String, Option<i64>)>`
   mapped to `SearchResult` with `distance: 0.0`.
2. **Filtered vector search** — extend `search_similar_images_meta` (or add a
   filtered variant) to splice the `<filter clauses>` into its `WHERE` after the
   `MATCH`/`k`/`distance` predicates. Because filters apply *after* the `k`
   nearest neighbours, `k` is raised to `max_k` so a full page can survive
   filtering. Total filtered results remain bounded by `max_k` (100) — same
   ceiling as today; documented as a v1 limitation.

### Live-update + debounce

Any filter change (`range-changed`, type toggle, GPS change) updates the Rust
`Filters` (held in an `Arc<Mutex<Filters>>`) and triggers a **debounced** re-run
of the active query. The range slider fires continuously while dragging, so a
`slint::Timer` (single-shot, ~250 ms, restarted on each change) coalesces bursts
into one query. The re-run dispatches by current `SearchMode`: `Text(q)` →
filtered vector search; browse (no/empty query) → `browse`. Off-thread +
`invoke_from_event_loop`, identical to existing search. Filters are also passed
into `spawn_search`/`spawn_similar`/`browse` so Load-more and search-similar
honour the active filters.

### Slint UI

New filter row beneath the search bar (its own `HorizontalLayout`):
- vendored `RangeSlider` (new file `imgfind-gui/ui/range_slider.slint`, imported
  by `app.slint`) + a `Text` showing the selected "X–Y MB" range. `lo`/`hi`
  ∈ [0,1] map to `(size_min_bound, size_max_bound)` queried at startup.
- file-type toggle chips, one per distinct extension present in the DB
  (`detail`/type list set from Rust); toggling updates the selected set.
- a 3-way GPS control (Any / Has GPS / No GPS) — e.g. three small toggle buttons
  or a ComboBox.

New `MainWindow` properties (set from Rust) + callbacks: `size-bounds` (min/max
MB labels), `in-out` lo/hi, `available-extensions: [string]`, the selected-type
state, `gps-mode: int`; callbacks `filters-changed()` (or per-control callbacks)
that push the new filter state to Rust. Keep the existing search/grid/detail/
lightbox unchanged except that all queries now carry filters.

### Backend / DB additions

- `Database`: `distinct_extensions() -> Result<Vec<String>>` (lowercased, from
  `images.path`); `file_size_bounds() -> Result<(i64, i64)>` (min/max non-null
  `file_size`, defaulting to `(0, 0)`/sane fallback when empty); `browse(&Filters,
  limit, offset)`; filter params threaded into the vector search. Pure
  `build_filter_clause(&Filters) -> (String, Vec<Box<dyn ToSql>>)` (or a params
  enum) — unit-tested.
- `Backend`: `browse(&Filters, offset) -> Result<Vec<SearchResult>>`;
  `search`/`search_similar` gain a `&Filters` parameter (default `Filters::default()`
  preserves current behavior).

## Testing

- **Pure unit tests:** `build_filter_clause` for each filter and combinations
  (empty → no clause; size both/one-sided; extensions → bound LIKE patterns; each
  GPS variant); extension-from-path derivation; [0,1]↔bytes mapping.
- **DB integration (temp DB):** insert images with varied `(file_size, extension,
  lat/long)`; assert `browse(filters)` returns exactly the matching set for size-
  only, type-only, gps-only, and combined filters; assert a filtered vector
  search excludes non-matching rows (seed embeddings as in the existing
  `find_similar_to_path` test).
- **Live-update/debounce + Slint UI:** verified by running (headless can't render);
  document manual smoke steps.

## Invariants this feature depends on

- **Relative paths / extension** — type filter derives extension from the stored
  relative `images.path`; matching is case-insensitive (`lower(path) LIKE '%.ext'`).
- **`image_metadata` is a LEFT JOIN** — images without metadata rows must still
  appear in browse when filters permit (e.g. GPS `Any`, no size bound); a GPS
  `HasGps` or any size filter naturally excludes rows lacking metadata. Tests pin
  this (an image with no metadata row + `Any`/no-size filter → still returned).
- **Vector search semantics** — filtered vector search keeps `SearchConfig`
  distance/`max_k`; filters only narrow, never reorder by anything but distance.

## Out of scope

- Persisting filter state across launches.
- Date / camera / dimension filters (future — the model + builder are designed to
  add them with one field + one clause each).
- Any change to the CLI/TUI; server (there is none); re-indexing to store mime.
