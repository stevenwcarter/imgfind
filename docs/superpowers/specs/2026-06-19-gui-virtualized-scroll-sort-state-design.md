# GUI: Virtualized Infinite Scroll, Sorting, Thumbnail Persistence, Neighbor Preload & Session State

**Date:** 2026-06-19
**Status:** Approved (brainstorm → spec)
**Crate(s):** `imgfind` (core lib/CLI), `imgfind-gui` (Slint GUI)

## Summary

A bundle of related GUI improvements to `imgfind-gui`, plus the supporting
library/CLI/config/schema changes:

1. **Virtualized infinite scroll** in the grid — a moving-window renderer over the
   full ordered result set (utmost's UX, sized to imgfind's small per-row data:
   Approach C), replacing the current paged `Vec` + manual "Load more" button.
2. **Sort selector** — sort by Name / Size / Type with a direction-flip button;
   Size/Type use Name as the secondary key. Relevance ordering is preserved (and
   exposed as a sort mode) while a semantic query is active.
3. **Thumbnail persistence everywhere** — every image a GUI surface displays (grid,
   detail panel, lightbox) is stored in the DB, including a new large cached size
   for the lightbox/preview. `imgfind thumbnails` documents the GUI sizes so they
   can be pre-generated.
4. **Neighbor preloading** — when the lightbox or detail panel opens/navigates,
   preload `n` images before and after the focus (config-driven, default 2), in an
   increasing arc from the focus outward.
5. **Configurable default sort** and **browse-all on startup** (default name asc).
6. **Persisted session/UI state** in the `.imgfind` DB — reopening the same DB
   restores the exact prior session (search text, sort, filters, selection, focused
   item, detail-panel state, scroll position, and the **found result set itself**),
   without re-running the query.

## Goals / Non-goals

**Goals**
- Smooth, flat-memory scrolling over arbitrarily large browse result sets.
- Persist every displayed thumbnail size so repeat views and pre-generation work.
- Instant session restore by persisting results by reference (ordered image ids).
- Keep all non-trivial logic in pure, unit-testable Rust functions (TDD); the Slint
  UI wiring itself is verified by build + manual smoke.

**Non-goals**
- No map view (still unported).
- No change to the indexing/embedding pipeline or vector-search math.
- No per-user multi-session history — exactly one persisted session per DB
  (last-wins).
- No automated UI-interaction tests (Slint UI is not reliably testable here).

## Clarified decisions (from brainstorming)

- **Search vs browse behavior:** A semantic (vector) search stays *relevance-ordered
  and bounded*. The moving-window infinite scroll and Name/Size/Type sorting are the
  browse/filter experience. While a query is active the sort selector exposes a
  `Relevance` mode (the default); choosing Name/Size/Type re-orders only the matched
  result set; scroll ends at the last match.
- **Lightbox image:** Introduce a new bounded "large" cached size (`LIGHTBOX_SIZE`,
  long edge ~2048px). The lightbox and preview preloading use it and persist it like
  300/512. RAW long-edge demosaic happens once, then is cached.
- **Persistence timing:** On exit only — persist once after the Slint event loop
  returns, from the captured state holders.
- **Session restore:** Do *not* re-run the query. Persist the found items as an
  ordered list of image ids; on restore, rehydrate `RowMeta` for those ids in one
  indexed query (preserving order, dropping any deleted images) and populate the grid
  directly. Thumbnails stream in lazily via the moving window.

## Architecture

### Core library (`imgfind` crate)

#### Sorting (`src/database.rs`, `src/filters.rs` or new `src/sort.rs`)

Introduce a sort type, kept in the core crate so both CLI and GUI share it:

```rust
pub enum SortKey { Name, Size, Type }      // Type = file extension
pub enum SortDir { Asc, Desc }
pub struct Sort { pub key: SortKey, pub dir: SortDir }
```

