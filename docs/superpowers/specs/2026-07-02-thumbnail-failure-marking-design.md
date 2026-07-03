# Thumbnail failure marking + "hide failed" filter

**Date:** 2026-07-02
**Status:** Approved

## Problem

When a thumbnail fails to generate (e.g. a truncated JPEG that fails to decode —
`Format error decoding Jpeg: "Marker missing where expected"`), the pipeline only
logs a `tracing::warn!` and writes nothing to the DB. Because "needs a thumbnail"
is defined purely as *"has no row in `thumbnails` of this size"* (a
`LEFT JOIN thumbnails ... WHERE t.id IS NULL`), a permanently-undecodable image is
re-selected on **every** processing pass — forever. The GUI background worker, the
`process` command, and each `status` call all re-attempt the same broken files.

The same broken image also churns the **embedding** phase: embeddings are computed
from the persisted 300px thumbnail, which never exists for an undecodable file, so
`get_images_without_embedding` keeps returning it too.

Users also want a way to hide these broken images from the GUI grid.

## Goals

1. Record a **permanent failure marker** the first time a thumbnail fails to
   generate, so it is never retried automatically.
2. Exclude marked images from the thumbnail **and** embedding phases (and, by
   reuse, from the `status` counts) so a fully-undecodable image drops out of the
   whole pipeline.
3. A GUI **filter toggle** to hide failed images from the grid.
4. A manual **`process --retry-failed`** escape hatch to clear all markers and
   re-attempt.

## Non-goals

- No attempt counter / backoff. One failure ⇒ marked. (A genuinely re-encoded file
  gets a new content hash and is retried naturally, since markers are hash-keyed.)
- No per-file "retry just this one" UI. `--retry-failed` clears all markers.
- No change to how successful thumbnails are stored or read.

## Design

### Storage — migration 006

A new table keyed by `(image_hash, size)` — mirroring how `thumbnails` is keyed by
content hash (`oshash`/md5), *not* `image_id`:

```sql
CREATE TABLE IF NOT EXISTS thumbnail_failures (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    image_hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    error TEXT,
    failed_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(image_hash, size)
);
CREATE INDEX IF NOT EXISTS idx_thumbnail_failures_hash
    ON thumbnail_failures(image_hash);
```

- `size` matches the `thumbnails.size` convention (long-edge px; `FullSize → 0`).
- Keying by hash means duplicate files (same bytes) share one marker, consistent
  with the content-hash-keyed thumbnail cache.
- Migration wiring in `src/schema.rs`: add `migration_006_thumbnail_failures`, an
  `if current < 6 { ... }` gate in `run_migrations`, set
  `LATEST_MIGRATION_VERSION = 6`, and extend the migration-idempotency test's table
  list.

### Recording failures — `src/thumbnail.rs`

`generate_missing_thumbnails_batch` runs a rayon-parallel loop; the per-image
closure calls `generate_and_store_thumbnail`, which on success `tx.send(...)`s the
JPEG bytes to a writer thread. On failure today it only logs.

Change: on error, in addition to the existing `tracing::warn!`, send a failure
record over the same writer channel so the writer thread persists it in the same
transaction context as successes. The channel message becomes an enum, e.g.:

```rust
enum ThumbMsg {
    Ok { image_hash: String, size: i64, data: Vec<u8> },
    Failed { image_hash: String, size: i64, error: String },
}
```

The writer thread inserts `Ok` into `thumbnails` (as today) and `Failed` into
`thumbnail_failures` via `INSERT OR IGNORE`. This keeps DB writes on the single
writer thread (Turso single-writer friendliness) and avoids sprinkling async
`block_on` calls inside the rayon closure.

