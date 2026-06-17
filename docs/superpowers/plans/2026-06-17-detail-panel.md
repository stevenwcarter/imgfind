# Detail Panel + Search-Similar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a right-side detail panel to the Slint GUI (single-click selects → panel with larger thumbnail + metadata + "Search similar"; double-click → existing lightbox; Escape closes the panel), where "Search similar" runs a vector search from the selected image's stored embedding.

**Architecture:** A new `Database::find_similar_to_path` reads the seed's stored embedding from the active vec0 table and reuses the existing `search_similar_images_meta`. New `Backend::metadata` (reusing `extract_image_metadata`) and `Backend::search_similar` (filters the seed). A pure controller (`format_metadata` + selection-state) is unit-tested. The Slint UI gains a fixed-width panel inside a top-level `HorizontalLayout` so the grid reflows into fewer columns; select/metadata/similar work runs off the UI thread via the existing `invoke_from_event_loop` + `Weak` pattern.

**Tech Stack:** Rust edition 2024, ratatui-image-free (GUI crate), Slint 1.x, `imgfind` library (`rusqlite`/`sqlite-vec` vec0, `zerocopy`), `image` 0.25.

## Global Constraints

- Rust edition 2024; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt -p imgfind -p imgfind-gui` (NOT `cargo fmt --all` — it touches the sibling `../clipper` repo).
- anyhow `Context`/`with_context` at every fallible boundary.
- Relative↔absolute paths convert via `RelativePath`/`abs_path`; DB rows store paths relative to `Database.parent_dir`.
- Search-similar reuses `SearchConfig::default()` (distance ≤ 1.3, max_k 100) and `state::PAGE_SIZE` (80); pagination/distance semantics match text search.
- The seed embedding comes from the STORED vector in the **active model's** vec0 table (via `vectors_table()`) — no re-embedding. Stored vectors are already L2-normalized; do not re-normalize.
- Off the UI thread: DB/IO and JPEG decode; `slint::Image` constructed ONLY inside the `invoke_from_event_loop` closure; closures stay `Send + 'static`.
- The full-screen lightbox behavior is unchanged except its trigger moves from single-click to double-click (or a panel "View full" button — see Task 4).

## Verified facts

- `Database::search_similar_images_meta(query_embedding: &[f32], limit, offset, distance_threshold, max_k) -> Result<Vec<(String, f32, Option<i64>)>>` (src/database.rs:766). Query vector passed as `.as_bytes()` (zerocopy). Does NOT normalize.
- Embeddings stored in vec0 as LE-f32 bytes (`INSERT INTO {vt} (rowid, embedding) VALUES (?1, ?2)` with `embedding.as_bytes()`). The default model table is `image_vectors`, dim 512.
- `Database::vectors_table() -> Result<String>` (private; the new method is on `Database`, so it can call it). `Database::active_model()`. `Database::get_image_id(&AbsolutePath) -> Result<i64>`.
- `imgfind::database::ImageMetadata { file_size: Option<u64>, width: Option<u32>, height: Option<u32>, latitude: Option<f64>, longitude: Option<f64>, camera_make: Option<String>, camera_model: Option<String>, datetime_taken: Option<String> }`.
- `imgfind::database::extract_image_metadata(file_path: &str) -> Result<ImageMetadata>` reads EXIF from a file.
- `imgfind-gui/src/main.rs` (419 lines): `main()` wires callbacks; `state: Arc<Mutex<SearchState>>`, `lb_index: Arc<Mutex<Option<usize>>>`; helpers `spawn_search(...)`, `build_tiles_model(results, raw_thumbs)`, plus a lightbox image loader. Search results marshalled via `slint::invoke_from_event_loop` + `window.as_weak()`.
- `imgfind-gui/src/state.rs`: `SearchState`, `pub struct SearchResult { path, distance, file_size }`, `pub const PAGE_SIZE: usize = 80`.
- `imgfind-gui/src/backend.rs`: `Backend` (Clone) with `open`, `start_loading_model`, `model_ready`, `search`, `thumbnail`, `abs_path`.

---

## File Structure

