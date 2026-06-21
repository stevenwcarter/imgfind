# Turso Storage Backend Migration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the synchronous `rusqlite` + `r2d2` + `sqlite-vec` storage stack with the async `turso` crate (native vector search), preserving all behavior, with a one-time migrator that carries existing embeddings/thumbnails forward.

**Architecture:** `Database` becomes an async core over turso, backed by an async connection pool. Sync callers (CLI, Slint GUI worker threads, the thumbnail writer thread) use a process-wide `block_on` bridge; the TUI (already on tokio) awaits directly. Vector search moves from `vec0` `MATCH` virtual tables to `F32_BLOB(dim)` columns + `vector_distance_cos` brute-force exact KNN. Schema versioning moves from `PRAGMA user_version` to a `schema_meta` table. A `migrate` subcommand reads the legacy DB (keeping rusqlite/sqlite-vec as transitional read-only deps) and writes a fresh turso DB in place.

**Tech Stack:** Rust edition 2024, `turso` (v0.4.x), `deadpool` (generic async pool), `tokio` (already a full dep), `anyhow`, `tracing`. Spec: `docs/superpowers/specs/2026-06-20-turso-migration-design.md`.

## Global Constraints

- Rust edition 2024; all workspace crates' editions must equal `rustfmt.toml`'s edition.
- Errors use `anyhow` with `.context()`/`.with_context()` everywhere.
- Dispatch all Rust coding to the `rust-developer` agent; code must be `cargo clippy`-clean and `cargo fmt --all`-clean (pre-empt `/tidy` and `/typecheck`).
- The **relative-path invariant** is unchanged: image paths are stored relative to the DB's parent dir; `Database.parent_dir` holds the base; convert at every FS boundary.
- The **canonical DB path is unchanged**: `.imgfind/imgfind.db`. `get_db_path`/`get_local_db_path` and walk-up resolution are NOT modified.
- Embeddings remain L2-normalized little-endian `f32`; cosine distance threshold stays `1.3`; `LATEST_MIGRATION_VERSION = 3`.
- `rusqlite`, `r2d2`, `r2d2_sqlite`, `sqlite_vec`, `zerocopy` may appear ONLY in `src/migrate.rs` after this plan. Their full removal is a SEPARATE follow-up PR (Task 9 records it; do not remove them here).
- Each task ends `cargo fmt --all` + `cargo clippy --workspace` clean and its stated build/test green before commit.

---

## File Structure

- `Cargo.toml` (root) — swap deps: drop nothing yet (migrator needs rusqlite/sqlite-vec), add `turso`, `deadpool`. (Removal of legacy deps is the follow-up PR.)
- `src/lib.rs` — add `block_on` bridge + `DB_RUNTIME`; export new modules.
- `src/db_pool.rs` (new) — async turso connection pool (deadpool Manager).
- `src/schema.rs` (new) — async schema runner (`schema_meta` + F32_BLOB vector tables) and `sanitize_model_table`.
- `src/database.rs` — rewritten async over turso; `pub pool` field removed; new `vectors_table`/vector-search SQL; consumes the turso filter clause.
- `src/filters.rs` — `build_filter_clause` returns a neutral param type (turso `Value`); no rusqlite.
- `src/thumbnail.rs` — background writer uses an async `Database` batch method via `block_on`; no direct pool/rusqlite.
- `src/models.rs` — async (`ensure_and_activate_model`, `open_db_seeding_default`).
- `src/main.rs` — CLI call sites wrapped in `block_on`; new `migrate` subcommand wiring; legacy-DB detection hint.
- `src/tui/**` — DB calls `.await` directly.
- `imgfind-gui/src/backend.rs` — worker-thread DB calls via `block_on`; no pool access.
- `src/migrate.rs` (new) — legacy reader (rusqlite+sqlite-vec) → turso writer; temp+rename+backup.
- Docs: `CLAUDE.md`, `README.md`, `USAGE.md`.

---

## Task 1: turso deps, runtime bridge, connection pool

**Files:**
- Modify: `Cargo.toml` (root, `[dependencies]`)
- Modify: `src/lib.rs`
- Create: `src/db_pool.rs`
- Test: inline `#[cfg(test)]` in `src/db_pool.rs`

**Interfaces:**
- Produces:
  - `imgfind::block_on<F: Future>(fut: F) -> F::Output` — runs a future on the shared multi-thread runtime (panics if called from within a tokio runtime; that is intended — only sync callers use it).
  - `imgfind::db_pool::TursoPool` with `async fn get(&self) -> anyhow::Result<deadpool::managed::Object<TursoManager>>` yielding a checked-out `turso::Connection` (deref to `&turso::Connection`), and `TursoPool::open(path: &std::path::Path, max_size: usize) -> anyhow::Result<TursoPool>`.

- [ ] **Step 1: Add deps**

In root `Cargo.toml` `[dependencies]`, add:

```toml
turso = "0.4"
deadpool = "0.12"
```

Leave `rusqlite`, `sqlite-vec`, `zerocopy`, `r2d2`, `r2d2_sqlite` in place (migrator still needs them).

- [ ] **Step 2: Write the failing pool test**

