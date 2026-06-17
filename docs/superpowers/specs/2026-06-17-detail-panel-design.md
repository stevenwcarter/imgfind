# Detail side-panel + search-similar (Slint GUI) — design

**Date:** 2026-06-17
**Branch:** `detail-panel`
**Status:** Approved (brainstorming → spec)

## Summary

Add a right-side **detail panel** to the `imgfind-gui` Slint search page. Single-
clicking a thumbnail selects it and opens the panel (which **shrinks the grid's
width — it does not overlay**); the panel shows a larger thumbnail plus the
image's metadata and a **"Search similar"** button that finds visually-similar
images using the stored CLIP embedding. Pressing **Escape** closes the panel.
The existing full-screen lightbox is **kept**, now opened by **double-click**.

## Decisions (from brainstorming Q&A)

1. **Select vs view:** single-click selects → opens the detail panel; double-
   click opens the existing full-screen lightbox (unchanged). Right-click still
   opens the original in the OS viewer.
2. **Search similar:** runs a vector search seeded by the selected image's
   embedding, **replaces** the grid with the similar results (ranked by
   distance), sets status to `Similar to <filename>`, and **keeps the panel open
   on the seed**. Supports "Load more" like a text search.
3. **Layout:** the panel reduces the grid area (the grid reflows into fewer
   columns); it never covers the images.

## Current state (verified)

- `imgfind-gui/ui/app.slint`: a `MainWindow` with a search bar, a `ScrollView`
  grid of `Tile { path, image, size-kb }` (absolute-positioned; `cols` derived
  from `root.width`), a "Load more" button, and a full-screen lightbox overlay.
  Single-click → `tile-clicked(i)` (opens lightbox today); right-click →
  `tile-open-external(i)`.
- `imgfind-gui/src/backend.rs` `Backend` (Clone): `open`, `start_loading_model`,
  `model_ready`, `search(query, offset) -> Vec<SearchResult>`,
  `thumbnail(rel_path, size) -> Vec<u8>`, `abs_path(rel_path) -> PathBuf`.
- `imgfind-gui/src/state.rs`: pure `SearchState` machine + `SearchResult { path,
  distance, file_size }`.
- `imgfind-gui/src/main.rs`: wires callbacks; search runs off-thread, marshalled
  back via `slint::invoke_from_event_loop` + `Weak<MainWindow>`; `slint::Image`
  built only on the UI thread; `SearchState` in `Arc<Mutex<…>>`.
- Library: `imgfind::database::ImageMetadata { file_size, width, height,
  latitude, longitude, camera_make, camera_model, datetime_taken }`;
  `extract_image_metadata(file_path: &str) -> Result<ImageMetadata>` reads EXIF
  from a file. `Database::get_image_id(&AbsolutePath) -> Result<i64>`,
  `Database::active_model()`, private `vectors_table()`. `search_similar_images_meta`
  already runs a vec0 search from a query vector.

## Architecture

### Selection model (decoupled from grid indices)

Clicking a tile captures that image's OWN data into a `DetailState`, NOT a live
index into the results vector:

```rust
struct DetailState {
    path: String,             // relative path of the seed image
    filename: String,         // display name (path's file component)
    metadata: ImageMetadata,  // fetched for the seed
    // the larger thumbnail image is pushed to the Slint `detail-image` property
}
```

Transitions (a pure helper, unit-tested):
- **select(path)** → set `DetailState` (panel opens).
- **close()** (Escape or ✕) → clear `DetailState` (panel closes).
- **fresh text search** → clear `DetailState` (panel closes).
- **search similar** → keep `DetailState` unchanged (panel stays on the seed)
  while the grid is replaced.

Because the panel holds the seed's own captured data, replacing the grid (search-
similar) never produces a stale/invalid panel — the same robustness lesson as the
lightbox stale-index fix.

### Layout (`app.slint`)

Wrap the current grid `ScrollView` and a new detail panel in a top-level
`HorizontalLayout`. The panel is a fixed-width (`340px`) `Rectangle`, present in
the layout only when `detail-open`. The grid `ScrollView` gets
`horizontal-stretch: 1` so it takes the remaining width; the existing
`cols: max(1, floor((available-width - 32px) / tile-stride))` must compute from
the GRID's width (bind `cols` to the grid container's width, not `root.width`),
so opening the panel reflows the grid into fewer columns. The lightbox overlay is
unchanged (still overlays the whole window).

### Backend additions (`imgfind-gui/src/backend.rs`)

- `pub fn metadata(&self, rel_path: &str) -> Result<ImageMetadata>` — reuse the
  library's `extract_image_metadata(&self.abs_path(rel_path).to_string_lossy())`.
  (Re-extracting from the file yields the same fields stored at index time and
  needs no new DB query.)
- `pub fn search_similar(&self, rel_path: &str, offset: usize) -> Result<Vec<SearchResult>>`
  — calls a new `Database` method (below), maps rows to `SearchResult`, and
  filters out the seed `rel_path` itself (the seed is its own nearest neighbour
  at distance ~0).

### Database addition (`src/database.rs`)