- `src/database.rs` — add `find_similar_to_path` (+ test).
- `imgfind-gui/src/backend.rs` — add `metadata`, `search_similar` (+ tests).
- `imgfind-gui/src/detail.rs` (new) — pure `format_metadata` + selection-state helper (+ tests); declared `mod detail;` in main.rs.
- `imgfind-gui/ui/app.slint` — detail panel, grid reflow, select/activate/close/similar callbacks, Escape.
- `imgfind-gui/src/main.rs` — wire the new callbacks (select→populate panel off-thread; search-similar→replace grid; double-click→lightbox; Escape/close).
- Docs: `CLAUDE.md` GUI bullet updated (Task 6).

---

## Task 1: `Database::find_similar_to_path` (vector search from a stored embedding)

**Files:**
- Modify: `src/database.rs` (new method + `#[cfg(test)]` test)

**Interfaces:**
- Produces: `pub fn find_similar_to_path(&self, path: &RelativePath, limit: usize, offset: usize, distance_threshold: f32, max_k: usize) -> Result<Vec<(String, f32, Option<i64>)>>`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `src/database.rs` (mirror the existing `temp_db_path()` helper already in that module):

```rust
#[test]
fn find_similar_to_path_returns_neighbors_from_stored_embedding() {
    use crate::RelativePath;
    use std::path::PathBuf;
    use zerocopy::IntoBytes;

    let db_path = temp_db_path();
    let db = Database::new(&db_path).expect("create db");

    // Two images with distinct 512-dim embeddings (default model dim).
    let mut a = vec![0.0f32; 512];
    a[0] = 1.0;
    let mut b = vec![0.0f32; 512];
    b[1] = 1.0;

    {
        let conn = db.pool.get().expect("conn");
        conn.execute("INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'ha')", [])
            .expect("img a");
        conn.execute("INSERT INTO images (id, path, hash) VALUES (2, 'b.jpg', 'hb')", [])
            .expect("img b");
        conn.execute(
            "INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
            rusqlite::params![1i64, a.as_bytes()],
        )
        .expect("vec a");
        conn.execute(
            "INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
            rusqlite::params![2i64, b.as_bytes()],
        )
        .expect("vec b");
    }

    // Seed = a.jpg. Its nearest neighbour is itself (distance ~0); b.jpg follows.
    let rows = db
        .find_similar_to_path(&RelativePath(PathBuf::from("a.jpg")), 10, 0, 1.3, 100)
        .expect("similar");
    let paths: Vec<&str> = rows.iter().map(|(p, _, _)| p.as_str()).collect();
    assert!(paths.contains(&"a.jpg"), "seed should appear among neighbours");
    assert!(paths.contains(&"b.jpg"), "other image should appear among neighbours");
    // a.jpg (the seed) is closest to itself.
    assert_eq!(rows[0].0, "a.jpg");

    let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
}
```

- [ ] **Step 2: Run — fails (method missing)**

Run: `cargo test -p imgfind --lib database::tests::find_similar_to_path_returns_neighbors_from_stored_embedding`
Expected: FAIL — `no method named find_similar_to_path`.

- [ ] **Step 3: Implement the method**

Add to `impl Database` in `src/database.rs` (near `search_similar_images_meta`):

```rust
/// Find images similar to an already-indexed image, using its STORED
/// embedding from the active model's vec0 table (no re-embedding). The seed
/// itself is typically the nearest neighbour (distance ~0); callers may filter
/// it out. Returns `(relative_path, distance, file_size)` rows.
pub fn find_similar_to_path(
    &self,
    path: &RelativePath,
    limit: usize,
    offset: usize,
    distance_threshold: f32,
    max_k: usize,
) -> Result<Vec<(String, f32, Option<i64>)>> {
    let vt = self.vectors_table()?;
    let rel = path.as_str();
    let conn = self
        .pool
        .get()
        .context("Failed to get DB connection for find_similar_to_path")?;

    let id: i64 = conn
        .query_row(
            "SELECT id FROM images WHERE path = ?1",
            params![rel.as_ref()],
            |row| row.get(0),
        )
        .with_context(|| format!("No indexed image at path {rel}"))?;

    let blob: Vec<u8> = conn
        .query_row(
            &format!("SELECT embedding FROM {vt} WHERE rowid = ?1"),
            params![id],
            |row| row.get(0),
        )
        .with_context(|| format!("No stored embedding for image id {id}"))?;

    // Stored vectors are LE-f32 and already L2-normalized; decode as-is.
    let embedding: Vec<f32> = blob
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    // Reuse the existing vec0 search path with the stored vector.
    self.search_similar_images_meta(&embedding, limit, offset, distance_threshold, max_k)
}
```