- `Sort` maps to a SQL `ORDER BY` fragment:
  - `Name` → `i.path <DIR>`
  - `Size` → `m.file_size <DIR>, i.path ASC` (Name as the secondary, tie-break key)
  - `Type` → extension `<DIR>, i.path ASC`. Extension is derived in SQL (e.g.
    `lower(substr(i.path, ...))` or an expression equivalent to the Rust-side
    `rsplit_once('.')` used by `distinct_extensions`) so ordering is stable and
    matches the chip extensions. The secondary key is `i.path ASC`.
  - `NULL` file_size sorts last regardless of direction (use
    `ORDER BY m.file_size IS NULL, m.file_size <DIR>, i.path ASC`) so size-less rows
    don't dominate the head of the list.
- `browse` gains a `sort: &Sort` parameter, replacing the hardcoded
  `ORDER BY m.datetime_taken DESC, i.id DESC`. Its return type is extended (see
  RowMeta) so the GUI can sort by and display size/type.
- **Vector search** keeps relevance (`distance`) ordering in SQL and stays bounded by
  `k` / `distance_threshold`. Re-sorting a search result by Name/Size/Type is done in
  memory over the matched rows via a pure `sort_rows(&mut Vec<RowMeta>, &Sort)` helper
  (so the SQL `ORDER BY` builder and the in-memory comparator share one definition of
  "the order"). Returning to `Relevance` restores the persisted/queried relevance
  order (which the GUI retains).

#### Full ordered result list (Approach C backbone)

Add a lightweight query returning the **entire** filtered+sorted browse result set as
a `Vec<RowMeta>`:

```rust
pub struct RowMeta {
    pub id: i64,            // images.id — stable reference for persistence
    pub path: String,       // relative path (DB invariant)
    pub size: Option<i64>,  // file_size
    pub ext: String,        // lowercased extension (may be "")
}
```

- `browse_all(f: &Filters, sort: &Sort) -> Result<Vec<RowMeta>>` — no LIMIT/OFFSET;
  returns every match in sorted order. Per-row data is tiny, so holding the whole list
  in memory is cheap even at 100k+ rows. This `Vec<RowMeta>` is the moving window's
  backbone; the window only bounds *thumbnail decode/render*, not the id list.
- A companion `rehydrate_rows(ids: &[i64]) -> Result<Vec<RowMeta>>` fetches `RowMeta`
  for an explicit ordered id list (single indexed query, results re-ordered to match
  input, missing ids dropped) — used by session restore.
- Vector search returns `Vec<RowMeta>` as well (with relevance order), so grid,
  sorting, persistence, and the window all operate on one row type.

#### Large cached thumbnail size (`src/thumbnail.rs`)

- New constant `LIGHTBOX_SIZE: u32 = 2048` (long-edge target). The `thumbnails` table
  is already keyed by `(image_hash, size)`, so no schema change is needed.
- `get_or_generate_thumbnail` already persists on miss; the lightbox/preview switch
  from live `decode_full_image` to `get_or_generate_thumbnail(.., LIGHTBOX_SIZE)`, so
  the large render is decoded once (RAW demosaiced at most once), stored, and reused.
- Define the canonical GUI size set in one place (e.g.
  `pub const GUI_THUMBNAIL_SIZES: &[u32] = &[300, 512, 2048]`) so the grid (300),
  detail (512), lightbox (2048), and the CLI help text all reference the same source
  of truth.

#### `ui_state` table (migration 003, `src/database.rs`)

- Bump `LATEST_MIGRATION_VERSION` to `3`; add `migration_003_ui_state`.
- Single-row table holding the serialized session:

  ```sql
  CREATE TABLE IF NOT EXISTS ui_state (
      id INTEGER PRIMARY KEY CHECK (id = 1),   -- enforce single row
      state_json TEXT NOT NULL,
      updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
  );
  ```

- Store the `UiState` as JSON (`serde_json`) in `state_json`. JSON keeps the struct
  free to evolve (new optional fields default cleanly) without further migrations.