`get_or_generate_thumbnail` (the GUI's single-image cache-first path) also records
a failure marker on decode error so on-demand failures are captured too.

A new `Database` method records/clears markers:
- `insert_thumbnail_failure(hash, size, error)` — `INSERT OR IGNORE`.
- `clear_thumbnail_failures()` — `DELETE FROM thumbnail_failures` (for
  `--retry-failed`).

### Stop the retries — `src/database.rs`

Add a `NOT EXISTS` guard to the work-selection queries:

- `get_images_without_thumbnails(size, count)` and
  `count_images_without_thumbnails(size)` gain:
  ```sql
  AND NOT EXISTS (
      SELECT 1 FROM thumbnail_failures f
      WHERE f.image_hash = i.hash AND f.size = ?size
  )
  ```
- `get_images_without_embedding(...)` and `count_images_without_embedding()` gain a
  guard against the **300px** marker (the decode failure that blocks embedding):
  ```sql
  AND NOT EXISTS (
      SELECT 1 FROM thumbnail_failures f
      WHERE f.image_hash = i.hash AND f.size = 300
  )
  ```

Because `processing::counts` and the GUI worker reuse these queries, the `status`
counts and the GUI's "N still indexing" hint drop marked images automatically. No
change needed in `src/processing.rs` beyond what the queries provide (the existing
zero-progress break remains as a backstop).

### `process --retry-failed` — `src/main.rs`

Add a `--retry-failed` bool flag to the `process` subcommand. When set, call
`db.clear_thumbnail_failures()` before `run_to_completion`, so every previously
marked image is re-attempted. Log how many markers were cleared.

### GUI "hide failed" filter

**Core (`src/filters.rs`):**
- Add `hide_failed: bool` to `Filters` (default `false`).
- Add it to `FiltersRepr` with `#[serde(default)]` so existing `ui_state` JSON
  (which predates the field) still deserializes.
- In `build_filter_clause_turso`, when `hide_failed` is true, push:
  ```sql
  NOT EXISTS (SELECT 1 FROM thumbnail_failures f WHERE f.image_hash = i.hash)
  ```
  (matches at any size; in practice a decode failure marks 300px, so these are the
  fully-broken images). `browse_all`, `search_meta`, and `find_similar` all route
  through this builder, so the toggle applies everywhere.

**GUI wiring (`imgfind-gui/`)** — mirror the existing single-toggle pattern:
- `ui/app.slint`: an `in property <bool> hide-failed;` and a
  `callback hide-failed-changed(bool);`, plus a toggle chip button in the filter
  bar next to the GPS tri-state / type chips.
- `src/main.rs`: map the property into `Filters` in `build_filters`; add a
  `hide-failed-changed` handler that updates the shared `Filters` and calls
  `start_debounce(...)`; restore the saved value into the UI prop in the
  filter-restore block on startup.

### Testing

- **Migration:** extend the `migrations_are_idempotent_and_create_tables` test to
  include `thumbnail_failures`; assert `LATEST_MIGRATION_VERSION == 6` runs clean
  twice.
- **DB queries:** with an image that has a failure marker, assert
  `get_images_without_thumbnails` / `count_images_without_thumbnails` exclude it,
  and that `get_images_without_embedding` / `count_images_without_embedding` exclude
  an image whose 300px marker is set. Assert `clear_thumbnail_failures` re-includes
  it.
- **Filters serde:** round-trip `Filters` with `hide_failed = true`; assert old
  JSON without the field deserializes to `hide_failed = false`.
- **Filter clause:** `build_filter_clause_turso` with `hide_failed = true` emits the
  `thumbnail_failures` `NOT EXISTS` predicate; with `false` it does not.

### Docs

- `CLAUDE.md`: note the `thumbnail_failures` table under Storage, migration 006 /
  `LATEST_MIGRATION_VERSION = 6`, the failure-marking behavior in the indexing-flow
  / thumbnail description, the `process --retry-failed` flag, and the GUI
  hide-failed filter.
- `USAGE.md`: document `process --retry-failed` and the hide-failed filter toggle.

## Risks / tradeoffs

- **Single writer-thread channel change** in `thumbnail.rs` touches the hottest
  path; the enum refactor must preserve the existing success flow byte-for-byte.
- A transient failure (locked file on a flaky mount) is marked permanently; the
  `--retry-failed` flag is the recovery. Acceptable per the "mark after 1" decision.
- The `NOT EXISTS` subqueries add a per-row lookup to work-selection queries; the
  `idx_thumbnail_failures_hash` index keeps this cheap, and these queries are not
  hot-path search queries.
