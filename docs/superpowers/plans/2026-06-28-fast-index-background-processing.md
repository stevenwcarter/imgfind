# Fast Index + Background Processing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `imgfind index` into a fast row-populating pass and a resumable `process` pass (embeddings + thumbnails + EXIF), runnable from the CLI or as an auto-starting, pausable background job in `imgfind-gui` with a `\`-toggled status panel.

**Architecture:** A new `src/processing.rs` engine owns the unit of work and the "what's unprocessed" queries; both the CLI `process` command and a dedicated GUI `process-worker` thread drive it. New thin DB methods add row-only insert and missing-embedding queries. Embeddings are generated from the persisted 300px thumbnail so a RAW file is decoded once.

**Tech Stack:** Rust 2024, turso (async SQLite) via `block_on`, clipper (CLIP), Slint 1.16, anyhow, tracing.

## Global Constraints

- Rust edition 2024; all Rust coding goes through the `rust-developer` agent; code must be clippy-clean and `cargo fmt --all`-clean.
- No schema migration: `LATEST_MIGRATION_VERSION` stays 5. An `images` row with no vector/thumbnail/metadata row is a valid intermediate state.
- Embeddings are L2-normalized via `normalize_vector` before storage; dimension is per active model (`vectors_table()` resolves it).
- Embedding source for all *new* embeddings is the **identity** 300px thumbnail (`ThumbnailSpec::ScaleSize(ThumbnailSize(300))`), decoded from its JPEG bytes.
- GUI: single heavy writer (the process-worker); writes go through `imgfind::block_on`; UI updates from worker threads via `slint::invoke_from_event_loop`. Chord keys suppressed while typing. Per the Slint-font memory, avoid multi-byte symbol glyphs (tofu risk) — use ASCII/Latin-1.
- Processing phase order is fixed: **300px thumbnails → embeddings → 512/2048 thumbnails**; EXIF metadata backfill rides along in the 300px phase.
- Errors use `anyhow` with `Context`/`with_context`. Logging via `tracing`.

---

### Task 1: DB methods for row-only insert + missing-embedding queries

