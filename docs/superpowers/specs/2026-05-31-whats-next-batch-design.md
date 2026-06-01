# whats-next batch (2026-05-31) — design spec

Bundled implementation of 13 `WHATS-NEXT.md` items selected via `/whats-next --execute`.
One branch (`whats-next-batch/2026-05-31`), six commits grouped by surface. Forward-looking
opportunity work — no bug fixes (those live in `bughunt.md`).

## Decisions locked with the user (brainstorming)

- **Structure:** one branch, one PR, commits grouped by surface (TUI / web / CLI / perf-index / search / graphql).
- **W18 knobs:** `SearchConfig` struct, `--threshold` CLI flag + `[search]` section in `config.toml`. (`--limit` already exists on `search`.)
- **W32 scope:** scaffold the real Mutation root **plus** one concrete vertical slice — a `favorites` table + `toggleFavorite` mutation + a read path. Proves the write plumbing end-to-end.
- **W15 batching:** configurable batch size (default 32, `[index].batch_size`); per-image decode/embed failures are logged + counted + skipped, the rest of the batch proceeds; the existing skip-if-already-indexed check runs before batching.
- **Verification:** `cargo test && cargo build --release`; in `site/`: `yarn build && yarn lint && yarn test`; unit tests for pure logic. No live indexing benchmark, no running server (no GPU/model assumed in this env).

## Key current-state facts (from code anchors, 2026-05-31)

- `ClipEmbedder` (sibling `../clipper`, blocking, `anyhow::Result`):
  - `new(model_path: Option<String>, tokenizer_path: Option<String>, use_cpu: bool) -> Result<Self>`
  - `get_image_embedding(&self, &str) -> Result<Vec<f32>>`
  - `get_image_embeddings(&self, &[String]) -> Result<Vec<Vec<f32>>>` — **loads all paths into one tensor; one bad path fails the whole call.**
  - `get_image_embeddings_from_dynamic(Vec<DynamicImage>) -> Result<Vec<Vec<f32>>>` — used for the skip-failures path.
- `GraphQLContext { db: Database, basepath: String, embedder: Arc<ClipEmbedder> }` — `db` is a plain `Database` sharing the r2d2 pool (Clone+Send). Mutations get a pooled connection via `&self`.
- `Database` insert pattern: `let mut conn = self.pool.get()?; let tx = conn.transaction()?; … tx.commit()?;`
- `images(id INTEGER PK AUTOINCREMENT, path TEXT UNIQUE, hash TEXT, created_at)`; paths stored **relative** to DB parent dir.
- `image_metadata` columns confirmed: `file_size,width,height,latitude,longitude,camera_make,camera_model,datetime_taken`. Existing indexes: `idx_metadata_image_id`, `idx_metadata_gps(latitude,longitude)`.
- Search SQL: three variants (`search_similar_images`, `search_similar_images_with_raw_blob`, `search_similar_images_with_blob`), each hardcoding `distance <= 1.3` and `limit.clamp(1, 100)`.
- r2d2 pool: built with `.with_init(PRAGMA foreign_keys=ON)`, default max size (10); WAL not set on the main DB.
- `indicatif` already imported in `main.rs` and used for the index progress bar.
- Web `Images.tsx`: `useSearchParams` drives `query`; `getImages()` fetches `/api/v1/search/{q}`, swallows errors to `console.error`; results render in a `flex flex-wrap gap-4 p-4` grid; no loading/empty/error UI. Tailwind v4.

---

## Per-item design

### Commit 1 — `feat(tui): help overlay, zoom cues, pagination hints` (W1, W13, W25)

**W1 — help overlay.** Add `show_help: bool` to `App` (init `false`). In `handle_key_events` (Normal mode), bind `?` to toggle it; while `show_help` is true, any key (or `?`/`Esc`) closes it. Render a centered modal (`Clear` + bordered `Paragraph`) in the `ui.rs` render pipeline when `show_help`. Bindings text comes from a single `const`/function (`keybindings_help()`), establishing one source of truth so the overlay can't drift from the handler. List: `e` edit search · `h/j/k/l` move focus · `H/L` page · `1-9`/`Enter` zoom · `Esc` close zoom/help · scroll/right-click zoom · `?` help · `q`/`Ctrl-C` quit.

**W13 — zoom status line.** When an image is zoomed (`zoomed_image.is_some()`), render a one-line status at the bottom of the zoom area: `scroll to zoom | right-click to reset | ESC to close`. Implement as a `render_zoom_status()` in `ui.rs` using the existing `zoomed_image_rect`.

**W25 — pagination hints.** In `ui.rs render_pagination`, change the format string to `Page {}/{} ({} results) [H prev | L next]`. Pure string change.

Files: `src/tui/app.rs`, `src/tui/ui.rs`, possibly a small `src/tui/widget/help.rs`.
Tests: unit test `keybindings_help()` returns non-empty and includes the core keys; pagination string format test if cheaply isolatable.