- `pub fn embedding_for_path(&self, path: &RelativePath) -> Result<Vec<f32>>` (or
  a combined `search_similar_to_path`) — resolve the image id, then
  `SELECT embedding FROM <active vec0 table> WHERE rowid = ?1`, decoding the
  stored vector to `Vec<f32>`. The active table comes from the existing
  `vectors_table()`/`active_model()`. The backend then feeds this vector to the
  existing `SearchEngine`/`search_similar_images_meta` path with
  `SearchConfig::default()` and `PAGE_SIZE`. Implementation may instead expose a
  single `Database::find_similar_to_path(path, limit, offset, threshold, max_k)`
  that does the embedding lookup + vec search in one method — the plan picks
  whichever keeps the seam cleanest, but the **embedding must come from the
  stored vector in the active model's table** (no re-embedding).

### Threading

`select` (fetch metadata + larger thumbnail), and `search_similar` (DB lookup +
vec search + thumbnail decode for the new grid) run OFF the UI thread via the
existing `std::thread::spawn` + `slint::invoke_from_event_loop` + `Weak` pattern.
`slint::Image` values (the panel's larger thumbnail; grid tiles) are constructed
only inside the event-loop closure. The closures stay `Send + 'static`
(`String`/`ImageMetadata`/`Vec<u8>` cross the boundary; `slint::Image` does not).

## Slint interface (`app.slint`)

New on `MainWindow`:
- `in property <bool> detail-open: false;`
- `in property <image> detail-image;` (larger thumbnail)
- `in property <string> detail-filename;`
- `in property <string> detail-meta;` (preformatted multi-line metadata text —
  simplest; built in Rust from the non-null `ImageMetadata` fields)
- `callback tile-selected(int);` (single-click → select)
- `callback tile-activated(int);` (double-click → lightbox; replaces today's
  single-click→lightbox)
- `callback detail-close();`
- `callback search-similar();`
- Escape: a top-level `FocusScope` closes the panel when `detail-open && !lightbox-open`.

The grid tile `TouchArea`: single `clicked` → `tile-selected(i)`; a
double-click → `tile-activated(i)`. (Slint exposes double-click via the
`pointer-event`/click-count or a `double-clicked` callback in 1.x; if not
available, use a "View full" button in the panel as the lightbox entry and treat
single-click as select — see Risks.)

## Metadata display

Build `detail-meta` in Rust from `ImageMetadata`, one line per present field,
omitting `None`:
- `Dimensions: {width}×{height}`
- `Size: {file_size/1024} KB`
- `Camera: {camera_make} {camera_model}`
- `Taken: {datetime_taken}`
- `GPS: {latitude}, {longitude}`
Plus `detail-filename` from the path's file component. The seed's similarity
distance is not shown (it's the seed).

## Testing

- **Backend/DB (unit, temp DB):** `embedding_for_path`/`find_similar_to_path`
  round-trip — insert an image row + a known embedding into the active vec table,
  assert the retrieved vector matches and that a similar-search returns the
  expected rows with the seed filtered out. `metadata` returns the expected
  fields for a file with known EXIF (or asserts graceful handling when EXIF is
  absent).
- **Pure selection-state transitions (unit):** select sets the seed; close/fresh-
  search clears; search-similar preserves the seed. Put this in a small testable
  helper (mirrors `state.rs`).
- **Metadata formatting (unit):** `format_metadata(&ImageMetadata) -> String`
  omits `None` fields and formats present ones exactly.
- Slint rendering is not headless-testable (the GUI has only logic tests); panel
  layout + reflow verified by running.

## Invariants this feature depends on

- **Stored embeddings are per-active-model** — `search_similar` must read from
  the active model's vec0 table (via `vectors_table()`), matching how text search
  resolves its table; a mismatch would compare across dimensions.
- **Relative↔absolute path conversion** — metadata extraction and thumbnail load
  go through `abs_path`/`RelativePath` exactly as the rest of the GUI does.
- **`SearchResult`/`PAGE_SIZE` and `SearchConfig` defaults** — search-similar
  reuses them so its pagination and distance/k semantics match text search.

## Risks

- **Double-click in Slint 1.x:** `tile-activated` (double-click → lightbox) may
  not map to a first-class Slint callback in this version. Behaviour contract:
  single-click selects (opens panel); there must be a way to reach the full
  lightbox. If a reliable double-click gesture isn't available after checking the
  Slint 1.x docs, the fallback is a **"View full"** button in the detail panel
  that fires the lightbox — single-click still selects. Either satisfies the
  design; the plan picks based on what Slint supports.
- **Grid reflow:** `cols` currently derives from `root.width`. It MUST be rebound
  to the grid container's actual width so that opening the 340px panel reflows
  the grid into fewer columns (not merely clips it). Verify by running.
- **Selecting a result of a similar-search** updates the panel to that new seed —
  confirm the select path works identically for similar-search results (they are
  ordinary `SearchResult`s, so it should).

## Out of scope

- Editing metadata; in-panel map/GPS rendering; multi-select; re-embedding the
  image (we use the stored vector). The lightbox itself is unchanged beyond its
  new double-click trigger.