Create `src/db_pool.rs` with a test first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pool_opens_memory_db_and_runs_query() {
        let pool = TursoPool::open(std::path::Path::new(":memory:"), 4).unwrap();
        let conn = pool.get().await.unwrap();
        let mut rows = conn.query("SELECT 1", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get_value(0).unwrap().as_integer().copied(), Some(1));
    }
}
```

- [ ] **Step 3: Run it (fails to compile — type missing)**

Run: `cargo test -p imgfind db_pool`
Expected: FAIL (unresolved `TursoPool`).

- [ ] **Step 4: Implement the pool**

In `src/db_pool.rs`:

```rust
use anyhow::{Context, Result};
use deadpool::managed::{Manager, Metrics, Pool, RecycleResult};
use std::path::{Path, PathBuf};

pub struct TursoManager {
    db: turso::Database,
}

impl Manager for TursoManager {
    type Type = turso::Connection;
    type Error = turso::Error;

    async fn create(&self) -> Result<turso::Connection, turso::Error> {
        let conn = self.db.connect()?;
        // Per-connection PRAGMAs (turso supports WAL + foreign_keys).
        conn.execute("PRAGMA foreign_keys = ON", ()).await?;
        conn.execute("PRAGMA journal_mode = WAL", ()).await?;
        Ok(conn)
    }

    async fn recycle(&self, _: &mut turso::Connection, _: &Metrics) -> RecycleResult<turso::Error> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct TursoPool {
    inner: Pool<TursoManager>,
    pub path: PathBuf,
}

impl TursoPool {
    pub fn open(path: &Path, max_size: usize) -> Result<Self> {
        let db = imgfind_open_database(path)?;
        let mgr = TursoManager { db };
        let inner = Pool::builder(mgr)
            .max_size(max_size.max(1))
            .build()
            .context("build turso connection pool")?;
        Ok(Self { inner, path: path.to_path_buf() })
    }

    pub async fn get(&self) -> Result<deadpool::managed::Object<TursoManager>> {
        self.inner.get().await.context("checkout turso connection")
    }
}

fn imgfind_open_database(path: &Path) -> Result<turso::Database> {
    // Builder::new_local is async; block within a tiny local runtime is not
    // possible here (we may already be on a runtime). Open lazily instead:
    // the turso Builder build() is async, so expose open via async constructor.
    unreachable!("replaced in Step 5")
}
```

> NOTE: `turso::Builder::new_local(path).build()` is async. Because `TursoPool::open` is sync, make it `async fn open(...)` OR build the `turso::Database` by blocking on the shared runtime. Choose: make `open` async (`pub async fn open`) and have callers await/`block_on` it. Update Step 4 accordingly — the manager stores the already-built `turso::Database`.

- [ ] **Step 5: Finalize `open` as async**

Replace `imgfind_open_database` + `open` with:

```rust
impl TursoPool {
    pub async fn open(path: &Path, max_size: usize) -> Result<Self> {
        let db = turso::Builder::new_local(path.to_str().context("non-utf8 db path")?)
            .build()
            .await
            .with_context(|| format!("open turso database at {path:?}"))?;
        let inner = Pool::builder(TursoManager { db })
            .max_size(max_size.max(1))
            .build()
            .context("build turso connection pool")?;
        Ok(Self { inner, path: path.to_path_buf() })
    }
}
```

Update the test to `TursoPool::open(...).await.unwrap()`.

- [ ] **Step 6: Add the runtime bridge to `src/lib.rs`**

```rust
use std::future::Future;
use std::sync::LazyLock;

static DB_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build shared DB tokio runtime")
});

/// Run `fut` to completion on the shared runtime. For SYNC callers only
/// (CLI, GUI worker threads, the thumbnail writer). Panics if called from
/// inside a tokio runtime — the TUI must `.await` instead.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    DB_RUNTIME.block_on(fut)
}

pub mod db_pool;
pub mod schema; // added in Task 2
```

- [ ] **Step 7: Run tests + lints**

Run: `cargo test -p imgfind db_pool && cargo clippy --workspace --all-targets && cargo fmt --all --check`
Expected: PASS (the one pool test green; no new clippy/fmt issues).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/db_pool.rs
git commit -m "feat(db): add turso connection pool and block_on bridge"
```

---

## Task 2: async schema runner (`schema_meta` + F32_BLOB vector tables)

**Files:**
- Create: `src/schema.rs`
- Modify: `src/lib.rs` (already declares `pub mod schema;` from Task 1)
- Test: inline `#[cfg(test)]` in `src/schema.rs`

**Interfaces:**
- Consumes: `turso::Connection` (from `TursoPool::get`).
- Produces:
  - `imgfind::schema::LATEST_MIGRATION_VERSION: i32` (= 3).
  - `async fn imgfind::schema::run_migrations(conn: &turso::Connection) -> anyhow::Result<()>`.
  - `fn imgfind::schema::sanitize_model_table(name: &str) -> String` (moved verbatim from database.rs).
  - `async fn imgfind::schema::create_vector_table(conn: &turso::Connection, table: &str, dim: usize) -> anyhow::Result<()>` — creates `<table>(image_id INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE, embedding F32_BLOB(<dim>) NOT NULL)`.

