# whats-next batch (2026-05-31) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement 13 selected `WHATS-NEXT.md` opportunities (W1, W5, W6, W9, W10, W13, W15, W17, W18, W24, W25, W27, W32) on one branch.

**Architecture:** Rust CLI/TUI/Axum binary + embedded React SPA. Changes are grouped by surface: TUI discoverability, web search-page UX, CLI ergonomics, indexing/DB performance, search configurability, and a GraphQL mutation-root + favorites vertical slice. No bug fixes; no new migration system (additive `CREATE … IF NOT EXISTS`).

**Tech Stack:** Rust 2024 (rusqlite, r2d2, sqlite-vec, juniper, clap, indicatif, anyhow), sibling `../clipper` crate (CLIP embeddings), React 19 + Vite + TypeScript + Tailwind v4 + Apollo.

**Commit grouping:** commit per task, with surface-prefixed messages (`feat(tui)`, `feat(web)`, `feat(cli)`, `perf(index)`, `feat(search)`, `feat(graphql)`). One PR off `whats-next-batch/2026-05-31`.

**Reference:** design spec at `docs/superpowers/specs/2026-05-31-whats-next-batch-design.md`. Read it before starting.

**Build-order gotcha:** the SPA is embedded via `rust-embed` from `site/build`. Any task touching `site/` must run `cd site && yarn build` so a later `cargo build` embeds fresh assets. Run `yarn lint` (zero warnings) + `yarn test` for web tasks.

---

## Task 1: `SearchConfig` struct + defaults (W18, part 1)

**Files:**
- Modify: `src/config.rs` (Config struct ~lines 9-17; loader ~lines 55-83)
- Test: inline `#[cfg(test)] mod tests` in `src/config.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `src/config.rs`:

```rust
#[test]
fn search_config_has_sane_defaults() {
    let sc = SearchConfig::default();
    assert_eq!(sc.distance_threshold, 1.3);
    assert_eq!(sc.max_k, 100);
}

#[test]
fn config_defaults_include_search_section() {
    // A Config deserialized from empty TOML fills in search defaults.
    let cfg: Config = toml::from_str("").expect("empty toml parses");
    assert_eq!(cfg.search.distance_threshold, 1.3);
    assert_eq!(cfg.search.max_k, 100);
}

