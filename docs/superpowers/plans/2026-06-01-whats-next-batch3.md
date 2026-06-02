# whats-next batch 3 (2026-06-01) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement W50 (path newtypes), W52 (multi-model embedding storage), W36 (tags/collections), W45 (GraphQL search), W46 (lazy CLIP init + readiness), W53 (parallel metadata), W38 (shell completions), W44 (TUI score labels) on branch `whats-next-batch/2026-06-01-b`.

**Architecture:** Introduce `RelativePath`/`AbsolutePath` newtypes at DB boundaries; add migration 2 (a `models` registry + per-model vec0 tables, and tags/collections tables); parameterize all `image_vectors` access by the active model's table; wire real GraphQL search; load the CLIP model lazily in `serve()` with a `/healthz` + 503-until-ready; parallelize metadata extraction with rayon; add `completions` + TUI score labels.

**Tech Stack:** Rust 2024 (rusqlite, r2d2, sqlite-vec, axum, juniper, clap+clap_complete, rayon, tokio), `../clipper` (single-model, dim 512 — see caveat).

**Reference:** spec `docs/superpowers/specs/2026-06-01-whats-next-batch3-design.md` (read first — esp. the W52 clipper caveat: storage infra only; clipper emits one model). Migration system: bump `LATEST_MIGRATION_VERSION`, add `migration_002_*`, `if current < 2` block in `run_migrations`.

**Execution order:** T1 (newtypes) → T2 (migration 2 schema) → T3 (active-model infra + parameterize) → T4 (W52 CLI) → T5 (tag/collection methods) → T6 (GraphQL search+tags) → T7 (lazy embedder) → T8 (rayon metadata) → T9 (completions) → T10 (TUI score). Sequential (heavy database.rs contention across T1–T5).

---

## Task 1: `RelativePath`/`AbsolutePath` newtypes (W50)

**Files:** Modify `src/lib.rs` (defs + the two conversion fns ~88-99), `src/database.rs` (path-handling method signatures + 8 call sites), `src/main.rs` (index loop call sites ~463,517), plus any api/tui callers the compiler flags.

- [ ] **Step 1: Write failing tests** (in `src/lib.rs` `#[cfg(test)]`)

```rust
#[test]
fn rel_abs_roundtrip_within_base() {
    let base = Path::new("/data");
    let abs = AbsolutePath(PathBuf::from("/data/sub/a.jpg"));
    let rel = abs.to_relative(base).unwrap();
    assert_eq!(rel.0, PathBuf::from("sub/a.jpg"));
    assert_eq!(rel.to_absolute(base).0, abs.0);
}
#[test]
fn to_relative_errors_outside_base() {
    let base = Path::new("/data");
    let abs = AbsolutePath(PathBuf::from("/other/a.jpg"));
    assert!(abs.to_relative(base).is_err());
}
```

- [ ] **Step 2: Run → fail.** `cargo test rel_abs` → FAIL (types undefined).

- [ ] **Step 3: Define the newtypes** in `src/lib.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelativePath(pub PathBuf);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AbsolutePath(pub PathBuf);

impl RelativePath {
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> { self.0.to_string_lossy() }
    pub fn to_absolute(&self, base: &Path) -> AbsolutePath { AbsolutePath(base.join(&self.0)) }
}
impl AbsolutePath {
    pub fn as_path(&self) -> &Path { &self.0 }
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> { self.0.to_string_lossy() }
    pub fn to_relative(&self, base: &Path) -> Result<RelativePath> {
        self.0.strip_prefix(base).map(|p| RelativePath(p.to_path_buf()))
            .context("Path is not within database parent directory")
    }
}
```

Reimplement `abs_to_relative_path`/`relative_to_abs_path` as thin shims over these (keep their signatures so unrelated callers still build), OR migrate callers — your call, but the build must stay green.

- [ ] **Step 4: Run → pass.** `cargo test rel_abs` → PASS.

- [ ] **Step 5: Thread newtypes through Database path methods.** Read each of the 10 call sites (spec lists them). Convert signatures: `insert_image`, `is_image_indexed`, `get_image_id` take `&AbsolutePath`; `get_sample_images`, `get_images_without_thumbnails`, `get_images_without_metadata`, `get_images_by_bounds` return `AbsolutePath` (in their tuples); `toggle_favorite`/`is_favorite`/`list_favorites`/`get_image_hash` take/return `RelativePath`. **Boundary rule:** at REST/GraphQL/JSON edges, convert to/from plain `String` so serialized output is unchanged (search methods may keep returning `String` relative paths — wrap internally only where it adds safety). Update all callers (main.rs index loop, api/search.rs, tui) to construct the right newtype. This is mechanical; lean on the compiler.