- [ ] **Step 1: Write failing idempotency + schema test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    async fn mem() -> turso::Connection {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        db.connect().unwrap()
    }

    async fn table_exists(conn: &turso::Connection, name: &str) -> bool {
        let mut rows = conn
            .query("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [name])
            .await.unwrap();
        rows.next().await.unwrap().is_some()
    }

    #[tokio::test]
    async fn migrations_are_idempotent_and_create_tables() {
        let conn = mem().await;
        run_migrations(&conn).await.unwrap();
        run_migrations(&conn).await.unwrap(); // second run is a no-op

        for t in ["images", "image_vectors", "thumbnails", "image_metadata",
                  "favorites", "tags", "image_tags", "collections",
                  "collection_images", "models", "ui_state", "schema_meta"] {
            assert!(table_exists(&conn, t).await, "missing table {t}");
        }

        let mut rows = conn.query("SELECT version FROM schema_meta", ()).await.unwrap();
        let v = rows.next().await.unwrap().unwrap().get_value(0).unwrap().as_integer().copied();
        assert_eq!(v, Some(LATEST_MIGRATION_VERSION as i64));
    }

    #[tokio::test]
    async fn baseline_seeds_default_model() {
        let conn = mem().await;
        run_migrations(&conn).await.unwrap();
        let mut rows = conn.query(
            "SELECT dim, table_name, is_active FROM models WHERE name='openai/clip-vit-base-patch32'", ()
        ).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get_value(0).unwrap().as_integer().copied(), Some(512));
        assert_eq!(row.get_value(2).unwrap().as_integer().copied(), Some(1));
    }
}
```

- [ ] **Step 2: Run it (fails — module/functions missing)**

Run: `cargo test -p imgfind schema::`
Expected: FAIL (unresolved names).

- [ ] **Step 3: Implement the runner**

In `src/schema.rs`, port the three migrations to turso. Key changes vs the rusqlite version: gate on `schema_meta` not `PRAGMA user_version`; `image_vectors` is a real `F32_BLOB(512)` table (and per-model tables via `create_vector_table`), not a `vec0` virtual table. Everything else is verbatim DDL.

```rust
use anyhow::{Context, Result};

pub const LATEST_MIGRATION_VERSION: i32 = 3;

pub fn sanitize_model_table(name: &str) -> String {
    let s: String = name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    format!("image_vectors_{s}")
}

pub async fn create_vector_table(conn: &turso::Connection, table: &str, dim: usize) -> Result<()> {
    anyhow::ensure!(table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "invalid table name");
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {table} (\
                image_id INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE, \
                embedding F32_BLOB({dim}) NOT NULL)"
        ),
        (),
    ).await.with_context(|| format!("create vector table {table}"))?;
    Ok(())
}

async fn current_version(conn: &turso::Connection) -> Result<i32> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_meta (version INTEGER NOT NULL)", (),
    ).await?;
    let mut rows = conn.query("SELECT version FROM schema_meta LIMIT 1", ()).await?;
    if let Some(row) = rows.next().await? {
        Ok(row.get_value(0)?.as_integer().copied().unwrap_or(0) as i32)
    } else {
        conn.execute("INSERT INTO schema_meta (version) VALUES (0)", ()).await?;
        Ok(0)
    }
}

pub async fn run_migrations(conn: &turso::Connection) -> Result<()> {
    let current = current_version(conn).await?;
    if current < 1 { migration_001_baseline(conn).await.context("migration 1")?; }
    if current < 2 { migration_002_models_and_userdata(conn).await.context("migration 2")?; }
    if current < 3 { migration_003_ui_state(conn).await.context("migration 3")?; }
    if current < LATEST_MIGRATION_VERSION {
        conn.execute("UPDATE schema_meta SET version = ?1", [LATEST_MIGRATION_VERSION as i64]).await?;
    }
    Ok(())
}
```

Implement `migration_001_baseline` with the SAME `CREATE TABLE IF NOT EXISTS` statements as the current `src/database.rs` baseline (images, thumbnails, image_metadata, favorites + all indexes), EXCEPT replace the `vec0` virtual table with `create_vector_table(conn, "image_vectors", 512).await?`. Implement `migration_002_*` and `migration_003_*` verbatim from the current code (models seed row, tags/image_tags/collections/collection_images, ui_state). Each statement is a separate `conn.execute(...).await?` (turso may not support multi-statement `execute_batch`; issue one per call).

- [ ] **Step 4: Run tests — green**

Run: `cargo test -p imgfind schema::`
Expected: PASS (3 schema tests).

- [ ] **Step 5: Lints + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets
git add src/schema.rs
git commit -m "feat(db): async turso schema runner with schema_meta and F32_BLOB vector tables"
```

---

## Task 3: neutral filter params + vector-search round-trip

**Files:**
- Modify: `src/filters.rs`
- Create: `src/vector_sql.rs` (vector-search SQL builder)
- Modify: `src/lib.rs` (`pub mod vector_sql;`)
- Test: inline tests in both files

**Interfaces:**
- Produces:
  - `src/filters.rs`: `pub fn build_filter_clause(f: &Filters) -> (String, Vec<turso::Value>)` — same SQL clause, but params are `turso::Value` (no `rusqlite`). The boolean placeholder style stays `?` (turso positional).
  - `src/vector_sql.rs`: `pub fn knn_query(vt: &str, select: &str, extra_joins: &str, filter_clause: &str, limit: usize, offset: usize, threshold: f32) -> String` building the brute-force KNN SQL using a subquery that binds the embedding ONCE:

    ```sql
    SELECT <select> FROM (
      SELECT i.id AS id, i.path AS path, m.file_size AS file_size,
             vector_distance_cos(v.embedding, ?) AS distance
        FROM <vt> v JOIN images i ON i.id = v.image_id
        LEFT JOIN image_metadata m ON m.image_id = i.id
        <extra_joins>
    ) WHERE distance <= <threshold> <filter_clause>
      ORDER BY distance LIMIT <limit> OFFSET <offset>
    ```

    The single `?` for the embedding is the FIRST bound param; filter params follow.