#[test]
fn config_roundtrips_search_section() {
    let mut cfg = Config::default();
    cfg.search.distance_threshold = 1.1;
    cfg.search.max_k = 50;
    let s = toml::to_string(&cfg).unwrap();
    let back: Config = toml::from_str(&s).unwrap();
    assert_eq!(back.search.distance_threshold, 1.1);
    assert_eq!(back.search.max_k, 50);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test config::tests::search_config -- --nocapture`
Expected: FAIL — `SearchConfig` / `Config.search` not defined.

- [ ] **Step 3: Implement `SearchConfig` and add it to `Config`**

In `src/config.rs`, add the struct (near the `Config` definition):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_distance_threshold")]
    pub distance_threshold: f32,
    #[serde(default = "default_max_k")]
    pub max_k: usize,
}

fn default_distance_threshold() -> f32 { 1.3 }
fn default_max_k() -> usize { 100 }

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig { distance_threshold: default_distance_threshold(), max_k: default_max_k() }
    }
}
```

Add the field to `Config`:

```rust
    #[serde(default)]
    pub search: SearchConfig,
```

If `Config` does not already `#[derive(Default)]`, add a manual or derived `Default` so `Config::default()` works (ensure `ignore_patterns` defaults to empty and `compiled_regexes` to its existing default). If a manual `Default` is cleaner given the `OnceCell` field, write it.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test config::tests -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(search): add SearchConfig (distance_threshold, max_k) to config [W18]"
```

---

## Task 2: Thread `SearchConfig` through search + `--threshold` flag (W18, part 2)

**Files:**
- Modify: `src/database.rs` (`search_similar_images` ~210-246, `search_similar_images_with_raw_blob` ~248-287, `search_similar_images_with_blob` ~288-303)
- Modify: `src/main.rs` (`Search` subcommand args ~60-81; search dispatch handler)

- [ ] **Step 1: Read the three search methods**

Read `src/database.rs:210-303`. Note each method hardcodes `distance <= 1.3` in the SQL and `limit.clamp(1, 100)`. They must accept the threshold and the max_k ceiling.

- [ ] **Step 2: Add params to the search methods**

Add two parameters (`distance_threshold: f32`, `max_k: usize`) to each of the three methods (append to the signatures to minimize churn). Replace:
- the literal `1.3` in each SQL string — build the SQL with the threshold value interpolated (it is a trusted numeric config value, not user free-text; format with `{:.6}` to avoid locale/format surprises, or bind as a parameter if the `MATCH … AND distance <= ?` form is supported by sqlite-vec here — prefer a bound parameter if it works, else interpolate the formatted float).
- `limit.clamp(1, 100)` → `limit.clamp(1, max_k)`.

Keep behavior identical when called with `(1.3, 100)`.

- [ ] **Step 3: Update all call sites**

Grep for callers: `rg "search_similar_images(_with_raw_blob|_with_blob)?\(" src/`. Update each (CLI search handler in `main.rs`, the REST handler in `src/api/search.rs`, GraphQL if any) to pass the resolved threshold + `max_k`. For non-CLI callers (API/GraphQL), pass the loaded `Config.search` values (or the existing defaults if config isn't readily threaded there — use `SearchConfig::default()` values to preserve current behavior, and leave a `// TODO(W33): thread per-request overrides` only if wiring config there is out of scope; do NOT change API behavior).

- [ ] **Step 4: Add the `--threshold` flag**

In `src/main.rs` `Search { … }` (after the existing `limit` arg), add:

```rust
    /// Max cosine distance to include (lower = stricter). Overrides config [search].distance_threshold.
    #[arg(long)]
    threshold: Option<f32>,
```

In the search handler, load `Config`, resolve `let threshold = threshold.unwrap_or(config.search.distance_threshold);` and `let max_k = config.search.max_k;`, and pass both into the search call. `--limit` continues to provide the requested count.

- [ ] **Step 5: Build + sanity test**

Run: `cargo build` then `cargo test`
Expected: compiles; existing tests still pass (default behavior unchanged).

- [ ] **Step 6: Commit**

```bash
git add src/database.rs src/main.rs src/api/search.rs
git commit -m "feat(search): configurable distance threshold + k ceiling via SearchConfig and --threshold [W18]"
```

---

## Task 3: Metadata covering indexes (W10)

**Files:**
- Modify: `src/database.rs` (`initialize_schema`, after the existing index DDL ~line 141)

- [ ] **Step 1: Add the three indexes**

In `initialize_schema`, after the existing `CREATE INDEX … idx_metadata_gps …`, add (each as its own `execute`/`execute_batch` statement, matching the surrounding style):

```sql
CREATE INDEX IF NOT EXISTS idx_metadata_geo_time ON image_metadata(latitude, longitude, datetime_taken);
CREATE INDEX IF NOT EXISTS idx_metadata_camera_time ON image_metadata(camera_model, datetime_taken);
CREATE INDEX IF NOT EXISTS idx_metadata_datetime ON image_metadata(datetime_taken) WHERE datetime_taken IS NOT NULL;
```

- [ ] **Step 2: Verify schema initializes**

Run: `cargo test` (the schema is created in `Database::new`; existing tests that open a DB will exercise it). If a focused test exists for schema init, run it.
Expected: PASS — no SQL errors.

- [ ] **Step 3: Commit**

```bash
git add src/database.rs
git commit -m "perf(index): add composite/partial metadata indexes for map + filter queries [W10]"
```

---

## Task 4: Pool sizing + WAL + checkpoint (W24)

**Files:**
- Modify: `src/database.rs` (pool construction ~47-50; add `checkpoint_wal`)
- Modify: `src/main.rs` (`index_directory` end — call checkpoint)

- [ ] **Step 1: Size the pool + enable WAL in `with_init`**

Replace the pool construction so it sets `max_size` and adds the WAL pragma alongside the existing `foreign_keys`:

```rust
let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
});
let max_size = std::thread::available_parallelism()
    .map(|n| n.get())
    .unwrap_or(8)
    .min(32) as u32;
let pool = r2d2::Pool::builder()
    .max_size(max_size)
    .build(manager)
    .with_context(|| format!("Failed to open database at {:?}", db_path))?;
```

(Keep the existing error context. `journal_mode = WAL` is persistent for the DB file; running it per-connection is idempotent and harmless.)

- [ ] **Step 2: Add `checkpoint_wal`**

Add to `impl Database`:

```rust
/// Truncate the WAL back into the main DB file. Call after a large write batch (e.g. indexing).
pub fn checkpoint_wal(&self) -> Result<()> {
    let conn = self.pool.get().context("get connection for WAL checkpoint")?;
    conn.pragma_update(None, "wal_checkpoint", "RESTART")
        .context("wal_checkpoint(RESTART)")?;
    Ok(())
}
```

(If `pragma_update` with `wal_checkpoint` misbehaves, use `conn.execute_batch("PRAGMA wal_checkpoint(RESTART);")` instead.)

- [ ] **Step 3: Call checkpoint at end of indexing**

In `src/main.rs` `index_directory`, after the summary is printed (or just before returning `Ok(())`), add:

```rust
if let Err(e) = db.checkpoint_wal() {
    tracing::warn!("WAL checkpoint failed (non-fatal): {e:#}");
}
```

- [ ] **Step 4: Build + test**

Run: `cargo build && cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/database.rs src/main.rs
git commit -m "perf(index): size r2d2 pool to cores, enable WAL, checkpoint after indexing [W24]"
```

---

## Task 5: Batch chunking helper (W15, part 1 — pure logic, TDD)

**Files:**
- Modify: `src/main.rs` (or a small new module `src/indexing.rs` if `main.rs` is large — prefer a module to keep the helper testable)
- Test: `#[cfg(test)] mod tests` next to the helper

We isolate the pure batching decision from the I/O so it can be unit-tested.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn chunks_pending_respects_batch_size() {
    let items: Vec<u32> = (0..10).collect();
    let chunks: Vec<Vec<u32>> = chunk_pending(&items, 4).map(|c| c.to_vec()).collect();
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0], vec![0, 1, 2, 3]);
    assert_eq!(chunks[2], vec![8, 9]); // last partial chunk
}