Ensure `RelativePath` is in scope (the module already uses `crate::{AbsolutePath, RelativePath, ...}` at the top of `database.rs`).

- [ ] **Step 4: Run — passes**

Run: `cargo test -p imgfind --lib database::tests::find_similar_to_path_returns_neighbors_from_stored_embedding`
Expected: PASS.

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy -p imgfind --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add src/database.rs
git commit -m "feat(db): find_similar_to_path — vec search from a stored image embedding"
```

---

## Task 2: Backend `metadata` + `search_similar`

**Files:**
- Modify: `imgfind-gui/src/backend.rs` (two methods + tests)

**Interfaces:**
- Consumes: `Database::find_similar_to_path` (Task 1); `imgfind::database::{ImageMetadata, extract_image_metadata}`; `crate::state::{SearchResult, PAGE_SIZE}`; `imgfind::config::SearchConfig`; `imgfind::RelativePath`.
- Produces:
  - `pub fn metadata(&self, rel_path: &str) -> anyhow::Result<ImageMetadata>`
  - `pub fn search_similar(&self, rel_path: &str, offset: usize) -> anyhow::Result<Vec<SearchResult>>`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `imgfind-gui/src/backend.rs` (it already has `temp_db()` returning `(Database, PathBuf)` and builds a `Backend` over it — reuse that pattern; if the existing helper is named differently, match it):

```rust
#[test]
fn search_similar_filters_out_the_seed() {
    use imgfind::database::Database;
    use zerocopy::IntoBytes;

    let (db, root) = temp_db();
    {
        let conn = db.pool.get().expect("conn");
        conn.execute("INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'ha')", [])
            .unwrap();
        conn.execute("INSERT INTO images (id, path, hash) VALUES (2, 'b.jpg', 'hb')", [])
            .unwrap();
        let mut a = vec![0.0f32; 512];
        a[0] = 1.0;
        let mut b = vec![0.0f32; 512];
        b[1] = 1.0;
        conn.execute("INSERT INTO image_vectors (rowid, embedding) VALUES (1, ?1)", rusqlite::params![a.as_bytes()]).unwrap();
        conn.execute("INSERT INTO image_vectors (rowid, embedding) VALUES (2, ?1)", rusqlite::params![b.as_bytes()]).unwrap();
    }
    let backend = backend_with(db); // the existing helper that wraps a Database in a Backend

    let results = backend.search_similar("a.jpg", 0).expect("similar");
    let paths: Vec<&str> = results.iter().map(|r| r.path.as_str()).collect();
    assert!(!paths.contains(&"a.jpg"), "seed must be filtered out of similar results");
    assert!(paths.contains(&"b.jpg"), "other images should remain");
    let _ = std::fs::remove_dir_all(root);
}
```

> If the existing test module names its Backend-builder differently than `backend_with`, use that name. If there is no such helper, construct the `Backend` the same way the existing backend tests do.

- [ ] **Step 2: Run — fails**

Run: `cargo test -p imgfind-gui backend::tests::search_similar_filters_out_the_seed`
Expected: FAIL — `no method named search_similar`.

- [ ] **Step 3: Implement both methods**

Add to `impl Backend` in `imgfind-gui/src/backend.rs` (add imports `use imgfind::database::{ImageMetadata, extract_image_metadata};` and `use imgfind::RelativePath;` and `use std::path::PathBuf;` if not present):

```rust
/// EXIF/metadata for an indexed image, read fresh from the file (same fields
/// stored at index time).
pub fn metadata(&self, rel_path: &str) -> Result<ImageMetadata> {
    let abs = self.abs_path(rel_path);
    extract_image_metadata(&abs.to_string_lossy())
        .with_context(|| format!("Failed to read metadata for {rel_path}"))
}