- `Database` methods: `get_ui_state() -> Result<Option<UiState>>` and
  `set_ui_state(&UiState) -> Result<()>` (UPSERT on `id = 1`).
- `UiState` lives in the core crate (so `Database` can (de)serialize it) and is
  imported by the GUI:

  ```rust
  pub struct UiState {
      pub search_text: String,
      pub mode: PersistedMode,        // Browse | Text | Similar(seed_id)
      pub sort: Sort,
      pub filters: Filters,           // already serde-friendly
      pub result_ids: Vec<i64>,       // the ordered found set (by reference)
      pub selected_index: Option<usize>,
      pub detail_open: bool,
      pub scroll_y: f32,              // Flickable viewport-y
  }
  ```

### Config (`src/config.rs`, `~/.imgfind/config.toml`)

New `[gui]` section, wired with serde defaults exactly like `IndexConfig`/`SearchConfig`:

```rust
pub struct GuiConfig {
    #[serde(default = "default_preload_neighbors")]
    pub preload_neighbors: usize,        // default 2
    #[serde(default = "default_sort_key")]
    pub default_sort: SortKeyConfig,     // default Name
    #[serde(default = "default_sort_dir")]
    pub default_sort_direction: SortDirConfig, // default Asc
}
```

- `Config` gains `#[serde(default)] pub gui: GuiConfig`. A config file with no `[gui]`
  section parses to the defaults (preload 2, name asc).
- The sort key/dir config values map to the core `SortKey`/`SortDir` (use lowercase
  string serde reprs: `"name" | "size" | "type"`, `"asc" | "desc"`).

### GUI: virtualized grid (Approach C — `imgfind-gui/src/`, `ui/app.slint`)

Replace the paged-`Vec` + "Load more" model with a moving window over the full
`Vec<RowMeta>`:

- **Slint markup (`ui/app.slint`):** the grid becomes a `Flickable` whose
  `viewport-height = ceil(rows / cols) * TILE_PITCH_Y`. Only the windowed `Tile`s are
  in the `tiles` model; each tile is positioned absolutely via its
  `absolute_row`/`absolute_col` (× pitch) so it lands at its true grid coordinate. The
  `Flickable`'s `viewport-y` and the computed `cols` are exposed to Rust. Remove the
  "Load more" button and its `show_load_more` plumbing.
- **Windowing logic (pure, testable — e.g. `imgfind-gui/src/window.rs`):**
  - `visible_range(scroll_y, viewport_h, cols, pitch_y, total) -> Range<row>`.
  - `window_range(visible: Range<row>, cols, total) -> Range<index>` — the bounded
    set of item indices to render (visible rows ± a buffer of `SLIDE_TRIGGER_ROWS`,
    clamped to `[0, total)`). Window size derived from visible tile count with a
    headroom multiplier and clamped to a `[MIN, MAX]` tile band (utmost uses 500/5000;
    pick imgfind-appropriate constants).
  - A `need_slide`-style check decides when the rendered window must move (when the
    visible range enters the outer buffer rows), to avoid rebuilding the tile model on
    every scroll tick.
- **Decode + cache:** a periodic sync (timer, ~100ms, as in utmost/the existing TUI
  pattern) reads `viewport-y`/`cols`, recomputes the window, and for each windowed row
  lacking a cached image requests a 300px thumbnail on background worker thread(s) via
  `get_or_generate_thumbnail` (so it persists). Decoded `slint::Image`s live in a
  bounded LRU; off-window images are evicted to keep memory flat. UI updates from
  workers go through `invoke_from_event_loop`; a generation/epoch guard discards stale
  decodes (consistent with the existing lightbox guard and the Slint skill).
- **Keyboard nav (`imgfind-gui/src/nav.rs`):** `move_selection` continues to operate
  on indices into the full result list. Selection drives `viewport-y` so the selected
  tile scrolls into view in the virtual grid (compute the target scroll from the
  selected row × pitch, clamped). No wrap, clamping at global first/last as today.