- [ ] **Step 1: Failing test — filter clause uses turso::Value**

In `src/filters.rs` tests, adapt the existing filter tests so the returned vec is `Vec<turso::Value>`. Example assertion:

```rust
#[test]
fn size_bounds_emit_two_integer_params() {
    let mut f = Filters::default();
    f.min_size = Some(10);
    f.max_size = Some(20);
    let (clause, params) = build_filter_clause(&f);
    assert!(clause.contains("file_size"));
    assert!(matches!(params.as_slice(), [turso::Value::Integer(10), turso::Value::Integer(20)]));
}
```

- [ ] **Step 2: Run — fails (still returns rusqlite::Value)**

Run: `cargo test -p imgfind filters::`
Expected: FAIL (type mismatch).

- [ ] **Step 3: Convert `build_filter_clause`**

Replace `use rusqlite::types::Value;` with `use turso::Value;`. Map each pushed param: `Value::Integer(i64)` and `Value::Text(String)` exist in both; the construction is mechanically identical (turso `Value::Integer(i)`, `Value::Text(s)`). Keep the clause string and `?` placeholders unchanged. Confirm `Filters` no longer references rusqlite.

- [ ] **Step 4: Failing vector round-trip test**

In `src/vector_sql.rs` tests, build a real turso DB via the schema runner, insert two known L2-normalized vectors as raw blobs, and assert KNN returns them in distance order, plus the load-bearing blob round-trip:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::run_migrations;

    fn norm(v: &mut [f32]) { let n = v.iter().map(|x| x*x).sum::<f32>().sqrt(); for x in v { *x /= n; } }
    fn bytes(v: &[f32]) -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() }

    #[tokio::test]
    async fn blob_roundtrip_and_knn_orders_by_distance() {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        run_migrations(&conn).await.unwrap();
        conn.execute("INSERT INTO images (id, path, hash) VALUES (1,'a',' '),(2,'b',' ')", ()).await.unwrap();

        let mut a = vec![1.0f32, 0.0, 0.0, 0.0]; // (image_vectors is F32_BLOB(512); use 512 in real test)
        // For the real test, build 512-dim vectors; this doc shows the shape.
        norm(&mut a);
        // Plan A: bind raw LE f32 blob.
        conn.execute("INSERT INTO image_vectors (image_id, embedding) VALUES (1, ?1)",
                     [turso::Value::Blob(bytes(&a))]).await.unwrap();

        // Read back, assert byte-exact.
        let mut rows = conn.query("SELECT embedding FROM image_vectors WHERE image_id=1", ()).await.unwrap();
        let got = rows.next().await.unwrap().unwrap().get_value(0).unwrap().as_blob().unwrap().to_vec();
        assert_eq!(got, bytes(&a));
    }
}
```

> The real test MUST use 512-dim vectors to match `F32_BLOB(512)`. If Plan-A blob binding fails at runtime, fall back to inserting via `vector32('[...]')` text (format the f32 slice as a `[..]` string) and record the decision in a code comment + the spec's §3.

- [ ] **Step 5: Implement `knn_query` + run tests green**

Implement the `knn_query` string builder per the Interfaces block. Run:

Run: `cargo test -p imgfind filters:: vector_sql::`
Expected: PASS.

- [ ] **Step 6: Lints + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets
git add src/filters.rs src/vector_sql.rs src/lib.rs
git commit -m "feat(db): turso filter params and brute-force KNN vector SQL builder"
```

---

## Task 4: convert `Database` + CLI to async turso (imgfind crate green, default features)

This is the core flip. At its end, `cargo build` and `cargo test` (default features, i.e. no `tui`) are green for the `imgfind` crate. The TUI (`tui` feature) and the GUI crate are handled in Tasks 6–7.

**Files:**
- Modify: `src/database.rs` (rewrite all methods async over turso; remove `pub pool`, the `vec0` FFI init, `BusyTimeoutCustomizer`; use `TursoPool`, `schema::run_migrations`, `vector_sql::knn_query`, turso filter clause)
- Modify: `src/models.rs` (async)
- Modify: `src/thumbnail.rs` (writer uses async `Database` batch method via `block_on`)
- Modify: `src/main.rs` (wrap CLI call sites in `block_on`)
- Test: inline `#[cfg(test)]` in `src/database.rs` (ported to `#[tokio::test]`)

**Interfaces:**
- Consumes: `TursoPool`, `schema::{run_migrations, sanitize_model_table, create_vector_table}`, `vector_sql::knn_query`, `filters::build_filter_clause` (turso params), `block_on`.
- Produces: `Database` with every method now `pub async fn` (same names/params/returns as the spec Appendix A list). New struct shape:

  ```rust
  #[derive(Clone)]
  pub struct Database { pool: db_pool::TursoPool, pub parent_dir: PathBuf }
  ```
  - `pub async fn Database::new(db_path: &Path) -> Result<Self>` — creates parent dir, opens `TursoPool` (size `available_parallelism().min(32)`), runs `schema::run_migrations` on a checked-out conn.
  - `pub async fn insert_thumbnails_batch(&self, items: &[(String, u32, Vec<u8>)]) -> Result<()>` — NEW method the writer thread calls (replaces direct pool access in `thumbnail.rs`).

- [ ] **Step 1: Port the DB test module to `#[tokio::test]` (write failing tests first)**