/// Images similar to `rel_path`, using its stored embedding. The seed itself
/// is filtered out of the results.
pub fn search_similar(&self, rel_path: &str, offset: usize) -> Result<Vec<SearchResult>> {
    let sc = SearchConfig::default();
    let rows = self
        .db
        .find_similar_to_path(
            &RelativePath(PathBuf::from(rel_path)),
            PAGE_SIZE,
            offset,
            sc.distance_threshold,
            sc.max_k,
        )
        .with_context(|| format!("Similar search failed for {rel_path}"))?;
    Ok(rows
        .into_iter()
        .filter(|(path, _, _)| path != rel_path)
        .map(|(path, distance, file_size)| SearchResult { path, distance, file_size })
        .collect())
}
```

> `self.db` is the `Database` field on `Backend` (used by the existing `search`/`thumbnail` methods). `SearchConfig` and `SearchResult`/`PAGE_SIZE` imports: mirror how `search` already imports them in this file.

- [ ] **Step 4: Run — passes**

Run: `cargo test -p imgfind-gui backend::tests::search_similar_filters_out_the_seed`
Expected: PASS. Then `cargo test -p imgfind-gui` (all gui tests still pass).

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add imgfind-gui/src/backend.rs
git commit -m "feat(gui): Backend::metadata + search_similar (filters the seed)"
```

---

## Task 3: Pure detail controller — `format_metadata` + selection state

**Files:**
- Create: `imgfind-gui/src/detail.rs`
- Modify: `imgfind-gui/src/main.rs` (add `mod detail;`)

**Interfaces:**
- Consumes: `imgfind::database::ImageMetadata`.
- Produces:
  - `pub fn format_metadata(meta: &ImageMetadata) -> String`
  - `pub struct DetailState { pub path: String, pub filename: String }` with `pub fn select(path: String) -> DetailState` and a free `pub fn filename_of(path: &str) -> String`.

- [ ] **Step 1: Write the failing tests**

Create `imgfind-gui/src/detail.rs`:

```rust
//! Pure, UI-agnostic helpers for the detail panel: metadata formatting and the
//! selection identity. Kept free of Slint so it is unit-testable.

use imgfind::database::ImageMetadata;

/// The seed image the detail panel is showing. Holds the image's OWN identity
/// (not a grid index), so replacing the grid (search-similar) never invalidates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailState {
    pub path: String,
    pub filename: String,
}

/// Last path component for display.
pub fn filename_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

pub fn select(path: String) -> DetailState {
    let filename = filename_of(&path);
    DetailState { path, filename }
}

/// One line per present metadata field; `None` fields omitted entirely.
pub fn format_metadata(meta: &ImageMetadata) -> String {
    let mut lines = Vec::new();
    if let (Some(w), Some(h)) = (meta.width, meta.height) {
        lines.push(format!("Dimensions: {w}×{h}"));
    }
    if let Some(size) = meta.file_size {
        lines.push(format!("Size: {} KB", size / 1024));
    }
    match (&meta.camera_make, &meta.camera_model) {
        (Some(make), Some(model)) => lines.push(format!("Camera: {make} {model}")),
        (Some(make), None) => lines.push(format!("Camera: {make}")),
        (None, Some(model)) => lines.push(format!("Camera: {model}")),
        (None, None) => {}
    }
    if let Some(dt) = &meta.datetime_taken {
        lines.push(format!("Taken: {dt}"));
    }
    if let (Some(lat), Some(lon)) = (meta.latitude, meta.longitude) {
        lines.push(format!("GPS: {lat:.5}, {lon:.5}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_meta() -> ImageMetadata {
        ImageMetadata {
            file_size: None,
            width: None,
            height: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
            datetime_taken: None,
        }
    }

    #[test]
    fn filename_takes_last_component() {
        assert_eq!(filename_of("a/b/c.jpg"), "c.jpg");
        assert_eq!(filename_of("c.jpg"), "c.jpg");
    }

    #[test]
    fn select_captures_path_and_filename() {
        let d = select("sub/dir/photo.png".to_string());
        assert_eq!(d.path, "sub/dir/photo.png");
        assert_eq!(d.filename, "photo.png");
    }

    #[test]
    fn format_metadata_omits_none_fields() {
        let meta = empty_meta();
        assert_eq!(format_metadata(&meta), "");
    }

    #[test]
    fn format_metadata_renders_present_fields() {
        let mut meta = empty_meta();
        meta.width = Some(800);
        meta.height = Some(600);
        meta.file_size = Some(2048);
        meta.camera_make = Some("Canon".to_string());
        meta.camera_model = Some("R6".to_string());
        meta.datetime_taken = Some("2024:01:02 03:04:05".to_string());
        meta.latitude = Some(37.7749);
        meta.longitude = Some(-122.4194);
        let out = format_metadata(&meta);
        assert!(out.contains("Dimensions: 800×600"));
        assert!(out.contains("Size: 2 KB"));
        assert!(out.contains("Camera: Canon R6"));
        assert!(out.contains("Taken: 2024:01:02 03:04:05"));
        assert!(out.contains("GPS: 37.77490, -122.41940"));
    }

    #[test]
    fn format_metadata_partial_camera() {
        let mut meta = empty_meta();
        meta.camera_make = Some("Sony".to_string());
        assert_eq!(format_metadata(&meta), "Camera: Sony");
    }
}
```