### GUI: sort selector (`ui/app.slint`, `imgfind-gui/src/main.rs`)

- A dropdown/segmented control beside the search/filter bar offering **Name / Size /
  Type**, plus **Relevance** shown only while a semantic query is active (and selected
  by default in that case). A direction-toggle button (asc/desc, ASCII glyph per the
  Slint default-font note — e.g. `^` / `v` or `A↓`-style ASCII, not a symbol-font
  arrow) sits next to it.
- Changing the sort key or direction:
  - **Browse mode:** re-runs `browse_all` with the new `Sort` and rebuilds the window
    (selection preserved by id where possible, else reset to first).
  - **Search mode:** re-sorts the matched `Vec<RowMeta>` in memory via `sort_rows`;
    `Relevance` restores the retained relevance order.

### GUI: neighbor preloading (`imgfind-gui/src/main.rs`)

- Pure helper `preload_arc(i: usize, n: usize, len: usize) -> Vec<usize>` returns the
  load order: `[i, i+1, i-1, i+2, i-2, …]` up to distance `n`, each clamped to
  `[0, len)` and de-duplicated, focus (`i`) always first.
- On opening or navigating the lightbox/detail panel: load the focus image first
  (distance 0), then iterate `preload_arc` issuing background decodes **at the
  surface's display size** — `LIGHTBOX_SIZE` (2048) for the lightbox, 512 for the
  detail/preview panel — via `get_or_generate_thumbnail`, so they persist and are
  instantly reusable on the next nav. The existing generation guard cancels stale work
  when the user moves on.
- `n` comes from `GuiConfig::preload_neighbors`.

### GUI: startup & exit (`imgfind-gui/src/main.rs`)

- **Startup:** after the model/DB are ready, call `get_ui_state()`.
  - **Some(state):** rehydrate via `rehydrate_rows(state.result_ids)` → load straight
    into the grid; restore `search_text`, `sort`, `filters`, detail-panel open/seed,
    `selected_index`, and `scroll_y` into the controls/window (display + state only —
    **no query executed**). Then start the windowed thumbnail loads.
  - **None:** browse-all with `GuiConfig` default sort (name asc), select the first
    item, grab grid focus (as today after a search).
- **Exit:** capture the `Arc<Mutex<…>>` state holders (results→ids, selected, detail,
  sort, filters, search text, current `viewport-y`); after `run()` returns, build a
  `UiState` and call `set_ui_state`. (Persisting after the event loop exits avoids
  Slint close-event timing pitfalls.)

### CLI: `imgfind thumbnails` (`src/main.rs`)

- Extend help text to state the GUI sizes from `GUI_THUMBNAIL_SIZES`:
  *"The GUI uses 300px (grid), 512px (detail panel), and 2048px (lightbox/preview).
  Pre-generate these to avoid first-view decoding."*
- Allow generating the GUI set conveniently: accept repeated `--size` (a `Vec<u32>`)
  and/or a `--gui-sizes` flag that expands to `GUI_THUMBNAIL_SIZES`; default behavior
  (single 300px) is preserved when no size flags are given. The batch generator
  (`generate_missing_thumbnails_batch`) is invoked once per requested size.

## Data flow

1. **Startup** → `Config::load` (+ `[gui]`) → `Database` open (runs migration 003) →
   `get_ui_state`. Restore session (rehydrate ids) or browse-all default.
2. **Scroll** → Slint updates `viewport-y` → 100ms sync reads it → `window_range` →
   request missing 300px thumbs on workers → workers persist + `invoke_from_event_loop`
   → tiles model updated; off-window images evicted from LRU.
3. **Sort change** → browse: `browse_all(filters, sort)`; search: `sort_rows`. Rebuild
   window.
4. **Open lightbox/detail** → load focus first (persisted) → `preload_arc` neighbors,
   all at the surface's display size (2048 lightbox / 512 detail), persisted,
   generation-guarded.