- [ ] **Step 6: Build + test.** `cargo build && cargo test` → green (38+ tests + new). Keep the diff focused on path types; do not run `cargo fmt` on the whole repo (only `rustfmt` individual files you edit if needed).

- [ ] **Step 7: Commit.**
```bash
git add src/lib.rs src/database.rs src/main.rs src/api src/tui
git commit -m "refactor(paths): RelativePath/AbsolutePath newtypes at DB boundaries [W50]"
```

---

## Task 2: Migration 2 — models registry + tags/collections schema (W52+W36)

**Files:** Modify `src/database.rs` (`run_migrations`, `LATEST_MIGRATION_VERSION`, add `migration_002_*`).

- [ ] **Step 1: Write failing tests** (`src/database.rs` tests):

```rust
#[test]
fn migration_2_seeds_baseline_model_and_user_tables() {
    let db = /* temp-db fixture */;
    let conn = db.pool.get().unwrap();
    let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, LATEST_MIGRATION_VERSION); // now 2
    // models seeded: exactly one active baseline pointing at image_vectors
    let (name, dim, table, active): (String, i64, String, i64) = conn.query_row(
        "SELECT name, dim, table_name, is_active FROM models WHERE is_active = 1", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!((dim, table.as_str(), active), (512, "image_vectors", 1));
    assert!(name.contains("clip"));
    for t in ["tags","image_tags","collections","collection_images","models"] {
        let n: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE name=?1",[t],|r| r.get(0)).unwrap();
        assert_eq!(n, 1, "table {t} exists");
    }
}
```

- [ ] **Step 2: Run → fail.** `cargo test migration_2` → FAIL.

- [ ] **Step 3: Implement migration 2.** Set `const LATEST_MIGRATION_VERSION: i32 = 2;`. In `run_migrations`, after the `if current < 1 {...}` block add:

```rust
if current < 2 {
    migration_002_models_and_userdata(conn).context("migration 2")?;
}
conn.execute_batch(&format!("PRAGMA user_version = {LATEST_MIGRATION_VERSION};"))?;
```

(Keep a single trailing stamp to LATEST, or stamp per-migration — match the existing style; stamping once at the end to LATEST after applying all pending is fine since each `if current < N` guards its own work.)