Add `mod detail;` to `imgfind-gui/src/main.rs` (next to `mod state;`/`mod backend;`). If `main()` doesn't reference `detail` items yet at this point, a temporary narrow `#[cfg(test)]`-free `#[allow(dead_code)]` is acceptable ONLY if clippy flags unused — but Task 4 wires it, so prefer to land Task 3 and Task 4 close together; if clippy complains about unused in the gap, add `#[allow(dead_code)]` on `mod detail;` with a comment and remove it in Task 4.

- [ ] **Step 2: Run — fails, then passes**

Run: `cargo test -p imgfind-gui detail::`
Expected: FAIL first (module missing), then after creating the file, PASS (5 tests).

- [ ] **Step 3: clippy + commit**

```bash
cargo clippy -p imgfind-gui --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add imgfind-gui/src/detail.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): pure detail controller — format_metadata + selection state"
```

---

## Task 4: Detail panel UI + select/populate/close (and double-click → lightbox)

**Files:**
- Modify: `imgfind-gui/ui/app.slint`, `imgfind-gui/src/main.rs`

**Interfaces:**
- Consumes: `Backend::metadata`, `Backend::thumbnail`, `detail::{select, format_metadata}`, `DetailState`.
- Produces (Slint): on `MainWindow` — `in property <bool> detail-open`, `in property <image> detail-image`, `in property <string> detail-filename`, `in property <string> detail-meta`; callbacks `tile-selected(int)`, `tile-activated(int)`, `detail-close()`, `search-similar()`. (`search-similar` wired in Task 5.)

- [ ] **Step 1: Restructure the layout + add the panel in `app.slint`**

Wrap the existing grid `ScrollView` and a new panel in a top-level `HorizontalLayout`, and bind `cols` to the grid container width (so the panel reflows the grid). Add the panel + new callbacks/properties. Behavior contract (consult Slint 1.x docs via the context7 MCP tool for exact syntax — `HorizontalLayout` stretch, conditional child, `double-clicked`/click-count, `FocusScope` key handling):

- Top-level `HorizontalLayout` containing: (left) the existing grid `ScrollView` with `horizontal-stretch: 1`; (right) `if root.detail-open: Rectangle { width: 340px; … }` — the panel.
- `cols` MUST derive from the grid container's actual width, not `root.width`, so opening the 340px panel reduces columns. (Bind to the `ScrollView`/its parent `width`.)
- Panel contents: a close ✕ `Button` → `root.detail-close()`; an `Image { source: root.detail-image; image-fit: contain; }` (larger thumbnail, ~300px tall); `Text { text: root.detail-filename; }`; `Text { text: root.detail-meta; }` (wraps, multi-line); a `Button { text: "Search similar"; clicked => root.search-similar(); }`.
- Tile `TouchArea`: single `clicked` → `root.tile-selected(i)`; double-click → `root.tile-activated(i)`. If a reliable double-click isn't available in this Slint version, instead add a "View full" `Button` in the panel that fires `tile-activated` for the selected index, and keep single-click = select. (Either satisfies the spec.)
- Escape: a top-level `FocusScope` (or reuse one) such that when `root.detail-open && !root.lightbox-open`, `Key.Escape` → `root.detail-close()`. The lightbox's own Escape handling stays.

