# Storage backend migration: rusqlite + sqlite-vec → turso

**Date:** 2026-06-20
**Status:** Design — approved (via `/ship-it --ask`)
**Topic:** Replace the synchronous `rusqlite` + `r2d2` + `sqlite-vec` storage stack
with the async `turso` crate (the pure-Rust SQLite rewrite, formerly Limbo),
including native vector search and a one-time data migrator.

---

## 1. Motivation & goals

`imgfind` stores CLIP embeddings and image metadata in a local SQLite database.
Today that runs on `rusqlite` (synchronous C SQLite bindings) + an `r2d2`
connection pool + the `sqlite-vec` C extension (`vec0` virtual tables) for
vector KNN search.

The goal is to move the storage backend to the [`turso`](https://crates.io/crates/turso)
crate — a from-scratch async Rust reimplementation of SQLite with **native**
vector search built in. This removes the C dependency (`sqlite-vec` and the
bundled SQLite C lib), gets the project onto an actively-developed pure-Rust
engine, and folds vector search into the core engine instead of a loadable
extension.

**This is a like-for-like backend swap.** Same logical schema, same features,
same observable behavior. No new product features ride along. Success =
every existing CLI command, the TUI, and the Slint GUI behave identically,
the test suite passes against turso, and an existing rusqlite database can be
migrated into a turso database with no loss of embeddings, thumbnails, or
metadata (no re-indexing).

### Non-goals

- No ANN / approximate vector index. turso's ANN index is still roadmap; our
  current `vec0` search is already brute-force *exact* KNN, so exact search in
  turso is **parity, not a regression**. (If turso ships an ANN index later,
  adopting it is a separate, additive change.)
- No new schema features, no new columns, no behavioral changes.
- No change to the embedding pipeline (`clipper`), the decode seam, or any
  frontend logic beyond what the async DB boundary forces.

---

## 2. Decisions (locked in brainstorming)

| Decision | Choice | Rationale |
|---|---|---|
| Engine | **`turso` crate** (Rust rewrite, beta) | User intent: ride the pure-Rust rewrite; accept beta risk. Native vectors, no C extension. |
| Async model | **Async core + sync bridge** | `Database` methods become `async`; sync CLI/GUI-worker callers use a `block_on` bridge; the TUI (already on tokio) awaits directly. |
| Migration | **One-time migrator, then retire old deps** | Preserve expensive CLIP embeddings + thumbnails. Keep `rusqlite`/`sqlite-vec` as transitional read-only deps; a planned follow-up removes them. |

---

## 3. turso facts this design relies on

Verified against turso's COMPAT.md, vector docs, and the Rust crate docs
(beta, v0.4.x):

**Supported and relied on:** WAL journal mode; `PRAGMA foreign_keys` with
`ON DELETE CASCADE`; transactions (`BEGIN`/`COMMIT`/`ROLLBACK`); `INSERT … ON
CONFLICT … DO UPDATE` (UPSERT); `AUTOINCREMENT`; `CREATE INDEX` including
partial indexes (`… WHERE …`); native vectors via `F32_BLOB(dim)` columns and
`vector_distance_cos(a, b)` (cosine distance in `[0, 2]`, same semantics as the
current `vec0` distance, so the existing `1.3` threshold carries over).

**Rust API shape:**
`turso::Builder::new_local(path).build().await?` → `Database`;
`db.connect()?` → `Connection`;
`conn.execute(sql, params).await?` → rows-affected;
`conn.query(sql, params).await?` → `Rows`; `rows.next().await?` → `Option<Row>`;
`row.get_value(i)?` → `turso::Value` (with `as_integer`/`as_text`/`as_blob`/…);
`conn.prepare(sql).await?` → `Statement`; `params!` / `named_params!` /
`params_from_iter` for binding; a `Transaction` type for explicit transactions;
`Value::Blob(Vec<u8>)` for byte blobs.

### Two flags designed around

1. **Schema versioning does not rely on `PRAGMA user_version` writes.** turso's
   COMPAT notes `user_version` writes may be no-op'd under defensive mode. The
   migration runner moves to an explicit `schema_meta(version INTEGER NOT NULL)`
   table (single row). The runner stays idempotent and `IF NOT EXISTS`-based.

2. **Embedding blobs: bind raw bytes first, `vector32(text)` as fallback.** Our
   embeddings are L2-normalized little-endian `f32`. Plan A binds the raw
   `&[u8]` blob to the `F32_BLOB(dim)` column (matching today's zerocopy path).
   libsql issue #1903 shows blob-binding friction; documented fallback is to
   format the vector as `'[f0, f1, …]'` text and insert via
   `vector32('[…]')`. The implementer verifies Plan A with a round-trip test
   before committing to it; falls back only if binding fails.

---

## 4. Architecture

### 4.1 Async core (`Database`)

`Database` becomes an async type over turso. It holds:
- the turso `Database` handle (the connection factory), and
- a connection pool (see §4.3),
- `parent_dir: PathBuf` (unchanged — the relative-path invariant base).

Every current `pub fn` on `impl Database` becomes `pub async fn` with the same
name, parameters, and logical return type. Bodies are rewritten from
rusqlite calls to turso calls (`query`/`execute`/`prepare` + `.await`, row
extraction via `get_value`/typed accessors). The relative-path conversion
boundary, the `models`-registry-driven `vectors_table()` resolution, the
transaction-wrapped batch writes, and the SQL-identifier validation all carry
over unchanged in shape.

rusqlite types must not leak from the public API or shared seams:

- **`src/filters.rs`** — `build_filter_clause` currently returns
  `(String, Vec<rusqlite::types::Value>)`, consumed by both `browse*` and the
  filtered vector searches. This becomes a neutral param representation (turso
  `Value`, or a small local `ParamValue` enum mapped to turso at the call site)
  so `filters.rs` no longer depends on rusqlite.
- **The public `pub pool` field is removed.** Today `src/thumbnail.rs` (the
  background JPEG writer, on a `std::thread`) and a few `imgfind-gui/src/backend.rs`
  paths reach into `db.pool.get()` directly and run rusqlite `params!` writes.
  These move to async `Database` methods (e.g. a batch thumbnail-insert method)
  invoked via the §4.2 `block_on` bridge from the writer thread. `Database`
  exposes no connection-pool type publicly.
- `src/models.rs` (`ensure_and_activate_model`, `open_db_seeding_default`) calls
  `list_models`/`register_model`/`set_active_model`/`Database::new`; it becomes
  async (or uses the bridge), and `Database::new(...).await` propagates to all
  seven CLI construction sites.

### 4.2 Sync bridge

A single process-wide tokio runtime backs the sync callers:

```rust
// e.g. in lib.rs
static DB_RUNTIME: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Builder::new_multi_thread()
        .enable_all().build().expect("DB runtime"));

pub fn block_on<F: Future>(fut: F) -> F::Output { DB_RUNTIME.block_on(fut) }
```

- **CLI (`main.rs`)** and **Slint GUI worker threads** (`std::thread`): wrap each
  call site `db.foo(…).await` → `block_on(db.foo(…))`. No method duplication.
- **TUI** (already on a tokio runtime): call `db.foo(…).await` directly. Never
  `block_on` from inside the TUI runtime (would panic).

No `BlockingDatabase` facade type — the bridge is the free `block_on` helper at
sync call sites. This keeps a single async API surface.

### 4.3 Connection pool

`r2d2` (sync) is replaced by an async pool of turso `Connection`s, sized
`available_parallelism().min(32)` (matching today). Recommended:
[`deadpool`](https://crates.io/crates/deadpool) with a thin custom `Manager`
that calls `db.connect()`; fallback is a hand-rolled async pool
(`Mutex<Vec<Connection>>` + `Semaphore` checkout). Each pooled connection is
initialized once with `PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;`
(turso applies WAL/foreign-keys per the COMPAT support above). The
`busy_timeout` customizer is dropped; turso's async model handles contention by
awaiting rather than blocking a thread — verify lock-contention behavior with a
concurrent-write test (mirrors the thumbnail-worker + grid-query contention).

### 4.4 Vector storage & search

Keep the **multi-model** design: the `models` registry row gives `(name, dim,
table_name, is_active)`; each model owns its own vector table. The change is the
table *kind*:

- **Before:** `image_vectors` (+ `vectors_<model>`) as `vec0` virtual tables
  (`embedding float[dim]`, `rowid` = image id).
- **After:** regular tables
  `<table_name>(image_id INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE
  CASCADE, embedding F32_BLOB(<dim>) NOT NULL)`.

`register_model` creates the per-model `F32_BLOB` table; `sanitize_model_table`
and the identifier validation in `vectors_table()` are unchanged.

Search rewrites the `vec0` `MATCH … k=N … distance <= t ORDER BY distance`
idiom to brute-force exact KNN:

```sql
SELECT i.id, i.path, vector_distance_cos(v.embedding, ?) AS distance, m.file_size
  FROM <vt> v
  JOIN images i ON i.id = v.image_id
  LEFT JOIN image_metadata m ON m.image_id = i.id
 WHERE vector_distance_cos(v.embedding, ?) <= <threshold>  -- expr repeated; no alias in WHERE
   <filter clause>
 ORDER BY distance
 LIMIT <limit> OFFSET <offset>
```

Notes:
- The query embedding binds as a blob param (same Plan-A/fallback rule as §3).
  The distance expression appears twice (SELECT alias + WHERE predicate) because
  SQL disallows the alias in `WHERE`; the `?` blob is therefore bound twice —
  use positional/`params_from_iter` ordering carefully, or bind via a CTE/
  subquery that computes distance once (implementer's call; subquery preferred
  to bind the embedding once and keep filter-param ordering simple).
- `k` clamping/`max_k` logic is preserved as the `LIMIT` (plus the
  offset+limit-vs-max_k flooring in `search_similar_images_meta`).
- `find_similar_to_path` still reads the stored embedding blob
  (`SELECT embedding FROM <vt> WHERE image_id = ?`) and re-runs the search with
  it; the f32 decode from the blob is unchanged.
- The `image_vectors_with_raw_blob` thumbnail join is unchanged in shape.

### 4.5 Schema runner & migrations

`run_migrations(conn)` becomes async and gates on the `schema_meta` table
instead of `PRAGMA user_version`:

1. `CREATE TABLE IF NOT EXISTS schema_meta(version INTEGER NOT NULL)`; if empty,
   seed `0`.
2. Read current version; apply baseline (001), models/userdata (002), ui_state
   (003) in order, each idempotent (`IF NOT EXISTS`).
3. Update `schema_meta.version` to `LATEST_MIGRATION_VERSION` (= 3) after
   success.

Baseline schema is identical **except** the `image_vectors` virtual table is now
a regular `F32_BLOB` table (per §4.4), and any other `vec0`-specific DDL is
replaced. All other tables (`images`, `thumbnails`, `image_metadata`,
`favorites`, `tags`, `image_tags`, `collections`, `collection_images`,
`ui_state`, `models`) are plain SQLite and migrate verbatim.

---

## 5. One-time migrator

A new explicit subcommand: **`imgfind migrate`**.

- **Reader:** opens the existing rusqlite + sqlite-vec database read-only. The
  `rusqlite`, `r2d2_sqlite`, `sqlite_vec`, and `zerocopy` deps stay in the tree,
  confined to a single `src/migrate.rs` (and its `sqlite-vec` auto-extension
  init) so the rest of the codebase is turso-only.
- **Canonical path is unchanged.** The turso DB keeps the existing canonical
  location `.imgfind/imgfind.db`, so `get_db_path` / `get_local_db_path` and the
  walk-up resolution logic need **no** change. The migrator writes the new turso
  DB to a temp file (`imgfind.db.turso.tmp`), then atomically renames the legacy
  file aside to `imgfind.db.rusqlite.bak` and the temp into place. The backup is
  kept (not deleted) so the user can roll back.
- **Writer:** runs the turso schema runner on the fresh DB, then streams every
  table: `models` (registry), `images`, per-model embeddings (decoded f32 from
  the old `vec0` table → `F32_BLOB` rows), `thumbnails`, `image_metadata`,
  `favorites`, `tags`, `image_tags`, `collections`, `collection_images`,
  `ui_state`. Image ids are preserved (explicit `id` inserts) so all foreign
  keys and per-model embedding links stay intact. Writes batch inside transactions.
- **Detection.** A DB "needs migration" when the canonical file exists but is
  not a valid turso DB at the current schema. Detection probe (in priority
  order, implementer picks the one that works against turso beta): (a) turso
  fails to open the file at all (likely if turso rejects the `vec0` virtual-table
  DDL) → legacy; (b) turso opens it but the `schema_meta` table is absent while
  `images` exists → legacy. **Early-verify which holds** — it changes only the
  probe, not feasibility (worst case: open-error ⇒ legacy). On detecting a
  legacy DB, the CLI/GUI prints one line — `legacy database detected; run
  \`imgfind migrate\` to upgrade it to the new engine` — and exits that command
  cleanly instead of crashing or silently rebuilding.
- **Idempotency / safety.** If `imgfind.db` is already a valid turso DB,
  `imgfind migrate` reports "already migrated" and does nothing. It refuses to
  overwrite an existing `.rusqlite.bak` unless `--force` is passed (prevents a
  second run from clobbering the original backup).
- **Retirement:** once the author confirms their own library migrates cleanly,
  a **planned follow-up PR** deletes `src/migrate.rs`, the `migrate` subcommand,
  and the `rusqlite`/`r2d2`/`r2d2_sqlite`/`sqlite_vec`/`zerocopy` deps. Tracked
  as an explicit task in the plan, not done in this branch.

### Invariants this feature depends on

- **Stored embeddings are L2-normalized little-endian f32**, readable via the
  old `SELECT embedding FROM <vec0 table> WHERE rowid = ?` path (the existing
  `find_similar_to_path` decode proves this). The migrator depends on this to
  copy embeddings without re-running CLIP.
- **`vector_distance_cos` on L2-normalized vectors == `1 − cosine similarity`**,
  matching `vec0`'s cosine distance, so the `1.3` distance threshold preserves
  result sets. Pinned by a parity test (§6).
- **Image ids are stable and shared** across `images` and every per-model vector
  table. The migrator preserves ids so the multi-model linkage survives.

---

## 6. Testing

Reworked DB suite runs against turso (in-memory or temp-file turso DBs):

- **Schema runner idempotency:** running the runner twice leaves
  `schema_meta.version = 3` and all tables present.
- **Round-trip:** `insert_image` / `insert_images_batch` → `search_similar_images*`
  returns the inserted rows in distance order; `is_image_indexed` reflects
  presence per active model.
- **Blob-binding round-trip (load-bearing):** insert an embedding via Plan A
  (raw blob), read it back, assert byte-exact f32 recovery. This is the test
  that decides Plan A vs the `vector32(text)` fallback — write it first.
- **Distance-threshold parity:** with known L2-normalized vectors, assert
  `vector_distance_cos` ranks/filters identically to the old `vec0` expectation
  at threshold `1.3`.
- **Tags / favorites / collections / metadata:** CRUD round-trips, including
  `ON DELETE CASCADE` (delete an image → its tags/metadata/embeddings vanish).
- **Concurrent contention:** N concurrent writers + readers through the pool do
  not deadlock or error (replaces the `busy_timeout` guarantee).
- **Migrator fidelity (load-bearing, cross-engine seam):** build a small
  old-format rusqlite+sqlite-vec DB with the legacy code path (a handful of
  images, embeddings, thumbnails, metadata, tags, a non-default model). Run the
  migrator. Assert: identical image ids, byte-exact embeddings, byte-exact
  thumbnail blobs, identical metadata/tags/favorites, and that a vector search
  on the migrated DB returns the same ordering as the source.

The TUI and GUI keep their existing tests; only the DB-call boundary changes
(await/block_on), which the type system enforces.

---

## 7. Risks

- **turso beta gaps.** A SQL feature or PRAGMA we rely on could be missing or
  buggy. Mitigation: the §3 facts are verified; the blob-binding and
  distance-parity tests are written first and fail loudly. If a hard blocker
  surfaces, the async + native-vector rewrite is ~90% reusable against the
  mature `libsql` crate (documented fallback engine, not pursued unless forced).
- **Connection thread-safety.** If turso `Connection` is `!Send`/`!Sync`, the
  pool must keep each connection bound to one task at a time (it does — checkout
  model) and the multi-thread runtime must not move a borrowed connection across
  threads mid-statement. Verified by the concurrency test.
- **Double-bind of the query embedding** in the search SQL — mitigated by the
  subquery/CTE form that binds it once.

---

## Appendix A — call-site & dependency inventory

**`Database` public surface → all become `async fn`** (same names/params/returns):
`new`, `checkpoint_wal`, `get_ui_state`, `set_ui_state`, `active_model`,
`register_model`, `set_active_model`, `list_models`, `insert_image`,
`insert_images_batch`, `is_image_indexed`, `search_similar_images`,
`search_similar_images_with_raw_blob`, `search_similar_images_with_blob`,
`search_similar_images_meta`, `find_similar_to_path`, `browse`, `browse_all`,
`rehydrate_rows`, `distinct_extensions`, `file_size_bounds`, `get_image_count`,
`get_sample_images`, `insert_thumbnail`, `get_thumbnail`, `get_image_hash`,
`get_images_without_thumbnails`, `count_images_without_thumbnails`,
`insert_or_update_metadata`, `get_images_without_metadata`, `get_image_metadata`,
`get_image_id`, `get_images_by_bounds`, `create_tag`, `tag_image`,
`untag_image`, `batch_tag_images`, `batch_untag_images`, `tags_for_image`,
`list_tags`, `images_by_tag`, `create_collection`, `add_to_collection`,
`remove_from_collection`, `collection_images`, `list_collections`,
`toggle_favorite`, `is_favorite`, `list_favorites`, `clean_missing_files`.
Unchanged exports: structs `ModelInfo`/`ImageMetadata`/`ImageWithMetadata`,
free fns `extract_image_metadata`/`downsample_by_grid`/`apply_stable_jitter`,
type aliases `ImageSearchResult`/`RankedMetaRow`.

**Sync call sites (CLI `src/main.rs`)** — 7 `Database::new` sites (lines ~191,
197, 244, 265, 270, 282, 289) + method calls at ~414, 528, 608, 629, 631, 688,
703, 768, 781, 903, 920, 924, 1065 (and `db.parent_dir` field reads, which stay
sync). All wrapped with `block_on`. `src/models.rs` (~lines 14, 26, 39, 50, 51,
67) on the same bridge.

**Sync call sites (GUI workers `imgfind-gui/src/backend.rs`)** — `browse_all`
(~112), `rehydrate_rows` (~117), `get_ui_state` (~124), `set_ui_state` (~129),
`list_tags` (~227), plus direct `db.pool.get()` paths (~300, 330, 382, 435, 464,
505) that migrate to async methods. Slint event handlers run on worker
closures; use `block_on`.

**Background-thread writer (`src/thumbnail.rs`)** — `db.pool.get()` (~50, 81) +
rusqlite `params!` execute (~120) on a `std::thread`. Replace with an async
`Database` thumbnail-write method called via `block_on`.

**Async call sites (TUI `src/tui/app.rs`)** — `App.db: Database` (~54, 109);
DB access happens inside the tokio event loop / spawned tasks — call `.await`
directly, never `block_on`.

**rusqlite/r2d2/sqlite-vec/zerocopy to remove (outside migrator):**
`src/database.rs` (imports + ~50 call sites; `pub pool` field; FFI
`sqlite3_auto_extension`/`sqlite3_vec_init`; `BusyTimeoutCustomizer`),
`src/filters.rs` (`rusqlite::types::Value`), `src/thumbnail.rs` (`params!`,
pool). The migrator (`src/migrate.rs`) is the **only** place these crates remain.

**Cargo manifests:** root `Cargo.toml` drops `rusqlite`/`sqlite-vec`/`zerocopy`/
`r2d2_sqlite`/`r2d2` from `[dependencies]` and adds `turso` (+ pool crate, e.g.
`deadpool`); the migrator's rusqlite/sqlite-vec deps are kept until the
retirement PR (gate behind a `migrate` feature if cleaner). `imgfind-gui/Cargo.toml`
has `rusqlite`/`zerocopy` as **dev-deps** for its tests — port or drop those.
`tokio` is already a full dep.

**Existing DB tests to port** (inline `#[cfg(test)]` in `database.rs`, → `#[tokio::test]`):
`migrations_set_user_version_and_create_tables` (rename — now `schema_meta`),
`migrations_are_idempotent`, `migration_2_seeds_baseline_model_and_user_tables`,
`active_model_defaults_to_baseline_table`,
`register_and_switch_model_creates_table_and_flips_active`,
`is_image_indexed_is_model_aware`, `search_meta_paginates_past_first_page`,
`search_meta_paginates_past_max_k`,
`find_similar_to_path_returns_neighbors_from_stored_embedding`,
`filtered_vector_search_excludes_nonmatching_types`,
`browse_filters_by_size_type_and_gps`, `distinct_extensions_and_size_bounds`,
`browse_all_sorts_by_size_then_name_nulls_last`, `browse_all_sorts_by_type_then_name`,
`browse_all_name_desc`, `browse_all_default_filters_returns_all`,
`rehydrate_preserves_order_and_drops_missing`, `rehydrate_empty_is_empty`,
`rehydrate_rows_ordered_with_metadata_populated`, `ui_state_round_trips_through_db`,
`toggle_favorite_flips_state`, `tag_and_collection_roundtrip`,
`get_image_metadata_reads_stored_row_without_decoding`,
`delete_image_cascades_to_metadata`,
`missing_thumbnail_limit_bind_uses_real_count_not_usize_max`,
`extracts_metadata_from_raw_fixture` (free-fn, no DB). The
`foreign_keys_enabled_on_fresh_pool_connection` and
`busy_timeout_set_on_fresh_pool_connection` tests are replaced by the §6
FK-cascade and concurrency tests.