**Files:**
- Modify: `src/database.rs` (add methods near `insert_images_batch` ~749 and `is_image_indexed` ~834)
- Test: `src/database.rs` (a `#[cfg(test)]` module already exists in the crate's test conventions; co-locate tests there or in the existing tests module)

**Interfaces:**
- Consumes: existing `vectors_table()`, `col_i64`, `to_le_bytes`, `Value`, `AbsolutePath`, `self.parent_dir`.
- Produces:
  - `async fn insert_image_rows_batch(&self, rows: &[(String, String)]) -> Result<()>` — `(rel_path, hash)` rows, no vector.
  - `async fn image_row_exists(&self, path: &AbsolutePath, hash: &str) -> Result<bool>`
  - `async fn count_images_without_embedding(&self) -> Result<usize>`
  - `async fn get_images_without_embedding(&self, limit: usize) -> Result<Vec<(i64, AbsolutePath, String)>>` — `(image_id, abs_path, hash)`
  - `async fn set_image_embedding(&self, image_id: i64, embedding: &[f32]) -> Result<()>`

- [ ] **Step 1: Write failing tests**

Add tests (temp DB helper already used elsewhere in `database.rs` tests — follow the existing pattern for constructing a `Database` over a tempdir and inserting an image). Cover:

```rust
#[tokio::test]
async fn row_only_insert_has_no_embedding() {
    let (db, _tmp) = test_db().await; // existing helper pattern
    db.insert_image_rows_batch(&[("a.jpg".into(), "hash1".into())]).await.unwrap();
    // present as a row...
    let abs = db.parent_dir.join("a.jpg");
    assert!(db.image_row_exists(&AbsolutePath(abs.clone()), "hash1").await.unwrap());
    // ...but not "indexed" (no embedding) and counted as missing-embedding
    assert!(!db.is_image_indexed(&AbsolutePath(abs), "hash1").await.unwrap());
    assert_eq!(db.count_images_without_embedding().await.unwrap(), 1);
}

#[tokio::test]
async fn set_embedding_flips_missing_count_to_zero() {
    let (db, _tmp) = test_db().await;
    db.insert_image_rows_batch(&[("a.jpg".into(), "h".into())]).await.unwrap();
    let missing = db.get_images_without_embedding(10).await.unwrap();
    assert_eq!(missing.len(), 1);
    let (id, _abs, _hash) = missing[0].clone();
    let dim = db.active_model().await.unwrap().dimension as usize;
    db.set_image_embedding(id, &vec![0.0_f32; dim]).await.unwrap();
    assert_eq!(db.count_images_without_embedding().await.unwrap(), 0);
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --lib database::tests::row_only_insert_has_no_embedding database::tests::set_embedding_flips_missing_count_to_zero 2>&1 | tail -20`
Expected: FAIL (methods not found).

- [ ] **Step 3: Implement the methods**

```rust
/// Insert `(rel_path, hash)` rows only — no embedding. Used by the fast `index`
/// pass; the embedding is filled in later by `process`/the GUI background job.
pub async fn insert_image_rows_batch(&self, rows: &[(String, String)]) -> Result<()> {
    if rows.is_empty() { return Ok(()); }
    let mut conn = self.pool.get().await.context("get connection for row insert")?;
    let tx = conn.transaction().await?;
    for (rel_path_str, hash) in rows {
        tx.execute(
            "INSERT INTO images (path, hash) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET hash = excluded.hash",
            (rel_path_str.clone(), hash.clone()),
        ).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Whether `(path, hash)` exists in `images`, independent of any embedding.
/// The fast-index dedup predicate (cf. `is_image_indexed`, which also requires a vector).
pub async fn image_row_exists(&self, path: &AbsolutePath, hash: &str) -> Result<bool> {
    let rel = path.to_relative(&self.parent_dir)
        .with_context(|| format!("relativize {}", path.as_str()))?;
    let conn = self.pool.get().await.context("conn for image_row_exists")?;
    let mut rows = conn.query(
        "SELECT COUNT(*) FROM images WHERE path = ?1 AND hash = ?2",
        (rel.as_str().into_owned(), hash.to_string()),
    ).await?;
    let row = rows.next().await?.context("COUNT returned no row")?;
    Ok(col_i64(&row, 0, "count")? > 0)
}

pub async fn count_images_without_embedding(&self) -> Result<usize> {
    let vt = self.vectors_table().await?;
    let conn = self.pool.get().await.context("conn for count_without_embedding")?;
    let sql = format!(
        "SELECT COUNT(*) FROM images i \
         LEFT JOIN {vt} v ON v.image_id = i.id WHERE v.image_id IS NULL"
    );
    let mut rows = conn.query(&sql, ()).await?;
    let row = rows.next().await?.context("COUNT returned no row")?;
    Ok(col_i64(&row, 0, "count")? as usize)
}

pub async fn get_images_without_embedding(
    &self, limit: usize,
) -> Result<Vec<(i64, AbsolutePath, String)>> {
    let vt = self.vectors_table().await?;
    let conn = self.pool.get().await.context("conn for get_without_embedding")?;
    let sql = format!(
        "SELECT i.id, i.path, i.hash FROM images i \
         LEFT JOIN {vt} v ON v.image_id = i.id WHERE v.image_id IS NULL LIMIT ?1"
    );
    let mut rows = conn.query(&sql, (limit as i64,)).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let id = col_i64(&row, 0, "id")?;
        let rel: String = row.get_value(1)?.as_text().context("path text")?.to_string();
        let hash: String = row.get_value(2)?.as_text().context("hash text")?.to_string();
        let abs = relative_to_abs_path(&self.parent_dir, &rel); // existing helper; adapt to its real signature
        out.push((id, abs, hash));
    }
    Ok(out)
}

pub async fn set_image_embedding(&self, image_id: i64, embedding: &[f32]) -> Result<()> {
    let vt = self.vectors_table().await?;
    let mut conn = self.pool.get().await.context("conn for set_image_embedding")?;
    let tx = conn.transaction().await?;
    tx.execute(&format!("DELETE FROM {vt} WHERE image_id = ?1"), (image_id,)).await?;
    tx.execute(
        &format!("INSERT INTO {vt} (image_id, embedding) VALUES (?1, ?2)"),
        (Value::Integer(image_id), Value::Blob(to_le_bytes(embedding))),
    ).await?;
    tx.commit().await?;
    Ok(())
}
```

Note: match the exact row-reading idiom already used in `database.rs` (e.g. how `get_images_without_thumbnails` reads `path`/`hash` columns and builds `AbsolutePath`). Reuse that exact pattern rather than the sketch above if it differs.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test --lib database:: 2>&1 | tail -20`
Expected: PASS. Then `cargo clippy --all-targets 2>&1 | tail -5` clean, `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add src/database.rs
git commit -m "feat(db): row-only insert + missing-embedding queries"
```

---

### Task 2: Make `imgfind index` fast (rows only)

**Files:**
- Modify: `src/main.rs` `index_directory` (382-728) and any `--no-thumbnails` help text in the `Commands::Index` definition (~37)
- Test: `src/main.rs` or `tests/` — an integration-style test driving `index_directory` over a temp dir of fixture images

**Interfaces:**
- Consumes: `Database::insert_image_rows_batch`, `Database::image_row_exists` (Task 1).
- Produces: a fast `index_directory` that writes only `images` rows.

- [ ] **Step 1: Write failing test**

Use the existing fixture images under the repo's test assets (grep for how other tests obtain sample images; reuse that). Test that after `index_directory`:

```rust
#[test]
fn fast_index_writes_rows_without_vectors_or_thumbnails() {
    // build temp DB rooted at a dir containing N fixture images (existing helper)
    // call index_directory(&mut db, dir, true, true, None, false, false)
    let total = imgfind::block_on(db.get_image_count()).unwrap();
    assert_eq!(total, N);
    assert_eq!(imgfind::block_on(db.count_images_without_embedding()).unwrap(), N);
    assert_eq!(imgfind::block_on(db.count_images_without_thumbnails(ThumbnailSize(300))).unwrap(), N);
}
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test fast_index_writes_rows 2>&1 | tail -20`
Expected: FAIL (current `index_directory` embeds + thumbnails).

- [ ] **Step 3: Rewrite `index_directory`**

Reduce to: walk (existing 435-476) → per file compute `oshash` → skip when `db.image_row_exists(abs, hash)` is true (unless `reindex`) → collect `(rel_path, hash)` → `db.insert_image_rows_batch` in chunks of `batch_size`. Remove the CLIP model load (414-426), the embed/normalize/insert-vector batch (552-671), the EXIF inline extraction, the metadata backfill, and the thumbnail phase (705-721). Keep `checkpoint_wal()` at the end. Keep `--no-thumbnails`/`--reindex` flags accepted; mark `--no-thumbnails` as a deprecated no-op in help text. Emit a final line suggesting `imgfind process` (e.g. `info!("Indexed {n} files. Run `imgfind process` (or open the GUI) to generate embeddings + thumbnails.")`).

- [ ] **Step 4: Run test, verify pass**

Run: `cargo test fast_index 2>&1 | tail -20`
Expected: PASS. `cargo clippy --all-targets` clean; `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(index): fast row-only index; defer heavy work to process"
```

---

### Task 3: Processing engine (`src/processing.rs`)

**Files:**
- Create: `src/processing.rs`
- Modify: `src/lib.rs` (add `pub mod processing;`)
- Test: `src/processing.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: Task 1 DB methods; `thumbnail::{generate_missing_thumbnails_batch, get_or_generate_thumbnail}`; `clipper::ClipEmbedder`; `normalize_vector`; `ThumbnailSize`/`ThumbnailSpec`; `decode` via the `image` crate to turn 300px JPEG bytes into a `DynamicImage`.
- Produces:
  - `enum ProcessPhase { Thumbnails300, Embeddings, FullThumbnails }`
  - `struct ProcessCounts { thumbs300: usize, embeddings: usize, full_thumbs: usize }` with `fn total_remaining(&self) -> usize` and `fn next_phase(&self) -> Option<ProcessPhase>` (300 → embeddings → full, skipping drained phases).
  - `async fn counts(db: &Database) -> Result<ProcessCounts>`
  - `fn embed_one_from_thumbnail(db: &Database, embedder: &ClipEmbedder, image_id: i64, abs_path: &str, hash: &str) -> Result<()>` (sync; uses `block_on` internally, mirroring `thumbnail.rs`)
  - `fn process_embeddings_batch(db: &Database, embedder: &ClipEmbedder, count: usize) -> Result<usize>`
  - `fn process_next_batch(db: &mut Database, embedder: Option<&ClipEmbedder>, phase: ProcessPhase, batch: usize) -> Result<usize>`
  - `fn run_to_completion(db: &mut Database, embedder: &ClipEmbedder, batch: usize, sizes: &[ThumbnailSize], with_embeddings: bool, mut progress: impl FnMut(ProcessPhase, usize)) -> Result<()>`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn next_phase_walks_in_order_then_none() {
    let c = ProcessCounts { thumbs300: 0, embeddings: 0, full_thumbs: 0 };
    assert_eq!(c.next_phase(), None);
    let c = ProcessCounts { thumbs300: 0, embeddings: 3, full_thumbs: 5 };
    assert_eq!(c.next_phase(), Some(ProcessPhase::Embeddings));
    let c = ProcessCounts { thumbs300: 2, embeddings: 3, full_thumbs: 5 };
    assert_eq!(c.next_phase(), Some(ProcessPhase::Thumbnails300));
    let c = ProcessCounts { thumbs300: 0, embeddings: 0, full_thumbs: 4 };
    assert_eq!(c.next_phase(), Some(ProcessPhase::FullThumbnails));
}

#[test]
fn embeddings_batch_fills_missing_vectors() {
    // temp DB with K fixture images already given 300px thumbnails (call the
    // thumbnail batcher first, or get_or_generate_thumbnail per image).
    // load a real embedder via ClipEmbedder::from_model(default, true /*cpu*/).
    // assert count_images_without_embedding == K, run process_embeddings_batch,
    // assert it returns K and the count is now 0, and each stored vector has the
    // active model's dimension and unit L2 norm (~1.0).
}
```

(The embeddings test loads a real CPU embedder; keep K small, e.g. 2 fixtures. If model download in CI is a concern, gate this test behind the same mechanism existing embedder tests use — grep for how current tests construct `ClipEmbedder`.)

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test --lib processing:: 2>&1 | tail -20`
Expected: FAIL (module/methods absent).

- [ ] **Step 3: Implement `src/processing.rs`**

`next_phase` returns the first non-zero phase in fixed order. `counts` calls `count_images_without_thumbnails(ThumbnailSize(300))`, `count_images_without_embedding()`, and the sum of `count_images_without_thumbnails` for 512+2048. `embed_one_from_thumbnail`:

```rust
pub fn embed_one_from_thumbnail(
    db: &Database, embedder: &ClipEmbedder, image_id: i64, abs_path: &str, hash: &str,
) -> Result<()> {
    let jpeg = thumbnail::get_or_generate_thumbnail(
        db, abs_path, hash, ThumbnailSize(300),
    ).context("get 300px thumbnail for embedding")?;
    let dynimg = image::load_from_memory(&jpeg).context("decode 300px thumbnail")?;
    let mut vecs = embedder
        .get_image_embeddings_from_dynamic(&[dynimg])
        .context("embed 300px thumbnail")?;
    let mut emb = vecs.pop().context("embedder returned no vector")?;
    crate::normalize_vector(&mut emb);
    block_on(db.set_image_embedding(image_id, &emb)).context("store embedding")?;
    Ok(())
}
```

(Confirm `get_image_embeddings_from_dynamic`'s exact argument/return type against `../clipper/src/lib.rs:199` and adapt — it may take a `Vec<DynamicImage>` and return `Result<Vec<Vec<f32>>>`.) `process_embeddings_batch` pulls `get_images_without_embedding(count)` and loops `embed_one_from_thumbnail`, logging+skipping per-image errors (return count of successes; a fully-failing batch makes zero progress and the caller's loop guard stops — mirror `run_until_complete`'s zero-progress guard). `process_next_batch` dispatches: `Thumbnails300`/`FullThumbnails` → `thumbnail::generate_missing_thumbnails_batch(db, size, batch)` (per size); `Embeddings` → `process_embeddings_batch`. `run_to_completion` loops `counts` → `next_phase` → `process_next_batch`, with a zero-progress guard per phase, invoking `progress` between batches; backfill EXIF via the existing `extract_missing_metadata` after the 300px phase drains.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test --lib processing:: 2>&1 | tail -20`
Expected: PASS. Clippy clean; `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add src/processing.rs src/lib.rs
git commit -m "feat(processing): shared engine (counts, phases, embed-from-thumbnail)"
```

---

### Task 4: `imgfind process` CLI command

**Files:**
- Modify: `src/main.rs` (add `Commands::Process { .. }` to the enum ~35-181 and a match arm in the dispatcher)

**Interfaces:**
- Consumes: `processing::run_to_completion`, `models::ensure_and_activate_model` + `ClipEmbedder::from_model` (mirror `search_images`'s model load at 791-798), `GUI_THUMBNAIL_SIZES`.

- [ ] **Step 1: Add the command + arm (manual smoke test)**

Add:

```rust
/// Generate embeddings + thumbnails for indexed images (resumable).
Process {
    /// Per-batch size.
    #[arg(long, default_value_t = 64)]
    count: usize,
    /// Skip embedding generation (thumbnails only).
    #[arg(long)]
    no_embeddings: bool,
    /// Directory whose DB to use (walk-up/global otherwise).
    #[arg(short, long)]
    dir: Option<String>,
},
```

Match arm: resolve DB (mirror `Thumbnails` at 287), load the active model embedder (mirror 414-426/791-798) unless `no_embeddings`, then:

```rust
imgfind::processing::run_to_completion(
    &mut db, &embedder, count,
    &[ThumbnailSize(300), ThumbnailSize(512), ThumbnailSize(2048)],
    !no_embeddings,
    |phase, n| println!("{phase:?}: processed {n}"),
)?;
```

(Order inside `run_to_completion` is fixed regardless of `sizes` ordering; pass the three GUI sizes.)

- [ ] **Step 2: Build + smoke**

Run: `cargo build 2>&1 | tail -5` then on a scratch library: `imgfind index <dir> && imgfind process && imgfind status`. Expected: `process` drains all phases; `status` reports zero remaining. Clippy clean; `cargo fmt --all`.

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): add `imgfind process` command"
```

---

### Task 5: `imgfind status` reports unprocessed counts

**Files:**
- Modify: `src/main.rs` `Commands::Status` arm (924-951)

- [ ] **Step 1: Extend the status arm (manual verification)**

After the existing `get_image_count` output, print:

```rust
let c = imgfind::block_on(imgfind::processing::counts(&db))?;
println!("  missing 300px thumbnails: {}", c.thumbs300);
println!("  missing embeddings:       {}", c.embeddings);
println!("  missing 512/2048 thumbs:  {}", c.full_thumbs);
```

- [ ] **Step 2: Build + verify**

Run: `cargo build && imgfind status` on a partially-processed library. Expected: counts shown and decrease after `imgfind process`. Clippy clean; fmt.

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): status reports unprocessed counts"
```

---

### Task 6: GUI process-worker (`imgfind-gui/src/processor.rs`)

**Files:**
- Create: `imgfind-gui/src/processor.rs`
- Modify: `imgfind-gui/src/main.rs` or module root (declare `mod processor;`), `imgfind-gui/src/backend.rs` (expose what the worker needs: `Database` clone, `Arc<OnceLock<ClipEmbedder>>`, `model_ready`)
- Test: `imgfind-gui/src/processor.rs` `#[cfg(test)]` (pure helpers only)

**Interfaces:**
- Consumes: `imgfind::processing::{counts, process_next_batch, ProcessPhase, ProcessCounts}`; `Backend`'s db + embedder.
- Produces:
  - `struct ProcessProgress { counts: ProcessCounts, state: WorkerState }` and `enum WorkerState { Running, Paused, Idle }`
  - `struct ProcessController { paused: Arc<AtomicBool>, /* handle */ }` with `pause()`, `resume()`, `is_paused()`.
  - `fn spawn_process_worker(backend: Backend, paused: Arc<AtomicBool>, progress: Sender<ProcessProgress>) -> JoinHandle<()>`
  - pure helper `fn phase_label(p: ProcessPhase) -> &'static str` and `fn overall(counts: &ProcessCounts, done_baseline: &ProcessCounts) -> (usize, usize)` (done/total for the pill) — unit-tested.

- [ ] **Step 1: Write failing tests for pure helpers**

```rust
#[test]
fn pause_flag_round_trips() {
    let f = Arc::new(AtomicBool::new(false));
    let c = ProcessController::new(f.clone());
    assert!(!c.is_paused());
    c.pause(); assert!(c.is_paused()); assert!(f.load(Ordering::SeqCst));
    c.resume(); assert!(!c.is_paused());
}

#[test]
fn overall_progress_done_total() {
    let baseline = ProcessCounts { thumbs300: 10, embeddings: 10, full_thumbs: 20 };
    let now = ProcessCounts { thumbs300: 0, embeddings: 4, full_thumbs: 20 };
    // total work = 40; remaining now = 24; done = 16
    assert_eq!(overall(&now, &baseline), (16, 40));
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p imgfind-gui processor:: 2>&1 | tail -20`
Expected: FAIL.

- [ ] **Step 3: Implement**

`spawn_process_worker` loops: read `counts` (via `block_on`); compute `next_phase`; if `None` → send `Idle` and park/exit; if `paused` → send `Paused`, sleep-park until resumed; for `Embeddings` phase, wait until `backend.model_ready()` (poll with short sleep) and pass the embedder; call `process_next_batch` (small batch, e.g. 16–32 for responsiveness); send `Running` + fresh counts. Zero-progress guard mirrors the engine. `overall` computes `(total - remaining, total)` from a baseline captured at worker start (re-baseline if counts grow, e.g. user indexed more). Keep `ProcessController` + helpers pure and tested; the thread body need not be unit-tested (manual smoke in Task 7).

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p imgfind-gui processor:: 2>&1 | tail -20`
Expected: PASS. Clippy clean (`cargo clippy -p imgfind-gui --all-targets`); `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/processor.rs imgfind-gui/src/backend.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): background process-worker + pause controller"
```

---

### Task 7: GUI status panel + `\` toggle + pill + search hint + launcher/wiring

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (status pill in the statusline; a status panel; `\` key in the grid/lightbox `FocusScope`s; reflow not overlay; "N still indexing" hint near the search box), `imgfind-gui/src/backend.rs` / app glue (wire `ProcessProgress` channel → Slint properties via `invoke_from_event_loop`; start the worker on open; Pause/Resume callback), `imgfind-launcher/src/` (drop the `imgfind thumbnails --gui-sizes --all` spawn)

**Interfaces:**
- Consumes: Task 6 `spawn_process_worker`, `ProcessController`, `ProcessProgress`.

- [ ] **Step 1: Wire the worker to the UI (manual verification — Slint markup is build+smoke tested)**

Add Slint properties: `process-state: string` ("RUNNING"/"PAUSED"/"IDLE"), `process-thumbs300-done/total`, `process-embed-done/total`, `process-full-done/total`, `process-overall-done/total`, `process-panel-open: bool`, and a `process-pill-text` (ASCII, e.g. `P 16/40`). Add callbacks `toggle-process-panel()` and `toggle-process-pause()`. On `Backend::open`/window show, if `counts.total_remaining() > 0`, spawn the worker; a periodic poll (reuse the existing ~100ms grid timer, or a dedicated channel drain) applies `ProcessProgress` to the properties via `invoke_from_event_loop`. The `\` key (in both grid and lightbox FocusScopes, guarded by the same "is typing" suppression as other chords) calls `toggle-process-panel`. The panel shows three labeled `done / total` bars + state + a Pause/Resume button bound to `toggle-process-pause`; it reflows the image area (follow the detail-panel/edit-sidebar width-reflow pattern, not an overlay). Add the "N still indexing" hint near the search box, visible when `process-embed-total > 0 && process-embed-done < process-embed-total`.

- [ ] **Step 2: Drop the launcher's thumbnails spawn**

In `imgfind-launcher/src/`, the "Index a folder…" flow currently spawns `imgfind index …` then `imgfind thumbnails --gui-sizes --all`. Remove the second spawn (the GUI background job now does it); keep streaming `index` output. (Optionally leave a comment pointing at this spec.)

- [ ] **Step 3: Build + manual smoke**

Run: `cargo build --release --workspace 2>&1 | tail -5`, then on a fresh library: `imgfind index <dir>` → `imgfind gui -d <dir>`. Verify: grid shows placeholders that fill in; the pill shows progress; `\` toggles the panel; Pause/Resume halts/continues; closing the panel keeps it running; search shows the "still indexing" hint until embeddings finish, then returns results. Clippy clean across the workspace; `cargo fmt --all`.

- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/ imgfind-launcher/src/
git commit -m "feat(gui): processing status panel, pill, \\ toggle; launcher drops thumbnails spawn"
```

---

### Task 8: Documentation

**Files:**
- Modify: `CLAUDE.md` (CLI commands list; indexing-flow; GUI background-job + status-panel; launcher section), `USAGE.md`, `README.md`

- [ ] **Step 1: Update docs**

In `CLAUDE.md`: change the `index` description to "fast row-only"; add `process` to the CLI commands list and a short architecture note pointing at this spec (`docs/superpowers/specs/2026-06-28-fast-index-background-processing-design.md`); document the GUI background job (auto-start, pausable, `\` toggle, status pill) and that `status` now reports unprocessed counts; update the launcher bullet (no more `thumbnails --all` spawn). Mirror in `USAGE.md` and `README.md` (user-facing: "`imgfind index` is fast; run `imgfind process` or just open the GUI to finish").

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md USAGE.md README.md
git commit -m "docs: fast index + background processing"
```

---

## Self-Review

**Spec coverage:** index-fast (T2), process CLI (T3/T4), status counts (T5), shared engine + embed-from-300px (T3), DB methods incl. no migration (T1), GUI auto-start worker + pause (T6), status panel + `\` + pill + search hint (T7), launcher/install/docs (T7/T8). All spec sections map to a task.

**Placeholder scan:** No TBD/TODO; code shown for each Rust logic step; Slint markup steps are build+smoke-verified (acceptable — UI markup isn't unit-testable, mirroring existing GUI tasks). The two notes to "confirm exact signature against the source" are deliberate guardrails, not placeholders — the surrounding code is concrete.

**Type consistency:** `ProcessPhase`/`ProcessCounts`/`process_next_batch`/`counts`/`run_to_completion` names are used identically across T3–T7; `set_image_embedding(i64, &[f32])`, `get_images_without_embedding(usize) -> Vec<(i64, AbsolutePath, String)>`, `count_images_without_embedding() -> usize` consistent T1↔T3↔T5. `ProcessProgress`/`WorkerState`/`ProcessController` consistent T6↔T7.