#[test]
fn chunks_pending_empty_input_yields_no_chunks() {
    let items: Vec<u32> = vec![];
    assert_eq!(chunk_pending(&items, 4).count(), 0);
}

#[test]
fn chunks_pending_clamps_zero_batch_size_to_one() {
    let items: Vec<u32> = (0..3).collect();
    // batch_size of 0 must not panic / infinite-loop; treat as 1.
    assert_eq!(chunk_pending(&items, 0).count(), 3);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test chunk_pending`
Expected: FAIL — `chunk_pending` not defined.

- [ ] **Step 3: Implement the helper**

```rust
/// Yield slices of `items` of at most `batch_size` (clamped to >= 1).
pub fn chunk_pending<T>(items: &[T], batch_size: usize) -> impl Iterator<Item = &[T]> {
    items.chunks(batch_size.max(1))
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test chunk_pending`
Expected: PASS.

- [ ] **Step 5: Add `batch_size` to config (IndexConfig)**

In `src/config.rs` add (mirroring `SearchConfig` from Task 1):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}
fn default_batch_size() -> usize { 32 }
impl Default for IndexConfig {
    fn default() -> Self { IndexConfig { batch_size: default_batch_size() } }
}
```

Add `#[serde(default)] pub index: IndexConfig,` to `Config`. Add a test mirroring Task 1 asserting default `batch_size == 32`.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/config.rs src/indexing.rs
git commit -m "perf(index): add batch chunking helper + [index].batch_size config [W15]"
```

---

## Task 6: Batch the indexing embedding loop (W15, part 2)

**Files:**
- Modify: `src/main.rs` (`index_directory` ~226-440: the per-image loop ~343-428)
- Modify: `src/database.rs` (add `insert_images_batch`)

This is the largest change. Read `src/main.rs:226-440` fully before editing.

- [ ] **Step 1: Add a batch insert to `Database`**

Mirror the existing `insert_image` transaction pattern. Add:

```rust
/// Insert many (relative_path, hash, normalized_embedding) rows in one transaction.
/// Mirrors insert_image's per-row writes (images row + image_vectors row).
pub fn insert_images_batch(&mut self, rows: &[(String, String, Vec<f32>)]) -> Result<()> {
    if rows.is_empty() { return Ok(()); }
    let mut conn = self.pool.get().context("get connection for batch insert")?;
    let tx = conn.transaction()?;
    for (path, hash, embedding) in rows {
        // Replicate exactly what insert_image does per row (INSERT OR REPLACE into images,
        // fetch rowid, INSERT into image_vectors with the embedding bytes). Read insert_image
        // (~src/database.rs:146) and reproduce its statements here against `tx`.
    }
    tx.commit()?;
    Ok(())
}
```

Implementer: open `insert_image` and replicate its exact SQL/binding logic inside the loop against `tx`. Do not invent column names — copy them.

- [ ] **Step 2: Restructure the loop to two phases**

Replace the single per-image loop with:

1. **Collect pending:** iterate `image_files`; for each, compute `oshash` and run the existing already-indexed check (`db.get_image_hash` / whatever the current skip uses). Push `(abs_path, relative_path, hash)` for not-yet-indexed images to a `pending` vec; increment `skipped_count` for already-indexed.
2. **Batch embed + insert:** for each `chunk in chunk_pending(&pending, batch_size)`:
   - Decode each path to a `DynamicImage` via `image::open(abs_path)` (or the crate already used). On error: `tracing::warn!`, `failed_count += 1`, skip that one. Keep a parallel vec of `(relative_path, hash)` for the successfully-decoded images and a `Vec<DynamicImage>` of the images, index-aligned.
   - If the survivor image vec is non-empty, call `model.get_image_embeddings_from_dynamic(images)`. On a whole-batch error: `tracing::warn!`, add the chunk's survivor count to `failed_count`, continue.
   - `normalize_vector` each returned embedding (match current single-image normalization), build `rows: Vec<(String,String,Vec<f32>)>` = `(relative_path, hash, normalized_embedding)`, call `db.insert_images_batch(&rows)`; `indexed_count += rows.len()`.
   - Advance the progress bar by the number of images processed in the chunk.
3. **Metadata:** keep the existing metadata extraction + end-of-run backfill. Extraction may stay per-image; ensure it runs for the newly-indexed images (reuse the current code path — e.g. extract during/after insert as today).

`batch_size` resolves from a new `--batch-size: Option<usize>` flag on `index` (default from `config.index.batch_size`).

- [ ] **Step 3: Update the summary**

Print `indexed / skipped / failed` (the bug-fixed backfill-failure counting from commit 370994/O3 already exists — keep it). Ensure counts are accurate.

- [ ] **Step 4: Build + test**

Run: `cargo build && cargo test`
Expected: compiles and existing tests pass. (No live indexing run — no model assumed.)

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/database.rs
git commit -m "perf(index): batch CLIP embedding (configurable size, skip+count decode failures) [W15]"
```

---

## Task 7: CLI model-load spinner + status DB path (W9, W17)

**Files:**
- Modify: `src/main.rs` (model load ~254 and the search-path model load; `show_status` ~615-642)

- [ ] **Step 1: Spinner around model load**

Wherever `ClipEmbedder::new(...)` is called (index path ~254, and the search handler), wrap it (respecting the existing `quiet`/TTY suppression):

```rust
let spinner = if quiet { ProgressBar::hidden() } else {
    let pb = ProgressBar::new_spinner();
    pb.set_message("Loading CLIP model… (this may take a minute on first use)");
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb
};
let model = ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;
spinner.finish_and_clear();
```

For the search handler (which may not have a `quiet` var), gate on whether output is interactive or just always show the spinner to stderr (ProgressBar writes to stderr by default, so it won't pollute `--short` stdout). Keep it simple and consistent.

- [ ] **Step 2: Status DB path first**

In `show_status`, make the first printed line lead with the resolved path (it already prints `Database location: {}` — promote/reorder so the `Database:`-style path line is the first thing, before the header or right after a short header). Minimal acceptable change: ensure the resolved `db_path` is clearly the lead line. Example:

```rust
println!("Database: {}", db_path.display());
println!("========================================");
// existing stats below
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): model-load progress spinner; status leads with resolved DB path [W9,W17]"
```

---

## Task 8: TUI keybindings source-of-truth + help overlay (W1)

**Files:**
- Modify: `src/tui/app.rs` (App struct ~36-81; key handler ~260-310)
- Modify: `src/tui/ui.rs` (render pipeline)
- Create (optional): `src/tui/widget/help.rs`

- [ ] **Step 1: Write the failing test for the bindings list**

Add near the helper (in `app.rs` or `help.rs`) a `#[cfg(test)]` test:

```rust
#[test]
fn keybindings_help_lists_core_keys() {
    let lines = keybindings_help();
    assert!(!lines.is_empty());
    let joined = lines.join(" ");
    for key in ["e", "h/j/k/l", "H/L", "1-9", "Enter", "Esc", "?", "q"] {
        assert!(joined.contains(key), "help should mention {key}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test keybindings_help`
Expected: FAIL — `keybindings_help` not defined.

- [ ] **Step 3: Implement the helper**

```rust
/// Single source of truth for the help overlay (and any docs). One entry per binding.
pub fn keybindings_help() -> Vec<String> {
    vec![
        "e          edit search".into(),
        "h/j/k/l    move focus".into(),
        "H/L        previous / next page".into(),
        "1-9        zoom that image".into(),
        "Enter      zoom focused image".into(),
        "Esc        close zoom / help".into(),
        "scroll     zoom in/out (in zoom view)".into(),
        "right-click reset zoom".into(),
        "?          toggle this help".into(),
        "q / Ctrl-C quit".into(),
    ]
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test keybindings_help`
Expected: PASS.

- [ ] **Step 5: Add `show_help` state + toggle**

Add `pub show_help: bool` to `App`; init `false` in `App::new`. In `handle_key_events` Normal mode: bind `KeyCode::Char('?')` to `self.show_help = !self.show_help;`. While `show_help` is true, make `Esc` and `?` close it (set false) and swallow other navigation keys (return early) so the overlay is modal.

- [ ] **Step 6: Render the overlay**

In `ui.rs`, after the normal render, when `self.show_help`, draw a centered modal: compute a centered `Rect`, `Clear` it, then render a bordered `Paragraph` built from `keybindings_help()` joined by `\n`, titled "Keybindings (? to close)". (Optionally factor the centered-rect math into `widget/help.rs`.)

- [ ] **Step 7: Build**

Run: `cargo build` (with `tui` feature, the default).
Expected: compiles.

- [ ] **Step 8: Commit**

```bash
git add src/tui/
git commit -m "feat(tui): '?' help overlay backed by a single keybindings source [W1]"
```

---

## Task 9: TUI zoom status line + pagination hints (W13, W25)

**Files:**
- Modify: `src/tui/ui.rs` (render pipeline; `render_pagination` ~19-35)

- [ ] **Step 1: Pagination hints (W25)**

In `render_pagination`, change the format string to:

```rust
let page_info = format!(
    "Page {}/{} ({} results) [H prev | L next]",
    self.page + 1,
    total_pages.max(1),
    search_result.result_count
);
```

- [ ] **Step 2: Zoom status line (W13)**

Add a `render_zoom_status(&self, area, buf)` that, when `self.zoomed_image.is_some()`, renders a one-line hint `scroll to zoom | right-click to reset | ESC to close` at the bottom of the zoom area (use `self.zoomed_image_rect` to place it, or the bottom row of the main area when zoomed). Call it from the main render path inside the zoom branch.

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add src/tui/ui.rs
git commit -m "feat(tui): zoom-view control hints + pagination keybinding hints [W13,W25]"
```

---

## Task 10: `favorites` schema + Database methods (W32, part 1)

**Files:**
- Modify: `src/database.rs` (`initialize_schema`; add `toggle_favorite`, `is_favorite`, `list_favorites`)

- [ ] **Step 1: Add the table**

In `initialize_schema`, add:

```sql
CREATE TABLE IF NOT EXISTS favorites (
    image_id INTEGER PRIMARY KEY,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE
);
```

- [ ] **Step 2: Write the failing test**

Use the existing test helper for an in-memory/temp DB (find how other `database.rs` tests open a `Database` — reuse that). Add:

```rust
#[test]
fn toggle_favorite_flips_state() {
    let db = /* existing test-db constructor */;
    // Insert an image so the FK resolves. Reuse the existing insert helper/test fixture.
    // Assume relative path "a/b.jpg" was inserted with some hash + embedding.
    let p = "a/b.jpg";
    assert_eq!(db.is_favorite(p).unwrap(), false);
    assert_eq!(db.toggle_favorite(p).unwrap(), true);  // now favorited
    assert_eq!(db.is_favorite(p).unwrap(), true);
    assert_eq!(db.list_favorites().unwrap(), vec![p.to_string()]);
    assert_eq!(db.toggle_favorite(p).unwrap(), false); // toggled off
    assert_eq!(db.is_favorite(p).unwrap(), false);
}
```

Implementer: adapt the fixture to however `database.rs` tests construct a DB and insert an image (read the existing tests first; reuse `insert_image`).

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test toggle_favorite_flips_state`
Expected: FAIL — methods undefined.

- [ ] **Step 4: Implement the three methods**

```rust
/// Toggle favorite for the image at `relative_path`. Returns the new state (true = now favorited).
pub fn toggle_favorite(&self, relative_path: &str) -> Result<bool> {
    let conn = self.pool.get().context("get connection")?;
    let image_id: i64 = conn
        .query_row("SELECT id FROM images WHERE path = ?1", [relative_path], |r| r.get(0))
        .with_context(|| format!("no indexed image at {relative_path}"))?;
    let exists: bool = conn
        .query_row("SELECT 1 FROM favorites WHERE image_id = ?1", [image_id], |_| Ok(()))
        .optional()?
        .is_some();
    if exists {
        conn.execute("DELETE FROM favorites WHERE image_id = ?1", [image_id])?;
        Ok(false)
    } else {
        conn.execute("INSERT INTO favorites (image_id) VALUES (?1)", [image_id])?;
        Ok(true)
    }
}

pub fn is_favorite(&self, relative_path: &str) -> Result<bool> {
    let conn = self.pool.get().context("get connection")?;
    let id: Option<i64> = conn
        .query_row("SELECT id FROM images WHERE path = ?1", [relative_path], |r| r.get(0))
        .optional()?;
    let Some(image_id) = id else { return Ok(false); };
    Ok(conn
        .query_row("SELECT 1 FROM favorites WHERE image_id = ?1", [image_id], |_| Ok(()))
        .optional()?
        .is_some())
}

pub fn list_favorites(&self) -> Result<Vec<String>> {
    let conn = self.pool.get().context("get connection")?;
    let mut stmt = conn.prepare(
        "SELECT i.path FROM favorites f JOIN images i ON i.id = f.image_id ORDER BY f.created_at DESC",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
```

Ensure `use rusqlite::OptionalExtension;` is in scope for `.optional()`.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test toggle_favorite_flips_state`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/database.rs
git commit -m "feat(graphql): favorites table + toggle/is/list Database methods [W32]"
```

---

## Task 11: GraphQL Mutation root + favorites resolvers (W32, part 2)

**Files:**
- Modify: `src/graphql.rs` (Query root ~24-82; Schema alias ~84)

- [ ] **Step 1: Add a `Mutation` root**

```rust
pub struct Mutation;

#[juniper::graphql_object(Context = GraphQLContext)]
impl Mutation {
    /// Toggle the favorite flag for an image (by its relative path). Returns the new state.
    #[graphql(name = "toggleFavorite")]
    pub fn toggle_favorite(context: &GraphQLContext, path: String) -> FieldResult<bool> {
        Ok(context.db.toggle_favorite(&path)?)
    }
}
```

- [ ] **Step 2: Add favorites read queries to `Query`**

```rust
    #[graphql(name = "favorites")]
    pub fn favorites(context: &GraphQLContext) -> FieldResult<Vec<String>> {
        Ok(context.db.list_favorites()?)
    }

    #[graphql(name = "isFavorite")]
    pub fn is_favorite(context: &GraphQLContext, path: String) -> FieldResult<bool> {
        Ok(context.db.is_favorite(&path)?)
    }
```

- [ ] **Step 3: Swap the Schema alias**

```rust
pub type Schema = RootNode<Query, Mutation, EmptySubscription<GraphQLContext>>;
```

Update the `Schema::new(...)` construction wherever the schema is built (grep `RootNode::new` / `Schema::new` in `src/routes.rs` / `src/graphql.rs`) to pass a `Mutation` instance instead of `EmptyMutation::new()`.

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: compiles; juniper macro expands cleanly.

- [ ] **Step 5: Commit**

```bash
git add src/graphql.rs src/routes.rs
git commit -m "feat(graphql): real Mutation root with toggleFavorite + favorites/isFavorite queries [W32]"
```

---

## Task 12: Web loading + empty/error states (W5, W6)

**Files:**
- Modify: `site/src/page/Images.tsx`
- Test (optional): `site/src/page/Images.test.tsx` (vitest) for an extracted pure helper

- [ ] **Step 1: Extract + test the view-state selector (pure logic)**

In `Images.tsx` (or a small sibling `searchViewState.ts`), define:

```typescript
export type SearchViewState = 'idle' | 'loading' | 'error' | 'empty' | 'results';

export function selectViewState(args: {
  hasSearched: boolean; loading: boolean; error: string | null; resultCount: number;
}): SearchViewState {
  if (args.loading) return 'loading';
  if (args.error) return 'error';
  if (!args.hasSearched) return 'idle';
  if (args.resultCount === 0) return 'empty';
  return 'results';
}
```

Add `site/src/page/searchViewState.test.ts`:

```typescript
import { describe, it, expect } from 'vitest';
import { selectViewState } from './searchViewState';

describe('selectViewState', () => {
  it('idle before any search', () => {
    expect(selectViewState({ hasSearched: false, loading: false, error: null, resultCount: 0 })).toBe('idle');
  });
  it('loading wins', () => {
    expect(selectViewState({ hasSearched: true, loading: true, error: null, resultCount: 5 })).toBe('loading');
  });
  it('error after load', () => {
    expect(selectViewState({ hasSearched: true, loading: false, error: 'boom', resultCount: 0 })).toBe('error');
  });
  it('empty when searched with no results', () => {
    expect(selectViewState({ hasSearched: true, loading: false, error: null, resultCount: 0 })).toBe('empty');
  });
  it('results when matches exist', () => {
    expect(selectViewState({ hasSearched: true, loading: false, error: null, resultCount: 3 })).toBe('results');
  });
});
```

- [ ] **Step 2: Run the test (fails, then passes once helper exists)**

Run: `cd site && yarn test --run searchViewState`
Expected: PASS after the helper is added.

- [ ] **Step 3: Wire state into the component**

Add `loading`, `error`, `hasSearched` state. In `getImages`:

```typescript
const getImages = async (q: string) => {
  setLoading(true);
  setError(null);
  setHasSearched(true);
  try {
    const response = await fetch(`/api/v1/search/${encodeURIComponent(q)}`);
    if (!response.ok) throw new Error(`Search failed (${response.status})`);
    const data = await response.json();
    setImages(data || []);
  } catch (e) {
    setError(e instanceof Error ? e.message : 'Search failed');
    setImages([]);
  } finally {
    setLoading(false);
  }
};
```

Remove the `console.log('Images fetched', …)`. In the effect that reads the query param, set `hasSearched(false)` and clear results when the query is empty (idle state).

- [ ] **Step 4: Render the states**

Above the grid, render based on `selectViewState(...)`:
- `idle`: muted text "Enter a search query to find images."
- `loading`: a spinner / "Searching…" (also disable the input while loading).
- `error`: a red banner `Error: {error}`.
- `empty`: `No images found for "{query}".`
- `results`: render the grid (Task 13 styles it).

- [ ] **Step 5: Lint + build + test**

Run: `cd site && yarn lint && yarn test --run && yarn build`
Expected: zero eslint warnings, tests pass, build succeeds.

- [ ] **Step 6: Commit**

```bash
git add site/src/page/
git commit -m "feat(web): loading, idle, no-results, and visible error states for search [W5,W6]"
```

---

## Task 13: Web masonry grid (W27)

**Files:**
- Modify: `site/src/page/Images.tsx` (grid container ~line 104)

- [ ] **Step 1: Swap the grid to CSS columns masonry**

Replace the results container class:

```tsx
<div className="columns-2 gap-4 sm:columns-3 lg:columns-4 p-4">
  {images.map((image) => (
    <div key={image[0]} className="mb-4 break-inside-avoid">
      <LightboxViewer image={image} handleClick={handleClick} />
    </div>
  ))}
</div>
```

(If `LightboxViewer`'s root has a fixed width/height that breaks column flow, ensure its image is `w-full h-auto`; adjust `LightboxViewer` minimally if needed.)

Note (already flagged to user): CSS columns order top-to-bottom per column, not strict left-to-right by relevance. Accepted tradeoff.

- [ ] **Step 2: Lint + build**

Run: `cd site && yarn lint && yarn build`
Expected: zero warnings; build succeeds.

- [ ] **Step 3: Commit**

```bash
git add site/src/page/
git commit -m "feat(web): masonry/waterfall layout for the results grid [W27]"
```

---

## Task 14: Full verification

**Files:** none (verification only)

- [ ] **Step 1: SPA build first (embedded asset freshness)**

Run: `cd site && yarn install && yarn lint && yarn test --run && yarn build && cd ..`
Expected: all green; `site/build/` regenerated.

- [ ] **Step 2: Rust build + tests**

Run: `cargo test && cargo build --release`
Expected: all tests pass; release binary builds (requires `../clipper` present).

- [ ] **Step 3: Confirm acceptance criteria**

Re-read the spec's "Acceptance criteria" and confirm each is satisfied by the committed code (static confirmation; no live indexing/server run per the agreed verification scope). Note any criterion that could not be confirmed and why.

- [ ] **Step 4: (No commit)** — verification only. Proceed to final review + branch finish.

---

## Self-review (author checklist, completed)

- **Spec coverage:** W1→T8, W5→T12, W6→T12, W9→T7, W10→T3, W13→T9, W15→T5+T6, W17→T7, W18→T1+T2, W24→T4, W25→T9, W27→T13, W32→T10+T11. All 13 mapped. Verification in T14.
- **Placeholders:** code shown for each change; where exact replication of an existing routine is required (insert_image internals, test DB fixture), the step names the source to copy rather than inventing column names — deliberate, to avoid guessing schema. Acceptable.
- **Type consistency:** `SearchConfig{distance_threshold,max_k}`, `IndexConfig{batch_size}`, `chunk_pending`, `insert_images_batch`, `toggle_favorite/is_favorite/list_favorites`, `keybindings_help`, `selectViewState` used consistently across tasks.