Add:
```rust
fn migration_002_models_and_userdata(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS models (
            name TEXT PRIMARY KEY, dim INTEGER NOT NULL, table_name TEXT NOT NULL,
            is_active INTEGER NOT NULL DEFAULT 0, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
         INSERT OR IGNORE INTO models (name, dim, table_name, is_active)
            VALUES ('openai/clip-vit-base-patch32', 512, 'image_vectors', 1);
         CREATE TABLE IF NOT EXISTS tags (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
         CREATE TABLE IF NOT EXISTS image_tags (image_id INTEGER NOT NULL, tag_id INTEGER NOT NULL, PRIMARY KEY(image_id, tag_id),
            FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE, FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE);
         CREATE TABLE IF NOT EXISTS collections (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
         CREATE TABLE IF NOT EXISTS collection_images (collection_id INTEGER NOT NULL, image_id INTEGER NOT NULL, PRIMARY KEY(collection_id, image_id),
            FOREIGN KEY(collection_id) REFERENCES collections(id) ON DELETE CASCADE, FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE);",
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run → pass.** `cargo test migration` → PASS (incl. the existing migration-1/idempotency tests, which now also run migration 2).

- [ ] **Step 5: Commit.**
```bash
git add src/database.rs
git commit -m "feat(db): migration 2 — models registry + tags/collections tables [W52,W36]"
```

---

## Task 3: active-model infra + parameterize vec0 access (W52)

**Files:** Modify `src/database.rs` (add ModelInfo + active_model/vectors_table/set_active_model/register_model; parameterize the 6 image_vectors sites).

- [ ] **Step 1: Write failing tests:**

```rust
#[test]
fn active_model_defaults_to_baseline_table() {
    let db = /* temp-db fixture */;
    let m = db.active_model().unwrap();
    assert_eq!(m.dim, 512);
    assert_eq!(m.table, "image_vectors");
}
#[test]
fn register_and_switch_model_creates_table_and_flips_active() {
    let db = /* temp-db fixture */;
    db.register_model("test-model", 256).unwrap();
    db.set_active_model("test-model").unwrap();
    let m = db.active_model().unwrap();
    assert_eq!((m.dim, m.table.as_str()), (256, "image_vectors_test_model"));
    // vec0 table exists
    let n: i64 = db.pool.get().unwrap().query_row(
        "SELECT count(*) FROM sqlite_master WHERE name='image_vectors_test_model'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}
```

- [ ] **Step 2: Run → fail.**

- [ ] **Step 3: Implement.**

```rust
#[derive(Debug, Clone)]
pub struct ModelInfo { pub name: String, pub dim: usize, pub table: String }

fn sanitize_model_table(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    format!("image_vectors_{s}")
}

impl Database {
    pub fn active_model(&self) -> Result<ModelInfo> {
        let conn = self.pool.get().context("get connection")?;
        let (name, dim, table): (String, i64, String) = conn.query_row(
            "SELECT name, dim, table_name FROM models WHERE is_active = 1 LIMIT 1", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .context("no active model")?;
        Ok(ModelInfo { name, dim: dim as usize, table })
    }
    fn vectors_table(&self) -> Result<String> {
        let t = self.active_model()?.table;
        // defensive: table names come from our registry, but validate anyway
        anyhow::ensure!(t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "invalid table name");
        Ok(t)
    }
    pub fn register_model(&self, name: &str, dim: usize) -> Result<()> {
        let table = sanitize_model_table(name);
        let conn = self.pool.get().context("get connection")?;
        conn.execute("INSERT OR IGNORE INTO models (name, dim, table_name, is_active) VALUES (?1, ?2, ?3, 0)",
            params![name, dim as i64, table])?;
        conn.execute_batch(&format!("CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(embedding float[{dim}]);"))?;
        Ok(())
    }
    pub fn set_active_model(&self, name: &str) -> Result<()> {
        let conn = self.pool.get().context("get connection")?;
        let exists: bool = conn.query_row("SELECT 1 FROM models WHERE name=?1", [name], |_| Ok(())).optional()?.is_some();
        anyhow::ensure!(exists, "unknown model: {name}");
        conn.execute("UPDATE models SET is_active = (name = ?1)", [name])?;
        Ok(())
    }
    pub fn list_models(&self) -> Result<Vec<(String, usize, bool)>> {
        let conn = self.pool.get().context("get connection")?;
        let mut stmt = conn.prepare("SELECT name, dim, is_active FROM models ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)? as usize, r.get::<_,i64>(2)? != 0)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>,_>>()?)
    }
}
```

- [ ] **Step 4: Parameterize the 6 `image_vectors` sites.** In `insert_image`, `insert_images_batch`, `clean_missing_files`, `search_similar_images`, `search_similar_images_with_raw_blob`, `search_similar_images_meta`: replace the literal `image_vectors` in each SQL string with `let vt = self.vectors_table()?;` interpolated into the `format!`-built SQL (these methods already `format!` the k/threshold; add `{vt}`). The rowid linkage to `images.id` is unchanged. Confirm each still binds the embedding identically.

- [ ] **Step 5: Run → pass.** `cargo test` → all green (existing search/insert tests now resolve the active table to `image_vectors`).

- [ ] **Step 6: Commit.**
```bash
git add src/database.rs
git commit -m "feat(db): active-model registry + parameterize vec0 table access [W52]"
```

---

## Task 4: W52 CLI — `--model` flag + `models` subcommand (W52)

**Files:** Modify `src/main.rs` (Commands enum + Index/Search args + dispatch).

- [ ] **Step 1: Add the subcommand + flags.** Add `Commands::Models { #[command(subcommand)] action: ModelsAction }` with `enum ModelsAction { List, Use { name: String } }`. Add `#[arg(long)] model: Option<String>` to `Index` and `Search`.

- [ ] **Step 2: Implement handlers.** `Models::List` → print `db.list_models()` (mark active with `*`). `Models::Use { name }` → `db.set_active_model(&name)?` + confirm. For `Index`/`Search`, if `--model` given, `db.set_active_model(&model)?` before running (so the run uses that active table). (No new-model registration UX yet — clipper single-model caveat; `register_model` exists for future use.)

- [ ] **Step 3: Build.** `cargo build` → compiles.

- [ ] **Step 4: Commit.**
```bash
git add src/main.rs
git commit -m "feat(cli): models list/use + --model flag [W52]"
```

---

## Task 5: tag/collection Database methods (W36)

**Files:** Modify `src/database.rs` (methods mirroring favorites; use `&RelativePath`). Tests inline.

- [ ] **Step 1: Write failing test** (reuse the fixture that inserts an image at a known relative path, e.g. `a.jpg`):

```rust
#[test]
fn tag_and_collection_roundtrip() {
    let db = /* temp-db fixture with image at rel path "a.jpg" */;
    let p = RelativePath(PathBuf::from("a.jpg"));
    db.tag_image(&p, "cats").unwrap();
    assert_eq!(db.tags_for_image(&p).unwrap(), vec!["cats".to_string()]);
    assert_eq!(db.images_by_tag("cats").unwrap(), vec![p.clone()]);
    db.untag_image(&p, "cats").unwrap();
    assert!(db.tags_for_image(&p).unwrap().is_empty());

    db.create_collection("trip").unwrap();
    db.add_to_collection("trip", &p).unwrap();
    assert_eq!(db.collection_images("trip").unwrap(), vec![p.clone()]);
    assert_eq!(db.list_collections().unwrap(), vec!["trip".to_string()]);
}
```

- [ ] **Step 2: Run → fail.**

- [ ] **Step 3: Implement** (mirror favorites' image_id-by-path resolution; `tag_image` upserts the tag then links). Methods: `create_tag(&self,name)->Result<i64>`, `tag_image(&self,&RelativePath,name)`, `untag_image(&self,&RelativePath,name)`, `tags_for_image(&self,&RelativePath)->Vec<String>`, `list_tags(&self)->Vec<String>`, `images_by_tag(&self,name)->Vec<RelativePath>`, `create_collection(&self,name)->Result<i64>`, `add_to_collection(&self,name,&RelativePath)`, `remove_from_collection(&self,name,&RelativePath)`, `collection_images(&self,name)->Vec<RelativePath>`, `list_collections(&self)->Vec<String>`. Resolve image_id via `SELECT id FROM images WHERE path = ?1` with `RelativePath::as_str()`. `tag_image`: `INSERT OR IGNORE INTO tags(name)`, get id, `INSERT OR IGNORE INTO image_tags`.

- [ ] **Step 4: Run → pass.** `cargo test tag_and_collection` → PASS.

- [ ] **Step 5: Commit.**
```bash
git add src/database.rs
git commit -m "feat(db): tag & collection CRUD methods [W36]"
```

---

## Task 6: GraphQL — real search + tags/collections (W45+W36)

**Files:** Modify `src/graphql.rs`.

- [ ] **Step 1: Add `ImageResult` + implement `search`.** Replace the stub:

```rust
#[derive(GraphQLObject)]
pub struct ImageResult { pub path: String, pub distance: f64, pub file_size: Option<i32> }
```
```rust
#[graphql(name = "search")]
pub fn search(context: &GraphQLContext, query: String, limit: Option<i32>, offset: Option<i32>) -> FieldResult<Vec<ImageResult>> {
    let emb = context.embedder_ready().ok_or_else(|| FieldError::new("model still loading", juniper::Value::null()))?
        .get_text_embedding(&query)?;
    let sc = crate::config::SearchConfig::default();
    let rows = crate::search::SearchEngine::new(&context.db)
        .search_meta(emb, limit.unwrap_or(80) as usize, offset.unwrap_or(0) as usize, sc.distance_threshold, sc.max_k)?;
    Ok(rows.into_iter().map(|(path, distance, file_size)|
        ImageResult { path, distance: distance as f64, file_size: file_size.map(|s| s as i32) }).collect())
}
```
(`embedder_ready()` is added in Task 7; until then, if Task 6 runs first, use `context.embedder.get_text_embedding(&query)?` and Task 7 swaps it. Per build order Task 6 is before Task 7 — so use the current `context.embedder` Arc access here and let Task 7 update it.)

CORRECTION (build order): Task 6 precedes Task 7, so in Task 6 use `context.embedder.get_text_embedding(&query)?` (the current Arc<ClipEmbedder>). Task 7 will refactor this call site for readiness.

- [ ] **Step 2: Add tag/collection resolvers.** Queries: `tags`, `tagsForImage(path)`, `collections`, `collectionImages(name)`, `imagesByTag(name)` → delegate to the Task-5 Database methods, converting `RelativePath`→`String` via `.as_str().to_string()`. Mutations on `Mutation`: `createTag(name)->bool`, `tagImage(path,tag)->bool`, `untagImage(path,tag)->bool`, `createCollection(name)->bool`, `addToCollection(name,path)->bool`, `removeFromCollection(name,path)->bool` (construct `RelativePath(PathBuf::from(path))`).

- [ ] **Step 3: Build.** `cargo build` → compiles (juniper macros expand).

- [ ] **Step 4: Commit.**
```bash
git add src/graphql.rs
git commit -m "feat(graphql): real search (ImageResult) + tag/collection queries & mutations [W45,W36]"
```

---

## Task 7: lazy CLIP init + /healthz (W46)

**Files:** Modify `src/context.rs` (embedder holder + `embedder_ready`), `src/main.rs` (serve spawns loader), `src/routes.rs` (/healthz), `src/api/search.rs` + `src/graphql.rs` (readiness handling).

- [ ] **Step 1: Change the embedder holder.** In `GraphQLContext`, replace `embedder: Arc<ClipEmbedder>` with `embedder: Arc<std::sync::OnceLock<ClipEmbedder>>` (std OnceLock — set-once, thread-safe, no async needed for reads). Add:
```rust
pub fn embedder_ready(&self) -> Option<&ClipEmbedder> { self.embedder.get() }
```
Update `GraphQLContext::new` to take the `Arc<OnceLock<ClipEmbedder>>`.

- [ ] **Step 2: serve() spawns the loader.** In `serve()`:
```rust
let cell: Arc<std::sync::OnceLock<ClipEmbedder>> = Arc::new(std::sync::OnceLock::new());
{
    let cell = cell.clone();
    tokio::task::spawn_blocking(move || {
        match ClipEmbedder::new(None, None, false) {
            Ok(m) => { let _ = cell.set(m); info!("CLIP model loaded"); }
            Err(e) => error!("CLIP model load failed: {e:#}"),
        }
    });
}
let context = GraphQLContext::new(db, directory, cell);
```
Start axum immediately (unchanged below).

- [ ] **Step 3: /healthz route.** In `app()` (routes.rs) add `.route("/healthz", get(healthz))` at the top level; handler:
```rust
async fn healthz(Extension(ctx): Extension<GraphQLContext>) -> impl IntoResponse {
    let status = if ctx.embedder_ready().is_some() { "ready" } else { "loading" };
    axum::Json(serde_json::json!({ "model": status }))
}
```

- [ ] **Step 4: REST search 503-until-ready.** In `src/api/search.rs` handler, before embedding:
```rust
let Some(embedder) = context.embedder_ready() else {
    return Ok((axum::http::StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::RETRY_AFTER, "5")],
        "model loading").into_response());
};
let query_embedding = embedder.get_text_embedding(search.as_str())?;
```
(Adjust the return type to `impl IntoResponse`; the success path wraps `Json` as today.)

- [ ] **Step 5: GraphQL readiness.** Update the Task-6 `search` resolver to use `context.embedder_ready().ok_or_else(|| FieldError::new("model still loading", juniper::Value::null()))?` instead of `context.embedder.get_text_embedding`.

- [ ] **Step 6: Build.** `cargo build` → compiles. (Index/CLI paths still construct ClipEmbedder directly — unchanged.)

- [ ] **Step 7: Commit.**
```bash
git add src/context.rs src/main.rs src/routes.rs src/api/search.rs src/graphql.rs
git commit -m "feat(serve): lazy CLIP init, /healthz, 503-until-ready [W46]"
```

---

## Task 8: parallel metadata extraction (W53)

**Files:** Modify `src/main.rs` (Phase-2 metadata loop ~513-537), `src/metadata.rs` (`extract_missing_metadata` loop).

- [ ] **Step 1: Parallelize Phase-2 extraction.** Replace the sequential per-image metadata loop with a rayon parallel extract + sequential DB write:
```rust
use rayon::prelude::*;
let extracted: Vec<(String, Result<ImageMetadata>)> = survivors
    .par_iter()
    .map(|(abs, _, _)| (abs.clone(), crate::database::extract_image_metadata(abs)))
    .collect();
for (abs, res) in extracted {
    match res {
        Ok(metadata) => { if let Ok(id) = db.get_image_id(&AbsolutePath(PathBuf::from(&abs))) { let _ = db.insert_or_update_metadata(id, &metadata); } }
        Err(e) => { warn!("metadata extract failed for {abs}: {e:#}"); }
    }
}
```
(Adapt to the actual survivor tuple shape + the W50 newtype for `get_image_id`.)

- [ ] **Step 2: Parallelize the backfill** in `extract_missing_metadata` similarly (par_iter the extract, serial writes), preserving existing counts/quiet behavior.

- [ ] **Step 3: Build + test.** `cargo build && cargo test` → green.

- [ ] **Step 4: Commit.**
```bash
git add src/main.rs src/metadata.rs
git commit -m "perf(index): parallel (rayon) metadata extraction [W53]"
```

---

## Task 9: shell completions (W38)

**Files:** Modify `Cargo.toml` (add dep), `src/main.rs` (Completions subcommand + handler).

- [ ] **Step 1: Add dep.** `Cargo.toml`: `clap_complete = "4.0"`.

- [ ] **Step 2: Add subcommand + handler.**
```rust
// in Commands:
/// Generate a shell completion script
Completions { shell: clap_complete::Shell },
```
```rust
// handler (needs `use clap::CommandFactory;`):
Commands::Completions { shell } => {
    clap_complete::generate(shell, &mut Cli::command(), "imgfind", &mut std::io::stdout());
}
```

- [ ] **Step 3: Build + smoke.** `cargo build` then `cargo run -- completions bash | head -5` shows a script.

- [ ] **Step 4: Commit.**
```bash
git add Cargo.toml Cargo.lock src/main.rs
git commit -m "feat(cli): completions command (bash/zsh/fish) via clap_complete [W38]"
```

---

## Task 10: TUI score labels (W44)

**Files:** Modify `src/tui/widget/image.rs` (`render_image`).

- [ ] **Step 1: Render the score.** After the image renders (before `Ok(center)`), draw a compact label from `image_entry.score`. Reserve the bottom row of the cell (or set it as the block title): build `let label = format!("{:.3}", image_entry.score);` and render a right-aligned `Line`/`Span` (style DarkGray) into the bottom row of `area` (use `area`'s bottom row, `saturating_sub` to avoid underflow), after the image so it overlays the border/cell edge. Keep it unobtrusive.

- [ ] **Step 2: Build.** `cargo build` (default tui feature) → compiles.

- [ ] **Step 3: Commit.**
```bash
git add src/tui/widget/image.rs
git commit -m "feat(tui): show similarity score on result thumbnails [W44]"
```

---

## Task 11: full verification

- [ ] **Step 1:** `cargo test && cargo build --release` → all green.
- [ ] **Step 2:** `cd site && yarn lint && yarn test --run && yarn build` (likely untouched, but confirm). If `site/` was not modified, a build sanity-check suffices.
- [ ] **Step 3:** Confirm acceptance criteria from the spec (static; no live server/index run). Note anything unconfirmable.

---

## Self-review (author checklist, completed)
- **Spec coverage:** W50→T1; W52→T2(schema)+T3(infra)+T4(cli); W36→T2(schema)+T5(methods)+T6(graphql); W45→T6; W46→T7; W53→T8; W38→T9; W44→T10; verification→T11. All 8 mapped.
- **Placeholders:** concrete code/SQL given; where exact replication is required (the vec0 embedding bind in T3, favorites pattern in T5, survivor-tuple shape in T8) the step names the source to copy. The migration-version stamp + the Task-6→Task-7 embedder-access ordering are explicitly noted.
- **Type consistency:** `RelativePath`/`AbsolutePath` (T1) used in T5/T8; `ModelInfo`/`active_model`/`vectors_table`/`register_model`/`set_active_model`/`list_models` (T3) used in T4; `migration_002_*` + `LATEST_MIGRATION_VERSION=2` (T2); `ImageResult` + `embedder_ready` (T6/T7); tag/collection method names consistent T5↔T6.