Convert the inline test module in `src/database.rs`: each `#[test] fn` → `#[tokio::test] async fn`; each `Database::new(...)?` → `Database::new(...).await?`; each method call `.await`. Port the helpers `temp_db_path`, `cleanup`, `test_db_with_rows` (the last becomes `async`). Keep every named test from spec Appendix A's port list. Rename `migrations_set_user_version_and_create_tables` → `migrations_set_schema_meta_and_create_tables` and assert on `schema_meta.version`. DROP `foreign_keys_enabled_on_fresh_pool_connection` and `busy_timeout_set_on_fresh_pool_connection` (pool internals gone); add instead:

```rust
#[tokio::test]
async fn delete_image_cascades_to_vectors_and_metadata() {
    let (db, path) = test_db_with_rows(&[("a.jpg", Some(100))]).await;
    // insert an embedding + metadata for the image, delete the image row,
    // assert the vector row and metadata row are gone (ON DELETE CASCADE).
    cleanup(&path);
}

#[tokio::test]
async fn concurrent_writes_do_not_deadlock() {
    // spawn N tasks each inserting a thumbnail through the pool; all succeed.
}
```

- [ ] **Step 2: Run the ported tests (fail — Database still rusqlite)**

Run: `cargo test -p imgfind database::`
Expected: FAIL to compile (async mismatch) — confirms the suite drives the rewrite.

- [ ] **Step 3: Rewrite `Database` struct + `new`**

Replace the struct, `new`, `checkpoint_wal`, and remove the sqlite-vec FFI init + `BusyTimeoutCustomizer`. Pattern:

```rust
impl Database {
    pub async fn new(db_path: &Path) -> Result<Self> {
        let parent_path = db_path.parent().context("DB path has no parent")?;
        std::fs::create_dir_all(parent_path).context("create DB parent dir")?;
        let max_size = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8).min(32);
        let pool = db_pool::TursoPool::open(db_path, max_size).await?;
        let parent_dir = get_db_parent_dir(db_path)?;
        let conn = pool.get().await?;
        crate::schema::run_migrations(&conn).await?;
        drop(conn);
        Ok(Self { pool, parent_dir })
    }

    pub async fn checkpoint_wal(&self) -> Result<()> {
        let conn = self.pool.get().await?;
        conn.execute("PRAGMA wal_checkpoint(RESTART)", ()).await.context("wal_checkpoint")?;
        Ok(())
    }
}
```

- [ ] **Step 4: Rewrite the remaining methods by translation pattern**

Apply these mechanical patterns to every method in spec Appendix A. Translation table (rusqlite → turso):

| rusqlite | turso |
|---|---|
| `self.pool.get()?` | `self.pool.get().await?` |
| `conn.query_row(sql, p, \|r\| r.get(0))` | `query` + `rows.next().await?` then `row.get_value(0)?` + typed accessor |
| `conn.execute(sql, params![...])` | `conn.execute(sql, (..)).await?` or `params!` |
| `stmt.query_map(p, f)?` loop | `let mut rows = conn.query(sql, p).await?; while let Some(row) = rows.next().await? { ... }` |
| `.optional()?` | `rows.next().await?` → `Option<Row>` |
| `conn.transaction()?` … `tx.commit()?` | `let tx = conn.transaction().await?;` … `tx.commit().await?` |
| `embedding.as_bytes()` (zerocopy) | `turso::Value::Blob(embedding.iter().flat_map(\|f\| f.to_le_bytes()).collect())` |
| `row.get::<_, i64>(0)?` | `row.get_value(0)?.as_integer().copied().context(..)? ` |
| `row.get::<_, String>(0)?` | `row.get_value(0)?.as_text().context(..)?.to_string()` |
| `row.get::<_, Option<i64>>(0)?` | match `row.get_value(0)?` Null → None else Integer |

Representative complete conversion — `active_model` + `vectors_table` + a search:

```rust
pub async fn active_model(&self) -> Result<ModelInfo> {
    let conn = self.pool.get().await?;
    let mut rows = conn.query(
        "SELECT name, dim, table_name FROM models WHERE is_active = 1 LIMIT 1", ()
    ).await?;
    let row = rows.next().await?.context("no active model")?;
    Ok(ModelInfo {
        name: row.get_value(0)?.as_text().context("name")?.to_string(),
        dim: row.get_value(1)?.as_integer().copied().context("dim")? as usize,
        table: row.get_value(2)?.as_text().context("table_name")?.to_string(),
    })
}

async fn vectors_table(&self) -> Result<String> {
    let t = self.active_model().await?.table;
    anyhow::ensure!(t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "invalid table name");
    Ok(t)
}

pub async fn search_similar_images_meta(
    &self, query_embedding: &[f32], limit: usize, offset: usize,
    distance_threshold: f32, max_k: usize, filters: &Filters,
) -> Result<Vec<RankedMetaRow>> {
    let k = (offset + limit).max(1).max(max_k);
    let vt = self.vectors_table().await?;
    let (clause, fparams) = build_filter_clause(filters);
    let sql = crate::vector_sql::knn_query(
        &vt, "id, path, distance, file_size", "", &clause, limit.min(k), offset, distance_threshold);
    let conn = self.pool.get().await?;
    let mut params = vec![turso::Value::Blob(to_le_bytes(query_embedding))];
    params.extend(fparams);
    let mut rows = conn.query(&sql, turso::params_from_iter(params)).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push((
            row.get_value(0)?.as_integer().copied().context("id")?,
            row.get_value(1)?.as_text().context("path")?.to_string(),
            row.get_value(2)?.as_real().copied().unwrap_or(0.0) as f32,
            match row.get_value(3)? { turso::Value::Integer(i) => Some(i), _ => None },
        ));
    }
    Ok(out)
}
```