> The lightbox overlay stays as-is. Its trigger moves: previously single-click → lightbox; now `tile-activated` (double-click / "View full") opens it.

- [ ] **Step 2: Rewire `main.rs` — select populates the panel off-thread; activate opens lightbox; close clears**

In `imgfind-gui/src/main.rs`:
- Add a holder near the others: `let detail: Arc<Mutex<Option<detail::DetailState>>> = Arc::new(Mutex::new(None));`
- `window.on_tile_selected(move |index| { … })`: read the result path at `index` from `state` (guard the lock; if index out of range, return), set `*detail = Some(detail::select(path))`, set `detail-open = true`, set `detail-filename`. Then spawn a worker thread that calls `backend.metadata(&path)` and `backend.thumbnail(&path, 512)`; marshal back via `invoke_from_event_loop` + `weak`: build the `slint::Image` from the thumbnail bytes (reuse the existing `image_util::jpeg_to_slint_image`), `set_detail_image`, and `set_detail_meta(format_metadata(&meta))`. On metadata/thumbnail error, log via tracing and leave defaults (don't panic).
- Repoint the OLD single-click→lightbox handler: the lightbox open logic currently in `on_tile_clicked` moves to `window.on_tile_activated(move |index| { … })` (same body — load full image off-thread, set `lightbox-image`, `lightbox-open = true`, set `lb_index`). Remove/replace `on_tile_clicked` accordingly (the `.slint` now emits `tile-selected`/`tile-activated`, not `tile-clicked`).
- `window.on_detail_close(move || { *detail = None; w.set_detail_open(false); })`.
- A fresh text search (`on_search`) must clear the panel: in its UI-thread closure set `*detail = None` and `w.set_detail_open(false)` (mirror how it already clears the lightbox: `lb_index`/`set_lightbox_open(false)`).
- Ensure every new `invoke_from_event_loop` closure is `Send + 'static` (only `String`/`ImageMetadata`/`Vec<u8>` cross; `slint::Image` built inside).

- [ ] **Step 3: Build + verify (headless cannot run the GUI)**

Run: `cargo build -p imgfind-gui` → compiles.
Run: `cargo clippy --workspace --all-targets -- -D warnings` → clean.
Run: `cargo test -p imgfind-gui` → existing tests still pass.
Document manual smoke steps in the report (single-click opens panel with thumb+metadata and the grid reflows narrower; Escape/✕ closes; double-click or "View full" opens the lightbox; a new text search closes the panel).

- [ ] **Step 4: fmt + commit**

```bash
cargo fmt -p imgfind -p imgfind-gui
git add imgfind-gui/
git commit -m "feat(gui): right-side detail panel (select→panel, double-click→lightbox, Esc to close)"
```

---

## Task 5: Wire "Search similar"

**Files:**
- Modify: `imgfind-gui/src/main.rs`

**Interfaces:**
- Consumes: `Backend::search_similar`, the `detail` holder + `state`, `build_tiles_model`, `spawn_search`-style marshalling.

- [ ] **Step 1: Implement the `search-similar` callback**

In `imgfind-gui/src/main.rs`, `window.on_search_similar(move || { … })`:
- Read the seed path from the `detail` holder; if `None`, return.
- Set status to `format!("Similar to {}", filename)` and clear/disable as the existing search does (mirror `on_search`'s loading setup, but keep `detail-open` TRUE — the panel stays on the seed).
- Spawn a worker calling `backend.search_similar(&seed_path, 0)`; on the UI thread: apply to `SearchState` (replace results — use `state.start_search`/`apply_page` or the equivalent the code already uses so `view_state`/`has_more`/`next_offset` stay consistent), fetch raw thumbnails for the new results (off-thread, same as `spawn_search`), `build_tiles_model`, `set_tiles`, set `show-load-more` from `has_more`. Do NOT clear the `detail` holder (panel stays on seed).
- "Load more" for similar results: the existing `on_load_more` calls `backend.search(committed_query, offset)`. Make load-more aware of whether the current result set is a similar-search: track a small mode flag (e.g. `Arc<Mutex<SearchMode>>` where `SearchMode` is `Text(String)` or `Similar(String)`), set it in `on_search` and `on_search_similar`, and branch in `on_load_more` to call `backend.search` or `backend.search_similar` with `state.next_offset()`. Keep it minimal — a 2-variant enum in `main.rs` or `state.rs`.

> Reuse `spawn_search`'s thumbnail-fetch + marshal pattern; factor a shared helper if it reduces duplication, but do not over-abstract.

- [ ] **Step 2: Build + verify**

Run: `cargo build -p imgfind-gui`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p imgfind-gui` → all clean/pass.
Manual smoke (document in report): select an image → "Search similar" → grid replaced with similar images, status "Similar to <file>", panel still shows the seed; "Load more" appends more similar results; selecting a result updates the panel; a new text search returns to text mode + closes panel.

- [ ] **Step 3: fmt + commit**

```bash
cargo fmt -p imgfind -p imgfind-gui
git add imgfind-gui/
git commit -m "feat(gui): Search similar — replace grid with vector-similar results, keep panel"
```

---

## Task 6: Docs

**Files:**
- Modify: `CLAUDE.md` (the `imgfind-gui` architecture bullet)

- [ ] **Step 1: Update the GUI bullet**

In `CLAUDE.md`'s `Native GUI (imgfind-gui/)` bullet, add: single-click selects an image and opens a right-side **detail panel** (larger thumbnail + metadata + "Search similar"; Escape closes it; the panel shrinks the grid, doesn't overlay); **double-click** opens the full-screen lightbox; "Search similar" runs a vector search from the selected image's stored embedding. Add a one-line pointer to `docs/superpowers/specs/2026-06-17-detail-panel-design.md`.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document the GUI detail panel + search-similar"
```

---

## Self-Review

**Spec coverage:**
- Right-side panel that shrinks the grid (reflow) → Task 4 (layout, `cols` bound to grid width). ✓
- Single-click selects → panel; double-click → lightbox → Task 4. ✓
- Escape closes panel → Task 4 (top-level FocusScope). ✓
- Larger thumbnail + metadata → Task 4 (thumbnail 512) + Task 3 (`format_metadata`). ✓
- "Search similar" via stored embedding → Task 1 (DB) + Task 2 (backend) + Task 5 (wiring). ✓
- Replace grid, keep panel on seed, Load more → Task 5. ✓
- Selection decoupled from grid index (no stale panel) → Task 3 `DetailState` + Task 4/5 (holder not reset by search-similar). ✓
- Testing: DB vec search (T1), backend seed-filter (T2), format/selection (T3), GUI by running (T4/T5). ✓
- Docs → Task 6. ✓
- Invariants (active-model table, rel↔abs, SearchConfig/PAGE_SIZE) → constraints + T1/T2. ✓

**Placeholder scan:** No TBD/"handle edge cases". The double-click uncertainty and Slint layout syntax carry an explicit behavior contract + context7 instruction + fallback ("View full" button), not a vague hope.

**Type consistency:** `find_similar_to_path(&RelativePath, usize, usize, f32, usize) -> Result<Vec<(String,f32,Option<i64>)>>` (T1) consumed by `Backend::search_similar` (T2); `metadata -> ImageMetadata` (T2) fed to `format_metadata(&ImageMetadata) -> String` (T3); `DetailState { path, filename }` + `select` (T3) used by T4/T5; Slint callbacks `tile-selected`/`tile-activated`/`detail-close`/`search-similar` consistent between T4 and T5. ✓

**Risk flag for implementer:** Slint 1.x specifics (double-click gesture, conditional layout child, `FocusScope` Escape, grid-width binding for reflow) are the main uncertainty — Tasks 4/5 instruct consulting context7 and give a "View full" fallback. The DB/backend/controller logic (Tasks 1-3) is fully specified and unit-tested, so the feature's correctness-critical parts are nailed down regardless of Slint syntax wrangling.