5. **Exit** → after `run()` → assemble `UiState` (incl. ordered `result_ids` +
   `scroll_y`) → `set_ui_state`.

## Error handling

- DB/decoding errors during windowed loads are logged (`tracing`) and surface as the
  existing placeholder tile; one failed thumbnail never blocks the window.
- `get_ui_state` returning a malformed/old JSON shape is treated as `None` (log + fall
  back to browse-all default) — never a hard failure on launch.
- `rehydrate_rows` dropping deleted ids is expected, not an error; selection index is
  re-clamped to the rehydrated length.
- Config with a missing/partial `[gui]` section falls back to field defaults.

## Invariants this feature depends on

- **Relative-path invariant:** `RowMeta.path` is DB-relative; any filesystem access
  goes through `relative_to_abs_path` (unchanged).
- **Thumbnail table is size-agnostic:** `(image_hash, size)` uniqueness already
  supports arbitrary sizes; the 2048 size is just another row.
- **`get_or_generate_thumbnail` persists on miss:** this is the load-bearing behavior
  that makes "thumbnails stored whenever used" true for all three GUI surfaces — pinned
  by a test (below) so a later refactor can't silently turn it into read-only.
- **Migration runner is idempotent and ordered:** migration 003 uses `IF NOT EXISTS`
  and bumps `user_version`.

## Testing strategy (TDD)

Pure-function / DB-integration tests (write first):

1. **Sort `ORDER BY` builder** — each `SortKey`×`SortDir` produces the expected
   fragment incl. the `path` secondary key and NULL-size-last handling.
2. **`browse_all` ordering** (DB integration) — fixture rows verify Name/Size/Type
   ordering, direction flip, and the name tiebreaker for equal size/type; NULL sizes
   sort last.
3. **`sort_rows` in-memory comparator** — matches the SQL ordering for the same data
   (shared definition of order); `Relevance` is a no-op preserving input order.
4. **`rehydrate_rows`** — preserves input id order and drops missing ids.
5. **`UiState` round-trip** — serde JSON round-trip + `set_ui_state`/`get_ui_state`
   DB round-trip; malformed JSON → `None`.
6. **Config defaults** — TOML with no `[gui]` parses to preload=2, name, asc; explicit
   values parse correctly; bad enum strings handled.
7. **`preload_arc`** — `(i, n, len)` cases: middle, near both edges, `n=0`,
   `len<=1`, focus-first ordering, dedup at clamps.
8. **Window math** — `visible_range` / `window_range` for representative
   scroll/cols/total combos, including top, bottom, and a `need_slide` boundary case.
9. **Thumbnail persistence** — calling the GUI fetch path at 300/512/2048 lands a
   `thumbnails` row at that size (pins the invariant above).

**Not auto-tested (manual smoke + build):** Slint markup, the 100ms sync timer wiring,
keyboard scroll-into-view, exit-time persistence hook. Logic is factored into the pure
modules above precisely so these thin wiring layers are all that's left untested.

## Documentation updates

- `CLAUDE.md`: update the GUI section (grid is now virtualized infinite scroll, not
  paged Load-more; sort selector; lightbox uses the cached 2048 size; neighbor
  preload; session persistence in `ui_state`). Add `[gui]` config keys. Correct the
  stale "no migrations" note (there is a migration runner) and record migration 003.
  Note `LIGHTBOX_SIZE` / `GUI_THUMBNAIL_SIZES`.
- `USAGE.md`: document the new `imgfind thumbnails` size flags/help and the `[gui]`
  config section.

## Rollout / risk

- Largest risk is the virtualization rewrite; it is isolated behind the pure
  `window.rs` math (tested) plus thin Slint wiring. The id-list backbone avoids
  utmost's second-thread `FetchWindow` protocol.
- Schema change is additive (new table, `IF NOT EXISTS`), safe on existing DBs.
- Lightbox behavior change (cached 2048 vs live full-res) is fix-forward; existing
  full-res-only path is removed for the lightbox but `decode_full_image` remains for
  any other caller.