Add a private helper `fn to_le_bytes(v: &[f32]) -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() }`. Update `insert_image`/`insert_images_batch` to write `INSERT INTO {vt} (image_id, embedding) VALUES (?1, ?2)` with `[Value::Integer(id), Value::Blob(to_le_bytes(emb))]`, and the `DELETE FROM {vt} WHERE image_id = ?1` pre-delete. Update `find_similar_to_path` to read `SELECT embedding FROM {vt} WHERE image_id = ?1`. Update `is_image_indexed` `EXISTS (SELECT 1 FROM {vt} WHERE image_id = i.id)`. Update `search_similar_images`, `search_similar_images_with_raw_blob` (keep the `thumbnails` LEFT JOIN via `knn_query`'s `extra_joins`/`select`), `clean_missing_files` (`DELETE FROM {vt} WHERE image_id=?1`).

- [ ] **Step 5: Add `insert_thumbnails_batch` and convert `src/thumbnail.rs`**

Add the async batch method:

```rust
pub async fn insert_thumbnails_batch(&self, items: &[(String, u32, Vec<u8>)]) -> Result<()> {
    if items.is_empty() { return Ok(()); }
    let conn = self.pool.get().await?;
    let tx = conn.transaction().await?;
    for (hash, size, data) in items {
        tx.execute(
            "INSERT OR REPLACE INTO thumbnails (image_hash, size, thumbnail_data) VALUES (?1, ?2, ?3)",
            (hash.clone(), *size as i64, data.clone()),
        ).await?;
    }
    tx.commit().await?;
    Ok(())
}
```

In `src/thumbnail.rs`, the writer thread (a `std::thread`) replaces `db.pool.get()` + rusqlite `params!` with `imgfind::block_on(db.insert_thumbnails_batch(&batch))?`. Remove `use rusqlite::params;` and the `db.pool` access (now private). No rusqlite remains in this file.

- [ ] **Step 6: Convert `src/models.rs` to async**

`ensure_and_activate_model` and `open_db_seeding_default` become `async fn`; `Database::new(...).await`, `db.list_models().await`, `db.register_model(...).await`, `db.set_active_model(...).await`. `list_rows` (CLI helper) becomes async or wraps with `block_on` at its CLI caller.

- [ ] **Step 7: Convert CLI call sites in `src/main.rs`**

Wrap each `Database`/db-method call with `imgfind::block_on(...)`. The 7 `Database::new` sites become `block_on(Database::new(&db_path))?` (or `block_on(open_db_seeding_default(...))?`). Each `db.foo(args)?` → `block_on(db.foo(args))?`. `db.parent_dir` field reads stay as-is. Do NOT make `main` itself async — keep it sync and bridge per-call (the spec's sync-bridge model). Leave the `migrate` subcommand stubbed (`todo!()` is NOT allowed — return `anyhow::bail!("run after Task 8")` placeholder is acceptable only if Task 8 is the very next task; instead add the `Migrate` variant in Task 8 to avoid a placeholder here).

- [ ] **Step 8: Build + test (default features) green**

Run: `cargo build -p imgfind && cargo test -p imgfind`
Expected: PASS (all ported DB tests, filters, schema, vector_sql, pool). If blob-binding (Plan A) fails, switch to the `vector32(text)` fallback in `insert_*`/search and re-run.

- [ ] **Step 9: Lints + commit**

```bash
cargo fmt --all && cargo clippy -p imgfind --all-targets
git add src/database.rs src/models.rs src/thumbnail.rs src/main.rs src/filters.rs
git commit -m "feat(db): convert Database core and CLI to async turso"
```

---

## Task 5: verify CLI end-to-end against a real turso DB

**Files:**
- Test: `tests/cli_smoke.rs` (new integration test) OR a manual smoke documented here.

**Interfaces:** Consumes the built `imgfind` binary / library `Database`.

- [ ] **Step 1: Write an integration test that indexes a tiny fixture and searches**

Create `tests/cli_smoke.rs`:

```rust
// Uses the library directly (no clipper model download): build a Database in a
// temp dir, insert a known image+embedding via the public async API through
// block_on, then assert search returns it. Keeps the test hermetic.
#[test]
fn index_and_search_roundtrip_through_block_on() {
    let dir = std::env::temp_dir().join(format!("imgfind_cli_{}", std::process::id()));
    let db_path = dir.join(".imgfind/imgfind.db");
    let mut db = imgfind::block_on(imgfind::database::Database::new(&db_path)).unwrap();
    // ... insert image + 512-dim normalized embedding, search, assert hit ...
    std::fs::remove_dir_all(&dir).ok();
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p imgfind --test cli_smoke`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/cli_smoke.rs
git commit -m "test(db): hermetic CLI index/search roundtrip on turso"
```

---

## Task 6: convert the TUI to await directly (`tui` feature)

**Files:**
- Modify: `src/tui/**` (every `Database` method call)

**Interfaces:** Consumes async `Database`. The TUI runs on a tokio runtime — call `.await` directly; NEVER `block_on`.

- [ ] **Step 1: Build with the feature to surface every break**

Run: `cargo build -p imgfind --features tui`
Expected: FAIL — compile errors at each DB call (futures not awaited). Use these as the worklist.

- [ ] **Step 2: Add `.await` at each TUI DB call**

For each error site in `src/tui/app.rs` (and submodules under `app/`), add `.await`. Where a DB call happens inside a non-async closure passed to ratatui, move it into the async event-loop body or a spawned task that already exists (the TUI uses `tokio::select!` + channels — search results are already produced async; route DB reads through the same async context). Do not introduce `block_on`.

- [ ] **Step 3: Build + run TUI tests green**

Run: `cargo build -p imgfind --features tui && cargo test -p imgfind --features tui`
Expected: PASS.

- [ ] **Step 4: Lints + commit**

```bash
cargo fmt --all && cargo clippy -p imgfind --features tui --all-targets
git add src/tui
git commit -m "feat(tui): await async turso Database calls"
```

---

## Task 7: convert the Slint GUI crate (`imgfind-gui`)

**Files:**
- Modify: `imgfind-gui/src/backend.rs` (worker-thread DB calls via `block_on`; remove pool access)
- Modify: `imgfind-gui/Cargo.toml` (port/drop rusqlite/zerocopy dev-deps)
- Modify: other `imgfind-gui/src/**` files that call `Database` methods

**Interfaces:** Consumes async `Database`. GUI work happens on `std::thread` workers (no tokio runtime) — use `imgfind::block_on(...)`.

- [ ] **Step 1: Build the workspace to surface breaks**

Run: `cargo build --workspace`
Expected: FAIL at GUI DB call sites.

- [ ] **Step 2: Wrap each GUI DB call in `block_on`**

In `imgfind-gui/src/backend.rs`: `self.db.browse_all(...)` → `imgfind::block_on(self.db.browse_all(...))`, and likewise `rehydrate_rows`, `get_ui_state`, `set_ui_state`, `list_tags`, and any others. Replace the direct `db.pool.get()` paths (now private) with the appropriate async `Database` method via `block_on`. Confirm these run on worker closures, not on any tokio runtime (the Slint event loop is not tokio — `block_on` is safe here).

- [ ] **Step 3: Fix GUI tests + dev-deps**

In `imgfind-gui/Cargo.toml`, the `rusqlite`/`zerocopy` dev-deps were used to hand-build test DBs. Port those tests to the async `Database` API (via `block_on`) and remove the dev-deps, OR keep them only if a test genuinely needs raw SQLite (it should not). Update test bodies accordingly.

- [ ] **Step 4: Build + test the workspace green**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Lints + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets
git add imgfind-gui
git commit -m "feat(gui): bridge async turso Database via block_on in worker threads"
```

---

## Task 8: one-time `migrate` subcommand + legacy detection

**Files:**
- Create: `src/migrate.rs` (legacy rusqlite+sqlite-vec reader → turso writer)
- Modify: `src/main.rs` (`Migrate` subcommand; legacy-DB detection hint on open failure)
- Modify: `src/lib.rs` (`pub mod migrate;`)
- Test: inline `#[cfg(test)]` in `src/migrate.rs`

**Interfaces:**
- Produces: `pub async fn migrate::migrate_db(canonical_path: &Path, force: bool) -> anyhow::Result<MigrateOutcome>` where `MigrateOutcome { AlreadyMigrated, Migrated { images: usize, embeddings: usize, thumbnails: usize } }`. Reads the legacy DB at `canonical_path` (rusqlite + sqlite-vec auto-extension), writes a fresh turso DB to `canonical_path.with_extension("db.turso.tmp")`, renames the legacy file to `*.rusqlite.bak` (refuse if exists unless `force`), renames the temp into `canonical_path`.

- [ ] **Step 1: Failing migrator fidelity test (load-bearing)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrates_images_embeddings_thumbnails_with_stable_ids() {
        // 1. Build a legacy DB with rusqlite + sqlite-vec: create the OLD schema
        //    (vec0 virtual table), insert 3 images (ids 1,2,3), 3 known 512-dim
        //    normalized embeddings, 2 thumbnail blobs, metadata, a tag, a
        //    non-default model row + its vec0 table.
        // 2. Run migrate_db(path, false).await.
        // 3. Open the resulting turso DB; assert: same image ids/paths/hashes;
        //    byte-exact embeddings (read F32_BLOB, compare to source bytes);
        //    byte-exact thumbnail blobs; tag/metadata present; a KNN search
        //    returns the same nearest order as the source.
        // 4. Assert the legacy file now exists as *.rusqlite.bak.
    }

    #[tokio::test]
    async fn already_migrated_is_noop() {
        // Given a fresh turso DB at the path, migrate_db reports AlreadyMigrated.
    }
}
```

- [ ] **Step 2: Run — fails (module missing)**

Run: `cargo test -p imgfind migrate::`
Expected: FAIL.

- [ ] **Step 3: Implement the reader**

In `src/migrate.rs`, init the sqlite-vec auto-extension (as the current `Database::new` does), open the legacy DB with `rusqlite::Connection::open`, and read each table into Vecs: `models` (name, dim, table_name, is_active), `images` (id, path, hash, created_at), per-model embeddings (`SELECT rowid, embedding FROM <vec0 table>` → `(id, Vec<u8>)`), `thumbnails`, `image_metadata`, `favorites`, `tags`, `image_tags`, `collections`, `collection_images`, `ui_state`. Decode embeddings as raw bytes (no f32 round-trip needed — copy the blob verbatim).

- [ ] **Step 4: Implement the writer + atomic swap**

Build the turso DB at the temp path: `TursoPool::open(tmp, 1).await?`, `schema::run_migrations`, then for each non-default model call `schema::create_vector_table`. Insert rows with EXPLICIT ids (`INSERT INTO images (id, path, hash, created_at) VALUES (?1,?2,?3,?4)`), embeddings as `Value::Blob` into the right per-model table, thumbnails/metadata/tags/etc. verbatim, all inside transactions. Then:

```rust
let bak = canonical_path.with_extension("db.rusqlite.bak");
anyhow::ensure!(force || !bak.exists(), "backup {bak:?} exists; pass --force to overwrite");
std::fs::rename(canonical_path, &bak).context("back up legacy DB")?;
std::fs::rename(&tmp, canonical_path).context("install migrated DB")?;
```

Detect AlreadyMigrated by probing the canonical file: if turso opens it and `schema_meta` exists, return `AlreadyMigrated` without touching anything.

- [ ] **Step 5: Wire the `Migrate` subcommand + detection hint**

In `src/main.rs`, add `Commands::Migrate { #[arg(long)] force: bool }` and dispatch `block_on(migrate::migrate_db(&db_path, force))?`, printing the outcome counts. In the DB-open path used by other commands, when `Database::new` / `open_db_seeding_default` fails to open the canonical file as a turso DB (or detects no `schema_meta` while `images` exists — implementer verifies which probe works against turso beta), print to stderr: `legacy database detected; run \`imgfind migrate\` to upgrade it to the new engine` and exit non-zero cleanly (no panic/backtrace).

- [ ] **Step 6: Tests green**

Run: `cargo test -p imgfind migrate::`
Expected: PASS (fidelity + already-migrated).

- [ ] **Step 7: Lints + commit**

```bash
cargo fmt --all && cargo clippy -p imgfind --all-targets
git add src/migrate.rs src/main.rs src/lib.rs
git commit -m "feat(db): one-time migrate subcommand from rusqlite/sqlite-vec to turso"
```

---

## Task 9: docs + record the dependency-retirement follow-up

**Files:**
- Modify: `CLAUDE.md`, `README.md`, `USAGE.md`
- Create: `docs/superpowers/specs/2026-06-20-turso-dep-retirement-followup.md` (a short note, NOT executed in this branch)

- [ ] **Step 1: Update `CLAUDE.md`**

In the Storage section, replace the rusqlite/r2d2/sqlite-vec/`vec0` description with: turso async engine; `F32_BLOB(dim)` vector tables searched via `vector_distance_cos` brute-force KNN; `schema_meta`-gated migration runner (`LATEST_MIGRATION_VERSION = 3`); the async-core + `block_on`-bridge model (sync CLI/GUI/thumbnail-writer bridge, TUI awaits). Add the `migrate` subcommand to the CLI-commands list and note the canonical path is unchanged. Note `rusqlite`/`sqlite-vec` survive only in `src/migrate.rs` pending the retirement PR.

- [ ] **Step 2: Update `README.md` + `USAGE.md`**

Document `imgfind migrate [--force]`: "Upgrade an existing database created by a previous (sqlite-vec) version to the new turso engine; preserves embeddings and thumbnails. The old file is kept as `imgfind.db.rusqlite.bak`."

- [ ] **Step 3: Write the follow-up note**

Create the follow-up spec: a one-page checklist to (a) confirm the author's own library migrated cleanly, then (b) delete `src/migrate.rs` + the `Migrate` subcommand + the `rusqlite`/`r2d2`/`r2d2_sqlite`/`sqlite_vec`/`zerocopy` deps, in a dedicated PR. List the exact files/lines.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md README.md USAGE.md docs/superpowers/specs/2026-06-20-turso-dep-retirement-followup.md
git commit -m "docs: turso migration, migrate command, and dep-retirement follow-up"
```

---

## Final: review + finish branch

- [ ] Dispatch the final code-reviewer subagent over the whole branch diff (spec compliance + quality). Resolve Critical/Important findings.
- [ ] Run full verification: `cargo fmt --all --check && cargo clippy --workspace --all-targets && cargo build --workspace && cargo test --workspace && cargo test -p imgfind --features tui`.
- [ ] Invoke `superpowers:finishing-a-development-branch`.

---

## Self-Review (completed)

- **Spec coverage:** Engine swap (T1–T4), async core + bridge (T1/T4/T6/T7), pool (T1), schema_meta runner + F32_BLOB tables (T2), vector KNN + threshold parity (T3/T4), neutral filter params (T3), `pub pool` removal + thumbnail writer (T4), models async (T4), CLI/TUI/GUI call sites (T4/T6/T7), migrator + detection + temp/rename/backup (T8), tests incl. load-bearing blob round-trip (T3), distance parity (T3/T4), migrator fidelity (T8), concurrency (T4), docs + retirement follow-up (T9). All spec sections map to a task.
- **Placeholder scan:** Mechanical method conversions are specified by an exhaustive method list (spec Appendix A) + complete representative patterns + a translation table, not "etc." The one risky spot (Plan-A blob binding) has an explicit fallback path and a load-bearing test that forces the decision early. The `migrate` placeholder in T4 Step 7 is resolved by ordering T8 immediately after (the `Migrate` variant is added in T8, not stubbed in T4).
- **Type consistency:** `image_id` (not `rowid`) is the vector-table key everywhere (schema T2, search/insert/find T4, migrator T8). `build_filter_clause` returns `Vec<turso::Value>` in T3 and is consumed that way in T4. `block_on` signature is consistent across T1/T4/T6/T7/T8. `TursoPool::open` is async in T1 and awaited in T4/T8.
