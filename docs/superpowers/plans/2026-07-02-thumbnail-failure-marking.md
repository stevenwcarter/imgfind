# Thumbnail Failure Marking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record a permanent per-image failure marker the first time a thumbnail fails to generate so undecodable images stop being retried on every processing pass, exclude them from the thumbnail/embedding/status work queues, add a `process --retry-failed` escape hatch, and add a GUI filter toggle that hides failed images.

**Architecture:** A new `thumbnail_failures(image_hash, size)` table (migration 006) records failures. The thumbnail writer thread persists a failure row when decode fails (channel message becomes an enum). Work-selection queries gain `NOT EXISTS` guards. The GUI `Filters` struct gains a `hide_failed` bool wired through the existing single-toggle filter pattern.

**Tech Stack:** Rust (edition 2024), Turso (async SQLite), anyhow, rayon, Slint (GUI).

## Global Constraints

- Migrations are idempotent (`CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`), version-gated via `schema_meta`, and stamp `LATEST_MIGRATION_VERSION` only after all succeed.
- `thumbnails` and `thumbnail_failures` are keyed by **content hash** (TEXT `oshash`/md5), not `image_id`.
- The `thumbnails.size` convention: scaled long-edge px; `FullSize → 0`. Failure markers use the same convention; the 300 px marker (`size = 300`) is the decode-failure sentinel that also gates embeddings.
- All `Database` methods are `async`; sync callers use `imgfind::block_on(...)`.
- DB writes in the thumbnail batch path happen only on the single writer thread.
- `Filters` persists to `ui_state` JSON via the hand-rolled `FiltersRepr`; **new fields MUST be `#[serde(default)]`** so old saved sessions still load.
- Errors use `anyhow` `Context`/`with_context`. Logging via `tracing`.
- Run `cargo test` from the repo root (workspace). Building the workspace requires the sibling `../clipper` crate to be present.

---

### Task 1: Migration 006 — `thumbnail_failures` table

**Files:**
- Modify: `src/schema.rs` (bump `LATEST_MIGRATION_VERSION`, add migration fn + gate, extend test)

**Interfaces:**
- Produces: table `thumbnail_failures(id, image_hash TEXT, size INTEGER, error TEXT, failed_at, UNIQUE(image_hash, size))`, index `idx_thumbnail_failures_hash`, and `LATEST_MIGRATION_VERSION = 6`.

- [ ] **Step 1: Extend the idempotency test to expect the new table and version 6**

In `src/schema.rs`, in the `migrations_are_idempotent_and_create_tables` test (currently around line 450), add `"thumbnail_failures"` to the table list, and update the version assertion below it to expect `6`. The final assertion block reads the `schema_meta` version — change its expected value from `5` to `6`:

```rust
        for t in [
            "images",
            "image_vectors",
            "thumbnails",
            "thumbnail_failures",
            "image_metadata",
            "favorites",
            "tags",
            "image_tags",
            "collections",
            "collection_images",
            "models",
            "ui_state",
            "image_edits",
            "schema_meta",
        ] {
            assert!(table_exists(&conn, t).await, "missing table {t}");
        }
```

Also add a focused test right after `migration_005_adds_edit_control_columns`:

```rust
    #[tokio::test]
    async fn migration_006_creates_thumbnail_failures() {
        let conn = mem().await;
        run_migrations(&conn).await.unwrap();
        assert!(table_exists(&conn, "thumbnail_failures").await);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind schema:: 2>&1 | tail -20`
Expected: FAIL — `thumbnail_failures` table missing and/or version mismatch (or the new test fails).

- [ ] **Step 3: Add the migration function and gate**

In `src/schema.rs`, change the constant (line 20):

```rust
pub const LATEST_MIGRATION_VERSION: i32 = 6;
```

Add the gate inside `run_migrations`, after the `if current < 5 { … }` block and before the version-stamp block:

```rust
    if current < 6 {
        migration_006_thumbnail_failures(conn)
            .await
            .context("migration 6 (thumbnail failures)")?;
    }
```

Add the migration function next to the other `migration_00N_*` functions (e.g. after `migration_005_edit_controls`):

