# Fast Index + Background Processing — Design

**Date:** 2026-06-28
**Status:** Approved (brainstorm → spec)
**Topic:** Split indexing into a fast row-populating pass and a slow, resumable
processing pass (embeddings + thumbnails + EXIF) that runs either explicitly via
the CLI or as an auto-starting background job inside `imgfind-gui`.

## Problem

`imgfind index` does everything in one shot: walk → hash → embed → insert →
EXIF → 300px thumbnails. Embedding (CLIP) and thumbnail generation dominate the
runtime, and the whole run holds SQLite's WAL single-writer lock. A user who
wants to browse a freshly-added folder must wait for the entire slow pass to
finish before the GUI is useful, because the GUI's own writes (`ui_state`,
thumbnails) contend with the indexer's writer lock.

We want: a near-instant initial `index` that just records which files exist, and
the heavy work (embeddings, thumbnails, EXIF) done later — either explicitly with
the `imgfind` CLI or as a pausable background job the GUI runs while you browse.

## Goals

- `imgfind index` returns in seconds on a large folder (rows only).
- A new `imgfind process` command completes the remaining work, resumable and
  idempotent (only does what's missing).
- `imgfind-gui` auto-starts the same processing in the background on launch,
  pausable, with a status panel (toggled by `\` or a progress pill) showing
  per-phase progress.
- The GUI is fully usable (browse, open, filter, and search the already-embedded
  subset) while processing runs.
- All new embeddings share provenance: both CLI and GUI embed from the persisted
  **300px** thumbnail, so a RAW file is decoded once, not twice.

## Non-Goals

- No schema migration (an `images` row without a vector/thumbnail row is now a
  valid intermediate state; no new columns or tables).
- No re-embedding of images already embedded (provenance of *pre-existing*
  vectors is left as-is until a manual re-index).
- No ETA/throughput readout, no Stop/cancel control in v1 (Pause/Resume only).
- No change to the map view (still not ported).

## CLI surface changes

### `imgfind index` — now fast (rows only)

`index_directory` (src/main.rs) drops phases 2–3 (embed/insert-vector, EXIF,
thumbnails). The new flow:

1. Walk the directory honoring `Config` ignore regexes (unchanged).
2. For each file, compute `oshash`.
3. Skip if a row with the same `(path, hash)` already exists (new cheap
   predicate — see `image_row_exists`), so re-running `index` is fast.
4. Insert surviving `(rel_path, hash)` rows in batched transactions via a new
   `insert_image_rows_batch` (no vector row written).

`index` no longer loads the CLIP model and no longer touches `image_vectors`,
`image_metadata`, or `thumbnails`. The `--no-thumbnails` flag becomes a no-op
(kept for compatibility, deprecation note in help text); `--reindex` still forces
re-hashing/re-insert of rows. `--batch-size` now sizes the row-insert batches.

### `imgfind process` — new command (the slow pass)

```
imgfind process [--count <N>] [--no-embeddings] [--sizes <300,512,2048>] [-d DIR]
```

Runs **to completion** (loops batches until nothing is missing), in this order:

1. **300px thumbnails** for images missing them (`ThumbnailSize(300)`), plus an
   EXIF metadata backfill for images in each batch (reuses the existing
   `extract_image_metadata` / `extract_missing_metadata` path).
2. **Embeddings** for images missing a vector in the active model's table —
   decoded **from the persisted 300px thumbnail** (see "Unified embedding
   source").
3. **512px and 2048px thumbnails** (`GUI_THUMBNAIL_SIZES` minus 300) for images
   missing them.

`--count` is the per-batch size (default mirrors `thumbnails`); `--no-embeddings`
skips phase 2 (thumbnails-only); `--sizes` overrides which thumbnail sizes to
fill (default = the three GUI sizes). The command prints per-phase progress
lines, mirroring `thumbnails --all`. It is the CLI face of the shared engine
(below).

### `imgfind status` — extended

Adds unprocessed counts: total images, count missing 300px thumbnails, count
missing embeddings (active model), count missing 512/2048 thumbnails. Lets a
user see at a glance what `process` would do.

## Shared processing engine — `src/processing.rs` (new)

A single module both the CLI and GUI drive, so results are identical. It owns:

- **`ProcessPhase`** enum: `Thumbnails300`, `Embeddings`, `FullThumbnails`.
- **Count queries** (`ProcessCounts`): how many images remain in each phase.
  Built from existing `count_images_without_thumbnails(size)` plus a new
  `count_images_without_embedding()` (active-model-aware `LEFT JOIN`).
- **`process_next_batch(db, embedder, phase, batch_size) -> BatchOutcome`** — does
  one batch of one phase and reports `{ processed: usize, remaining: usize }`.
  Phase 1/3 wrap the existing `generate_missing_thumbnails_batch`; phase 2 is new
  (embed-from-thumbnail). `embedder` is only required/loaded for the `Embeddings`
  phase.
- **`run_to_completion(db, opts, progress_cb)`** — the CLI loop: walk phases in
  order, batch until each is drained, invoking `progress_cb` between batches.
  This is what `imgfind process` calls.

The GUI does **not** call `run_to_completion`; it drives `process_next_batch`
itself so it can interleave its pause flag and progress channel (below).

### Unified embedding source

Both CLI and GUI generate embeddings from the persisted **300px** thumbnail
rather than re-decoding the original:

- Phase 1 guarantees the 300px rendition exists before phase 2 runs.
- Phase 2, per image: `get_or_generate_thumbnail(hash, ScaleSize(300))` → decode
  the JPEG bytes to a `DynamicImage` → `get_image_embeddings_from_dynamic` →
  `normalize_vector` → store via a new `set_image_embedding(image_id, vec)`.
- CLIP downsizes to 224px internally, so 300px is ample resolution; the fidelity
  difference vs. decoding the original is negligible, and RAW is demosaiced once.
- The 300px rendition used is the **identity** (unedited) one — a freshly
  processed image has no baked edits, and embeddings are never regenerated on
  later edits (unchanged invariant). **Invariant this depends on:** a newly
  inserted `images` row has no `image_edits` row, so its 300px thumbnail is
  byte-identical to the unedited decode. A characterization test pins this.

## New database methods (`src/database.rs`)

- `insert_image_rows_batch(rows: &[(String, String)])` — insert `(path, hash)`
  rows only, `ON CONFLICT(path) DO UPDATE SET hash = excluded.hash`. No vector
  write. (Mirrors `insert_images_batch` minus the vector half.)
- `image_row_exists(path: &AbsolutePath, hash: &str) -> bool` — `(path, hash)`
  present in `images`, independent of any vector (the fast-index dedup
  predicate; distinct from `is_image_indexed`, which requires an embedding).
- `count_images_without_embedding() -> usize` and
  `get_images_without_embedding(limit) -> Vec<(i64, AbsolutePath, String)>`
  (`image_id`, abs path, hash) — active-model-aware `LEFT JOIN` on
  `vectors_table()` `WHERE v.image_id IS NULL`.
- `set_image_embedding(image_id: i64, embedding: &[f32])` — upsert one vector row
  into the active model's table (`DELETE` + `INSERT`, like `insert_images_batch`
  does per-row).

No migration: `LATEST_MIGRATION_VERSION` is unchanged.

## GUI — background job + status panel (`imgfind-gui/`)

### Worker (`imgfind-gui/src/processor.rs`, new)

A dedicated `process-worker` thread, **separate** from the interactive
`thumb-worker` in `loader.rs`, so bulk work never starves viewport-priority grid
loading:

- On `Backend::open` (or first window show), if `ProcessCounts` shows any
  remaining work, spawn the process-worker. It loops `process_next_batch` across
  the three phases in order, reusing the already-lazily-loaded embedder
  (`Arc<OnceLock<ClipEmbedder>>`); for the `Embeddings` phase it waits until
  `model_ready()`.
- **Pause:** an `Arc<AtomicBool>`; checked between batches. Paused → the thread
  parks on a `Condvar`/channel until resumed.
- **Progress:** after each batch, send a `ProcessProgress { phase, counts }` over
  a channel; the UI thread applies it via `invoke_from_event_loop` (same pattern
  as the thumb-worker results). A generation/epoch is unnecessary (progress is
  monotonic) but the worker re-reads counts from the DB each batch so a manual
  CLI `process` run in parallel is reflected too.
- **Single heavy writer:** the process-worker is the only thread doing bulk
  writes; GUI `ui_state` writes and interactive thumbnail writes stay short and
  batched, keeping WAL contention within the existing 5s busy-timeout. Writes go
  through `block_on` like the rest of the GUI.
- On caught-up (all counts zero), the worker exits (or parks); state shows
  **Idle**. When the user indexes more via the launcher and reopens, a fresh
  worker starts.

### Status panel (`imgfind-gui/ui/`)

- A **progress pill** in the bottom statusline showing `⚙ done / total` (overall
  across phases) — click to toggle the panel. (Glyph note: per the project's
  Slint-font memory, use an ASCII/Latin-1 symbol, not a multi-byte gear, if it
  tofus — fall back to `[ ]` or `P:`.)
- **`\`** toggles the same panel (suppressed while typing in a text field, like
  the other chord keys). Distinct from `` ` `` (rail).
- Panel contents: three labeled rows — **Thumbnails (300px)**, **Embeddings**,
  **Full thumbnails (512/2048)** — each `done / total` with a bar; an overall
  **Running / Paused / Idle** state line; a **Pause/Resume** button. Closing the
  panel does not stop the job.
- Reuses the existing slide-toggle / panel chrome conventions; the image area
  reflows (not an overlay) consistent with the detail panel and edit sidebar.

### Search while incomplete

Semantic search runs against the embedded subset (unchanged query path —
un-embedded images simply have no vector to match). When `count_images_without_
embedding() > 0`, show a subtle hint near the search box (e.g. "N still
indexing") so an empty/partial result is understood. Browse/grid/filter work on
the full set immediately; un-thumbnailed tiles show the existing placeholder and
fill in as phase 1 progresses.

## Caller / doc updates

- **Launcher** (`imgfind-launcher/`): "Index a folder…" spawns `imgfind index`
  (fast) and **drops** the `imgfind thumbnails --gui-sizes --all` spawn — the GUI
  background job now does that work. The log pane still streams `index` output.
- **`install.sh`**: unchanged (installs binaries); no behavioral coupling.
- **Docs:** update `CLAUDE.md` (CLI commands list, indexing-flow section, GUI
  background-job + status-panel section), `USAGE.md`, and `README.md` to describe
  `index` (fast) + `process` and the GUI background processing.

## Testing strategy

Following TDD; tests live beside the code they cover.

- **`processing` engine:** unit-test `ProcessCounts`/phase selection logic and a
  `process_next_batch` round-trip against a temp DB seeded with a few images —
  assert each phase drains and is idempotent (re-running does nothing).
- **DB methods:** `insert_image_rows_batch` writes no vector; `image_row_exists`
  vs `is_image_indexed` divergence (row present, no embedding →
  `image_row_exists` true, `is_image_indexed` false);
  `count/get_images_without_embedding` correctness after partial embed;
  `set_image_embedding` round-trips and flips the missing-embedding count to 0.
- **Unified embedding source (load-bearing):** a characterization test that
  embedding a freshly-inserted image uses the identity 300px rendition and
  produces a normalized vector of the active model's dimension — pins the
  "new row has no edits → 300px == unedited decode" invariant the embedding
  source depends on.
- **Fast `index`:** an integration test that `index` inserts rows with **no**
  vectors/thumbnails/metadata and returns, and that a subsequent `process`
  fills all three and leaves zero remaining.
- **GUI pure logic:** the pause-flag state transitions and the progress→panel
  mapping (done/total per phase) as pure helpers in `processor.rs`, unit-tested
  off the UI thread (mirroring `zoompan.rs`/`edits_ui.rs` conventions). Slint
  wiring is verified by build + manual smoke.

## Invariants this feature depends on

- A newly inserted `images` row has **no** `image_edits` row, so its 300px
  thumbnail equals the unedited decode (the embedding source). Pinned by the
  unified-embedding-source characterization test.
- `vectors_table()` resolves to the active model's table; "missing embedding" is
  model-relative. Switching models legitimately makes all rows "missing
  embedding" again — `process` (or the GUI job) then backfills the new model.
- The thumbnail cache is content-hash-keyed, so duplicate files share one
  rendition (and thus one embedding source) — unchanged.

## Risks / tradeoffs

- **WAL contention:** background worker + GUI writes share one DB. Mitigation:
  single heavy writer (the process-worker), short batched writes, the existing
  5s busy timeout. If contention shows up under load, batch sizes are the tuning
  knob.
- **Embedding provenance shift:** new embeddings come from 300px renditions;
  pre-existing vectors (from originals) remain until re-index. Acceptable — CLIP
  crops to 224 so the vectors are effectively equivalent.
- **CPU load while browsing:** the embedding pass is heavy; the visible Pause
  control and the dedicated (non-grid) worker thread mitigate perceived
  sluggishness.