### Commit 2 — `feat(web): loading + empty/error states, masonry grid` (W5, W6, W27)

**W5 — loading.** Add `loading` state; set true before `fetch`, false in `finally`. Disable the input and show a "Searching…" indicator while loading.

**W6 — empty/error states.** Add `error: string | null`. In `catch`, set a user-visible error (banner above the grid) instead of only `console.error`; remove the `console.log` of fetched data. Render three explicit states under the search bar:
- idle (no query entered): "Enter a search query to find images."
- zero results (query ran, `images.length === 0`, not loading, no error): `No images found for "{query}".`
- error: red banner with the message.
Track whether a search has actually run (`hasSearched`) to distinguish idle from zero-results.

**W27 — masonry grid.** Replace `flex flex-wrap` with a CSS-columns masonry (`columns-2 sm:columns-3 lg:columns-4 gap-4`, each item `mb-4 break-inside-avoid w-full`). No new dependency. **Known tradeoff:** CSS columns order top-to-bottom within each column, not strict left-to-right by relevance. Acceptable per design; if strict relevance order matters we'd swap to a row-based masonry lib — flagged, not done.

Files: `site/src/page/Images.tsx` (+ the result item / `LightboxViewer` styling as needed).
Tests: vitest for the state-selection helper (idle vs no-results vs error vs results) if extracted to a pure function; otherwise rely on `yarn lint` + `yarn build`.

### Commit 3 — `feat(cli): model-load progress, status db path` (W9, W17)

**W9 — model-load feedback.** Wrap `ClipEmbedder::new(...)` (index path ~`main.rs:254`, and the search path where the model loads for a query) in an `indicatif` spinner: start `ProgressBar::new_spinner()` with message "Loading CLIP model… (this may take a minute on first use)", `finish_and_clear()` once loaded. Suppress under `--quiet`/non-TTY (mirror existing `quiet` handling). Reuse for query-time embedding where the model is loaded.

**W17 — status db path.** In `show_status`, lead output with `Database: {db_path}` (the `db_path: &PathBuf` is already in scope) before the stats block. Keep existing stats.

Files: `src/main.rs`.
Tests: none meaningful (I/O + progress); covered by build.

### Commit 4 — `perf(index): batch GPU embedding, pool+WAL tuning, metadata indexes` (W15, W24, W10)

**W15 — batch embedding.** Restructure `index_directory`:
1. Walk + filter as today (honor ignore patterns).
2. For each candidate: compute `oshash`, run the **existing skip-if-already-indexed** check. Collect the pending set (path + hash).
3. Process pending in chunks of `batch_size` (default 32, from `[index].batch_size`, overridable by a new `--batch-size` flag on `index`):
   - For each path in the chunk, decode to `DynamicImage` individually; on decode failure, log + increment `failed`, skip that image (do **not** abort the chunk).
   - Batch-embed the survivors via `get_image_embeddings_from_dynamic(images)`; if the whole batch call still errors, fall back to logging + counting the chunk as failed (defensive).
   - Normalize each embedding (existing `normalize_vector`), insert all good rows for the chunk in one transaction.
4. Metadata extraction + backfill stays after embedding (unchanged ordering); can remain per-image for this batch.
5. Summary line reports `indexed / skipped(already-indexed) / failed`.
   Update the progress bar to advance per processed image.

New `Database` method: `insert_images_batch(&mut self, rows: &[(path, hash, embedding)])` (or reuse `insert_image` inside one transaction) to insert a chunk atomically.

**W24 — pool + WAL.** Build the pool with `r2d2::Pool::builder().max_size(min(num_cpus, 32))`; extend `with_init` to also run `PRAGMA journal_mode=WAL;` (idempotent; `foreign_keys=ON` retained). Add a `Database::checkpoint_wal()` calling `PRAGMA wal_checkpoint(RESTART);`, invoked at the end of `index_directory`. Add `num_cpus` dep if not present (or use `std::thread::available_parallelism`). Prefer `std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8).min(32)` — no new dep.

**W10 — metadata indexes.** In `initialize_schema`, after the existing indexes, add:
- `CREATE INDEX IF NOT EXISTS idx_metadata_geo_time ON image_metadata(latitude, longitude, datetime_taken)`
- `CREATE INDEX IF NOT EXISTS idx_metadata_camera_time ON image_metadata(camera_model, datetime_taken)`
- `CREATE INDEX IF NOT EXISTS idx_metadata_datetime ON image_metadata(datetime_taken) WHERE datetime_taken IS NOT NULL`
Additive, idempotent; no migration system needed.