```rust
/// Migration 6: record thumbnails that permanently fail to generate so the
/// pipeline stops retrying undecodable images on every pass. Keyed by content
/// hash + size, mirroring the `thumbnails` table.
async fn migration_006_thumbnail_failures(conn: &turso::Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS thumbnail_failures (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            image_hash TEXT NOT NULL, \
            size INTEGER NOT NULL, \
            error TEXT, \
            failed_at DATETIME DEFAULT CURRENT_TIMESTAMP, \
            UNIQUE(image_hash, size)\
        )",
        (),
    )
    .await
    .context("create thumbnail_failures table")?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_thumbnail_failures_hash \
         ON thumbnail_failures(image_hash)",
        (),
    )
    .await
    .context("create idx_thumbnail_failures_hash")?;
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind schema:: 2>&1 | tail -20`
Expected: PASS (all schema tests, including `migration_006_creates_thumbnail_failures` and the idempotency test).

- [ ] **Step 5: Commit**

```bash
git add src/schema.rs
git commit -m "feat(schema): migration 006 thumbnail_failures table"
```

---

### Task 2: Database methods to record/clear/query failure markers

**Files:**
- Modify: `src/database.rs` (add insert/clear methods; add `NOT EXISTS` guards to 4 query fns)
- Test: add a `#[cfg(test)]` test (in `src/database.rs`'s existing test module, or a new one following the file's conventions)

**Interfaces:**
- Consumes: migration 006 table from Task 1.
- Produces:
  - `Database::insert_thumbnail_failure(&self, hash: &str, size: u32, error: &str) -> Result<()>` (async) — `INSERT OR IGNORE`.
  - `Database::clear_thumbnail_failures(&self) -> Result<usize>` (async) — `DELETE FROM thumbnail_failures`, returns rows deleted.
  - `get_images_without_thumbnails` / `count_images_without_thumbnails` now exclude images with a marker at that size.
  - `get_images_without_embedding` / `count_images_without_embedding` now exclude images with a **300 px** marker.

- [ ] **Step 1: Write a failing test**

Add to the test module in `src/database.rs` (find the existing `#[cfg(test)] mod tests` in that file and add there; it already has helpers to build an in-memory/temp DB — follow the nearest existing test's setup pattern for constructing a `Database` and inserting an image row). Write:

```rust
    #[tokio::test]
    async fn failure_marker_excludes_from_work_queues_and_clears() {
        // Build a temp DB with one image row (path "a.jpg", hash "h1").
        let (db, _tmp) = new_test_db().await; // use the file's existing test-db helper
        db.insert_image_row("a.jpg", "h1").await.unwrap(); // use the file's existing insert helper

        // Initially the image needs a 300px thumbnail and an embedding.
        assert_eq!(
            db.count_images_without_thumbnails(ThumbnailSize(300)).await.unwrap(),
            1
        );
        assert_eq!(db.count_images_without_embedding().await.unwrap(), 1);

        // Mark the 300px thumbnail as failed.
        db.insert_thumbnail_failure("h1", 300, "boom").await.unwrap();

        // Now it is excluded from BOTH the thumbnail and embedding queues.
        assert_eq!(
            db.count_images_without_thumbnails(ThumbnailSize(300)).await.unwrap(),
            0
        );
        assert!(
            db.get_images_without_thumbnails(ThumbnailSize(300), 10).await.unwrap().is_empty()
        );
        assert_eq!(db.count_images_without_embedding().await.unwrap(), 0);
        assert!(db.get_images_without_embedding(10).await.unwrap().is_empty());

        // Clearing markers re-includes it.
        let cleared = db.clear_thumbnail_failures().await.unwrap();
        assert_eq!(cleared, 1);
        assert_eq!(
            db.count_images_without_thumbnails(ThumbnailSize(300)).await.unwrap(),
            1
        );
        assert_eq!(db.count_images_without_embedding().await.unwrap(), 1);
    }
```

NOTE to implementer: use the exact test-DB constructor and image-insert helper this file already uses in its other tests (search the test module for how existing tests build a `Database` and add an image + its metadata/embedding rows). If no reusable image-insert helper exists, insert directly with a raw `conn.execute("INSERT INTO images (path, hash) VALUES ('a.jpg','h1')", ())`. Do NOT invent helper names that don't exist — match the file.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p imgfind failure_marker_excludes 2>&1 | tail -20`
Expected: FAIL — `insert_thumbnail_failure` / `clear_thumbnail_failures` don't exist (compile error), or the exclusion assertions fail.

- [ ] **Step 3: Add the insert/clear methods**

In `src/database.rs`, near the other thumbnail methods (e.g. after `insert_thumbnails_batch`), add:

```rust
    /// Record that a thumbnail permanently failed to generate for `hash`/`size`.
    ///
    /// `INSERT OR IGNORE` so repeated failures for the same `(hash, size)` are a
    /// no-op. Marked images are excluded from the thumbnail/embedding work queues
    /// until [`Database::clear_thumbnail_failures`] is called.
    pub async fn insert_thumbnail_failure(&self, hash: &str, size: u32, error: &str) -> Result<()> {
        let conn = self
            .pool
            .get()
            .await
            .context("get connection to record thumbnail failure")?;
        conn.execute(
            "INSERT OR IGNORE INTO thumbnail_failures (image_hash, size, error) \
             VALUES (?1, ?2, ?3)",
            (hash.to_string(), size as i64, error.to_string()),
        )
        .await?;
        Ok(())
    }

    /// Delete all thumbnail failure markers (used by `process --retry-failed`).
    /// Returns the number of markers cleared.
    pub async fn clear_thumbnail_failures(&self) -> Result<usize> {
        let conn = self
            .pool
            .get()
            .await
            .context("get connection to clear thumbnail failures")?;
        let n = conn
            .execute("DELETE FROM thumbnail_failures", ())
            .await
            .context("delete thumbnail_failures")?;
        Ok(n as usize)
    }
```

NOTE: verify the return type of `conn.execute(...)` in this codebase (Turso returns a `u64` row-count). If it returns `()` rather than a count, instead run a `SELECT COUNT(*)` before the `DELETE` and return that. Match how other methods in this file read affected-row counts.

- [ ] **Step 4: Add the `NOT EXISTS` guards to the four query functions**

`get_images_without_thumbnails` (SQL currently around line 1364) — add the guard before `LIMIT`:

```rust
                "SELECT i.path, i.hash \
                 FROM images i \
                 LEFT JOIN thumbnails t ON i.hash = t.image_hash AND t.size = ?1 \
                 WHERE t.id IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM thumbnail_failures f \
                                   WHERE f.image_hash = i.hash AND f.size = ?1) \
                 LIMIT ?2",
```

`count_images_without_thumbnails` (SQL around line 1391):

```rust
                "SELECT COUNT(*) \
                 FROM images i \
                 LEFT JOIN thumbnails t ON i.hash = t.image_hash AND t.size = ?1 \
                 WHERE t.id IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM thumbnail_failures f \
                                   WHERE f.image_hash = i.hash AND f.size = ?1)",
```

`get_images_without_embedding` (SQL built via `format!` around line 891) — gate on the 300 px marker (literal, not a bound param, since this query uses positional `?1` for `limit`):

```rust
        let sql = format!(
            "SELECT i.id, i.path, i.hash \
             FROM images i \
             LEFT JOIN {vt} v ON v.image_id = i.id \
             WHERE v.image_id IS NULL \
               AND NOT EXISTS (SELECT 1 FROM thumbnail_failures f \
                               WHERE f.image_hash = i.hash AND f.size = 300) \
             LIMIT ?1"
        );
```

`count_images_without_embedding` (SQL built via `format!` around line 864):

```rust
        let sql = format!(
            "SELECT COUNT(*) \
             FROM images i \
             LEFT JOIN {vt} v ON v.image_id = i.id \
             WHERE v.image_id IS NULL \
               AND NOT EXISTS (SELECT 1 FROM thumbnail_failures f \
                               WHERE f.image_hash = i.hash AND f.size = 300)"
        );
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p imgfind failure_marker_excludes 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Run the broader DB + schema tests to check for regressions**

Run: `cargo test -p imgfind database:: 2>&1 | tail -20`
Expected: PASS (no regressions in existing thumbnail/embedding query tests).

- [ ] **Step 7: Commit**

```bash
git add src/database.rs
git commit -m "feat(db): thumbnail failure markers exclude images from work queues"
```

---

### Task 3: Record failures at the thumbnail generation seam

**Files:**
- Modify: `src/thumbnail.rs` (writer-channel message enum; record on batch failure and on `get_or_generate_thumbnail` failure)

**Interfaces:**
- Consumes: `Database::insert_thumbnail_failure` from Task 2.
- Produces: on a decode/encode failure, a `thumbnail_failures` row is written for `(hash, size)`.

- [ ] **Step 1: Change the writer channel to carry success OR failure**

In `src/thumbnail.rs`, define a message enum above `generate_missing_thumbnails_batch`:

```rust
/// A message from a thumbnail-generation worker to the single DB writer thread.
enum ThumbMsg {
    /// Successfully generated JPEG bytes for `(hash, size)`.
    Ok { hash: String, size: u32, data: Vec<u8> },
    /// Generation failed for `(hash, size)`; record a permanent marker.
    Failed { hash: String, size: u32, error: String },
}
```

Change the channel type (line 53) from `(String, u32, Vec<u8>)` to `ThumbMsg`:

```rust
    let (tx, rx) = std::sync::mpsc::channel::<ThumbMsg>();
```

- [ ] **Step 2: Update the writer thread to split Ok vs Failed**

Rewrite the writer thread body so it buffers `Ok` rows for the batched insert (unchanged behavior) and writes `Failed` rows immediately via `insert_thumbnail_failure`. Replace the `for item in rx { … }` loop and the `flush` closure's buffer type accordingly:

```rust
        let mut buffer: Vec<(String, u32, Vec<u8>)> = Vec::with_capacity(10);

        let flush = |buf: &mut Vec<(String, u32, Vec<u8>)>| {
            if buf.is_empty() {
                return;
            }
            tracing::info!("Flushing {} thumbnails to database", buf.len());
            let batch = std::mem::take(buf);
            let n = batch.len();
            match block_on(writer_db.insert_thumbnails_batch(&batch)) {
                Ok(()) => {
                    writer_count.fetch_add(n, Ordering::SeqCst);
                    tracing::debug!("Inserted {n} thumbnails");
                }
                Err(e) => {
                    tracing::error!("Failed to commit thumbnail batch: {:?}", e);
                }
            }
        };

        for msg in rx {
            match msg {
                ThumbMsg::Ok { hash, size, data } => {
                    buffer.push((hash, size, data));
                    if buffer.len() >= 40 {
                        flush(&mut buffer);
                    }
                }
                ThumbMsg::Failed { hash, size, error } => {
                    if let Err(e) = block_on(writer_db.insert_thumbnail_failure(&hash, size, &error))
                    {
                        tracing::error!("Failed to record thumbnail failure for {hash}: {e:#}");
                    }
                }
            }
        }
        flush(&mut buffer);
```

- [ ] **Step 3: Send a `Failed` message from the rayon closure on error**

Update the parallel loop (around line 126) so the error arm sends a `ThumbMsg::Failed`. Because `generate_and_store_thumbnail` needs to send `ThumbMsg::Ok` now, change its signature too. Update the closure:

```rust
    images_with_edits
        .par_iter()
        .for_each(|(path, hash, edits)| {
            let path_str = path.as_str();
            if let Err(e) = generate_and_store_thumbnail(path_str.as_ref(), hash, size, edits, &tx)
            {
                tracing::warn!("Failed to generate thumbnail for {}: {:?}", path_str, e);
                let _ = tx.send(ThumbMsg::Failed {
                    hash: hash.to_string(),
                    size: size.get(),
                    error: format!("{e:#}"),
                });
            } else {
                tracing::info!("Generated thumbnail for: {}", path_str);
            }
        });
```

- [ ] **Step 4: Update `generate_and_store_thumbnail` to send `ThumbMsg::Ok`**

Change its `tx` parameter type and the `tx.send(...)` payload (around line 204):

```rust
fn generate_and_store_thumbnail(
    filepath: &str,
    hash: &str,
    size: ThumbnailSize,
    edits: &ImageEdits,
    tx: &Sender<ThumbMsg>,
) -> Result<()> {
    let bytes = generate_thumbnail_bytes(filepath, ThumbnailSpec::ScaleSize(size), edits)?;
    tx.send(ThumbMsg::Ok {
        hash: hash.to_string(),
        size: size.get(),
        data: bytes,
    })
    .context("Failed to send thumbnail bytes over channel")?;
    Ok(())
}
```

- [ ] **Step 5: Record a marker on the single-image on-demand path**

In `get_or_generate_thumbnail`, when `generate_thumbnail_bytes` fails, record a marker before returning the error. Only mark **scaled** renditions (a `FullSize` failure should not poison the 300/512 pipeline). Replace the `let bytes = generate_thumbnail_bytes(...)?;` line (around line 251) with:

```rust
    let bytes = match generate_thumbnail_bytes(filepath, spec, &edits) {
        Ok(b) => b,
        Err(e) => {
            if let ThumbnailSpec::ScaleSize(size) = spec {
                if let Err(rec) = block_on(db.insert_thumbnail_failure(hash, size.get(), &format!("{e:#}"))) {
                    tracing::error!("Failed to record thumbnail failure for {hash}: {rec:#}");
                }
            }
            return Err(e);
        }
    };
```

- [ ] **Step 6: Build to verify it compiles**

Run: `cargo build -p imgfind 2>&1 | tail -20`
Expected: clean build (the `Sender<(String, u32, Vec<u8>)>` import for `Sender` is still used — leave the `use` line as-is).

- [ ] **Step 7: Run thumbnail + full test suite for the core crate**

Run: `cargo test -p imgfind 2>&1 | tail -25`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/thumbnail.rs
git commit -m "feat(thumbnail): record failure markers on decode/encode failure"
```

---

### Task 4: `process --retry-failed` flag

**Files:**
- Modify: `src/main.rs` (add flag to `Commands::Process`; clear markers before running)

**Interfaces:**
- Consumes: `Database::clear_thumbnail_failures` from Task 2.
- Produces: `imgfind process --retry-failed` clears all markers before the pass.

- [ ] **Step 1: Add the flag to the `Process` subcommand**

In `src/main.rs`, in the `Process { … }` variant (around line 146), add:

```rust
        /// Clear all thumbnail-failure markers first, so previously failed
        /// images are re-attempted this pass.
        #[arg(long)]
        retry_failed: bool,
```

- [ ] **Step 2: Destructure and act on it in the handler**

In the `Commands::Process { … } =>` arm (around line 313), add `retry_failed` to the destructure and clear markers right after opening the DB, before building the embedder:

```rust
        Commands::Process {
            count,
            no_embeddings,
            dir,
            retry_failed,
        } => {
            let db_path = get_db_path(dir.as_deref())?;
            let mut db = imgfind::block_on(Database::new(&db_path))?;
            if retry_failed {
                let cleared = imgfind::block_on(db.clear_thumbnail_failures())?;
                info!("Cleared {cleared} thumbnail-failure marker(s); re-attempting.");
            }
```

(Leave the rest of the arm unchanged.)

- [ ] **Step 3: Build and check the flag is wired**

Run: `cargo build -p imgfind 2>&1 | tail -20 && ./target/debug/imgfind process --help 2>&1 | grep -A1 retry-failed`
Expected: clean build; `--retry-failed` appears in the help output.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): process --retry-failed clears failure markers"
```

---

### Task 5: `Filters.hide_failed` core field + SQL clause

**Files:**
- Modify: `src/filters.rs` (struct field, `FiltersRepr`, both `From` impls, `build_filter_clause_turso`, tests)

**Interfaces:**
- Produces: `Filters { hide_failed: bool, .. }`; when `true`, `build_filter_clause_turso` appends `NOT EXISTS (SELECT 1 FROM thumbnail_failures f WHERE f.image_hash = i.hash)`.

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `src/filters.rs`:

```rust
    #[test]
    fn hide_failed_emits_not_exists_clause() {
        let f = Filters {
            hide_failed: true,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause_turso(&f);
        assert_eq!(
            sql,
            " AND NOT EXISTS (SELECT 1 FROM thumbnail_failures f WHERE f.image_hash = i.hash)"
        );
        assert!(params.is_empty());
    }

    #[test]
    fn hide_failed_default_false_emits_nothing() {
        let (sql, _) = build_filter_clause_turso(&Filters::default());
        assert!(!sql.contains("thumbnail_failures"));
    }

    #[test]
    fn hide_failed_round_trips_and_defaults_when_absent() {
        // New field round-trips.
        let f = Filters { hide_failed: true, ..Default::default() };
        let json = serde_json::to_string(&f).unwrap();
        let back: Filters = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
        // Old JSON lacking the field deserializes to false.
        let old = r#"{"size_min":null,"size_max":null,"extensions":[],"gps":"any","tags":[],"tag_match":"allof","tags_enabled":false}"#;
        let loaded: Filters = serde_json::from_str(old).unwrap();
        assert!(!loaded.hide_failed);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind filters:: 2>&1 | tail -20`
Expected: FAIL — `hide_failed` field does not exist (compile error).

- [ ] **Step 3: Add the field to `Filters`**

In `src/filters.rs`, add to the struct (after `tag_filter`):

```rust
    /// When true, exclude images that have a thumbnail-failure marker.
    pub hide_failed: bool,
```

- [ ] **Step 4: Thread it through `FiltersRepr` and both `From` impls**

Add to `FiltersRepr`:

```rust
    #[serde(default)]
    hide_failed: bool,
```

In `From<&Filters> for FiltersRepr`, set `hide_failed: f.hide_failed,`.
In `From<FiltersRepr> for Filters`, set `hide_failed: r.hide_failed,`.

- [ ] **Step 5: Emit the clause in `build_filter_clause_turso`**

After the GPS `match` block and before the tag block (order doesn't affect correctness, but keep it grouped with the other boolean predicates), add:

```rust
    if f.hide_failed {
        clauses.push(
            "NOT EXISTS (SELECT 1 FROM thumbnail_failures f WHERE f.image_hash = i.hash)".into(),
        );
    }
```

NOTE: place this so the `hide_failed_emits_not_exists_clause` test's exact expected string holds. Since that test sets ONLY `hide_failed`, the emitted fragment is `" AND NOT EXISTS (...)"` regardless of placement. Fine.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p imgfind filters:: 2>&1 | tail -20`
Expected: PASS (new tests plus all existing filter tests, including the serde round-trip ones).

- [ ] **Step 7: Commit**

```bash
git add src/filters.rs
git commit -m "feat(filters): hide_failed filter excludes images with failure markers"
```

---

### Task 6: GUI wiring for the "hide failed" toggle

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (property + callback + toggle chip in the filter bar)
- Modify: `imgfind-gui/src/main.rs` (`build_filters` mapping, callback handler, restore-into-UI sync)

**Interfaces:**
- Consumes: `Filters.hide_failed` from Task 5.
- Produces: a filter-bar toggle that sets `hide_failed` and re-runs the query via the existing debounce.

**Implementer guidance:** Mirror the **GPS tri-state** wiring exactly, but as a single boolean toggle (like an on/off chip). The Explore-established anchor points are below; read the surrounding code and copy the nearest existing pattern (the GPS chip buttons and the `gps-mode-changed` handler are the closest analog).

- [ ] **Step 1: Add the Slint property + callback**

In `imgfind-gui/ui/app.slint`, in the filter-bar properties block (near `in property <int> gps-mode: 0;`, ~line 192), add:

```slint
    in property <bool> hide-failed: false;
```

In the callbacks block (near `callback gps-mode-changed(int);`, ~line 225), add:

```slint
    callback hide-failed-changed(bool);
```

- [ ] **Step 2: Add a toggle chip button in the filter bar**

In the filter-bar section of `app.slint` (the GPS buttons live ~line 678-800), add a single toggle button next to the GPS tri-state. Model it on the existing chip buttons; it shows an active/inactive state driven by `root.hide-failed` and emits the toggle:

```slint
        Rectangle {
            border-radius: 4px;
            background: root.hide-failed ? #3b82f6 : #2a2a2a;
            border-width: 1px;
            border-color: #444;
            HorizontalLayout {
                padding: 6px;
                Text {
                    text: "Hide failed";
                    color: white;
                    vertical-alignment: center;
                }
            }
            TouchArea {
                clicked => { root.hide-failed-changed(!root.hide-failed); }
            }
        }
```

NOTE: match the surrounding chips' exact styling/spacing (padding, font-size, colors) — copy a neighboring chip and adjust the label/binding rather than pasting the above verbatim if the file's chips look different.

- [ ] **Step 3: Map the property into `Filters` in `build_filters`**

In `imgfind-gui/src/main.rs`, in `build_filters` (~line 307), read the window property and set the field. Where it currently maps `gps_mode` → `GpsFilter`, add alongside:

```rust
        hide_failed: window.get_hide_failed(),
```

(If `build_filters` constructs `Filters { .. }` with an explicit field list, add `hide_failed`; if it mutates a `Filters` value, set `.hide_failed` accordingly. Match the function's existing shape.)

- [ ] **Step 4: Add the callback handler**

Mirror the `gps-mode-changed` handler registration (Explore located `on_filters_changed`/`gps-mode-changed` near lines 1962-1988). Register a handler for `on_hide_failed_changed` that: sets `window.set_hide_failed(v)`, updates the shared `Filters` (or just relies on `build_filters` reading the prop), and calls `start_debounce(...)` exactly like the GPS handler does:

```rust
    {
        let window_weak = window.as_weak();
        // clone whatever handles the GPS handler clones (filters Arc, debounce, etc.)
        window.on_hide_failed_changed(move |v| {
            let window = window_weak.unwrap();
            window.set_hide_failed(v);
            // rebuild filters + debounce, identical to the gps-mode-changed handler
            // (call the same helper the GPS handler calls)
        });
    }
```

NOTE: use whatever shared helper the GPS handler uses to rebuild filters and kick the debounce (e.g. `on_filters_changed(...)` / `start_debounce(...)`). Do not invent a new path — reuse the GPS handler's body, swapping the mutation for `set_hide_failed`.

- [ ] **Step 5: Restore the saved value into the UI on startup**

Where the filter-restore block sets `window.set_gps_mode(gps_to_mode(&filters.gps))` (~lines 3091-3113), add:

```rust
    window.set_hide_failed(filters.hide_failed);
```

- [ ] **Step 6: Build the GUI**

Run: `cargo build -p imgfind-gui 2>&1 | tail -25`
Expected: clean build. (Slint generates `get_hide_failed`/`set_hide_failed`/`on_hide_failed_changed` from the property/callback names — hyphens become underscores.)

- [ ] **Step 7: Run the workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -25`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): hide-failed filter toggle in the filter bar"
```

---

### Task 7: Documentation

**Files:**
- Modify: `CLAUDE.md`, `USAGE.md`

- [ ] **Step 1: Update `CLAUDE.md`**

- In the Storage section's table list, add a `thumbnail_failures` bullet: `thumbnail_failures — (image_hash, size) markers recorded when a thumbnail permanently fails to generate; excludes the image from the thumbnail/embedding/status work queues so undecodable files aren't retried every pass. Cleared by process --retry-failed.`
- Update the migration sentence: add `006 adds thumbnail_failures` and change `LATEST_MIGRATION_VERSION = 5` → `= 6`.
- In the CLI `process` description, add the `--retry-failed` flag (clears failure markers before the pass).
- In the indexing-flow / thumbnail description, note that a decode/encode failure now records a permanent marker instead of retrying forever.
- In the GUI filter-bar description, add the "Hide failed" toggle (hides images with a failure marker), and reference the spec `docs/superpowers/specs/2026-07-02-thumbnail-failure-marking-design.md`.

- [ ] **Step 2: Update `USAGE.md`**

Document `process --retry-failed` and the GUI "Hide failed" filter toggle, matching the file's existing style for command flags and GUI features.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md USAGE.md
git commit -m "docs: thumbnail failure marking, --retry-failed, hide-failed filter"
```

---

## Self-Review Notes

- **Spec coverage:** migration (T1), markers+exclusion+clear (T2), recording on failure (T3), `--retry-failed` (T4), core filter (T5), GUI toggle (T6), docs (T7). All spec sections covered.
- **Type consistency:** `insert_thumbnail_failure(hash: &str, size: u32, error: &str)` and `clear_thumbnail_failures() -> Result<usize>` used identically across T2/T3/T4. `ThumbMsg` enum defined and consumed within T3. `hide_failed: bool` consistent across T5/T6. `size: u32` matches `ThumbnailSize::get() -> u32` and the existing channel's `u32`.
- **Placeholder scan:** GUI Slint/handler steps intentionally say "copy the neighboring pattern" because exact styling depends on the current `app.slint` chip markup; anchor line numbers and the concrete property/callback/field names are all specified. This is guidance to match existing code, not a deferred decision.