Files: `src/main.rs`, `src/database.rs`, `src/config.rs` (batch_size).
Tests: unit test the chunking helper (`chunk_pending`) — correct chunk sizes, last partial chunk, empty input; unit test that a decode failure in a chunk yields the right survivor set + failed count (pure logic extracted from the I/O).

### Commit 5 — `feat(search): configurable distance threshold and k clamp` (W18)

Add `SearchConfig { distance_threshold: f32 (default 1.3), max_k: usize (default 100) }` to `config.rs` as `Config.search` (`#[serde(default)]`, with a `Default` impl). Thread a `SearchConfig` (or its two values) through all three `search_similar_images*` methods, replacing the literal `1.3` and the `clamp(1, 100)` ceiling. Add `--threshold: f32` to the `search` subcommand (default from config); the existing `--limit` continues to set the requested count. CLI resolves: flag value if passed, else config value, else hardcoded default. Document the `[search]` section.

Files: `src/config.rs`, `src/database.rs`, `src/main.rs`.
Tests: unit test `SearchConfig` defaults + toml round-trip; test the flag→config→default resolution helper.

### Commit 6 — `feat(graphql): mutation root + favorites vertical slice` (W32)

Replace `EmptyMutation<GraphQLContext>` with a real `Mutation` root in the `Schema` type alias.

Schema/DB:
- `CREATE TABLE IF NOT EXISTS favorites (image_id INTEGER PRIMARY KEY, created_at DATETIME DEFAULT CURRENT_TIMESTAMP, FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE)` in `initialize_schema`.
- `Database::toggle_favorite(&self, path: &str) -> Result<bool>` — resolve `image_id` from the (relative) path; if a favorites row exists, delete it and return `false`; else insert and return `true`. Uses a pooled connection (`&self`).
- `Database::is_favorite(&self, path: &str) -> Result<bool>` and `Database::list_favorites(&self) -> Result<Vec<String>>` (returns relative paths; convert to the form the API/web already use).

GraphQL:
- `struct Mutation;` with `toggle_favorite(context, path: String) -> FieldResult<bool>`.
- Query additions: `favorites(context) -> FieldResult<Vec<String>>` and `is_favorite(context, path: String) -> FieldResult<bool>`.
- **Deviation from the seed's "image.isFavorite":** there is no `Image` GraphQL object exposed by a real query today (`search` is a stub returning `Vec<String>`; only `imagesByBounds` returns objects). Rather than refactor search into typed image objects in this batch, the read path is the `favorites`/`isFavorite` queries. Equivalent proof of the write path, smaller blast radius. Noted for the user.

Files: `src/graphql.rs`, `src/database.rs`, `src/context.rs` (only if a helper is needed; `db` already accessible).
Tests: unit test `toggle_favorite` flips state (insert→true, again→false) against an in-memory/temp DB if the test harness supports it; otherwise test the path→id resolution helper.

---

## Cross-cutting notes

- **No migration system** in this batch (W43 not selected): all new tables/indexes use `CREATE … IF NOT EXISTS` in `initialize_schema`, consistent with the codebase.
- **Relative-path invariant:** favorites and any path handling convert at boundaries via the existing `abs_to_relative_path`/`relative_to_abs_path`. Favorites store `image_id`, sidestepping path storage.
- **SPA is embedded** (`rust-embed` from `site/build`): the web commit must `yarn build` so `cargo build --release` embeds fresh assets. CI/verification order: `yarn build` before `cargo build`.
- **Forbidden scope creep:** do not implement tagging/albums (W36/W54/W55) — W32 is only the mutation-root + favorites slice.

## Acceptance criteria

- `cargo build --release` succeeds (with `../clipper` present); `cargo test` green.
- `cd site && yarn build && yarn lint && yarn test` all green (zero eslint warnings).
- TUI: `?` opens/closes a help overlay; zoom view shows the control hint; pagination shows `[H prev | L next]`.
- Web: searching shows a loading indicator; empty query shows idle copy; a query with no matches shows the no-results message; a failed fetch shows a visible error; results render in a masonry grid.
- CLI: model load prints the "Loading CLIP model…" spinner; `status` leads with `Database: <path>`.
- Indexing: embeds in configurable batches (default 32), skips+counts decode failures, prints `indexed/skipped/failed`; WAL enabled + checkpointed at end; the three new metadata indexes exist; pool sized to `min(cores, 32)`.
- Search: `--threshold` flag + `[search]` config override the distance threshold and k ceiling; default behavior unchanged when unset.
- GraphQL: `toggleFavorite(path)` mutation flips and persists; `favorites`/`isFavorite` queries reflect it; schema compiles with a non-empty Mutation root.

## Out of scope / deferred

- W43 migration system, W36 user-data schema beyond `favorites`, W54/W55 tagging+albums.
- GPU benchmark of W15 and live server smoke test (env-dependent; verify via tests + build).
- Strict left-to-right masonry ordering (CSS-columns tradeoff accepted).
