# whats-next batch 3 (2026-06-01) — design spec

Bundled implementation of 8 selected `WHATS-NEXT.md` items: W36, W38, W44, W45, W46, W50, W52, W53.
One branch (`whats-next-batch/2026-06-01-b`, cut from main). (W37 was deferred during brainstorming.)
Largest batch to date — W52 (multi-model storage) and W50 (path newtypes) are broad refactors.

## Decisions locked with the user (brainstorming)
- **W52:** FULL variable-dimension storage — per-model vec0 tables + a `models(name,dim,is_active)` registry; existing `image_vectors` kept as the baseline model's table; `--model` selects active; index/search use the active model's table.
- **W50:** FULL `RelativePath`/`AbsolutePath` newtype refactor across all path boundaries.
- **W36:** schema + Database methods + GraphQL for tags & collections (favorites already exists).
- **W45:** new `ImageResult { path, distance, fileSize }` GraphQL type; `search(query, limit=80, offset=0): [ImageResult!]!` via `SearchEngine::search_meta`.
- **W46:** lazy/async CLIP init; **503 + Retry-After** until ready; `/healthz` reporting `loading|ready`.
- **W53:** rayon parallel metadata extraction.
- **W38:** `imgfind completions {shell}` via `clap_complete`.
- **W44:** similarity score label on TUI thumbnails.

## CRITICAL scoping caveat (W52)
`../clipper` `ClipEmbedder` hardcodes the model (`openai/clip-vit-base-patch32`, dim 512) and exposes **no** model-name/dim accessor or selection API. Therefore this batch builds the **imgfind-side multi-model storage + selection infrastructure** and seeds the single model clipper supports. Producing embeddings from a *different* model/dim requires future `../clipper` changes — explicitly out of scope. The infra is designed so a new model (name, dim) added to the registry with a matching clipper capability later "just works" without further imgfind schema changes.

## Key current-state facts (anchors, 2026-06-01)
- **Migration system:** `LATEST_MIGRATION_VERSION` (=1), `run_migrations(conn)` (reads `PRAGMA user_version`, applies `if current < N`, stamps), `migration_001_baseline` (src/database.rs:78-210). Add migration 2 by bumping the const, adding `migration_002_*`, and a `if current < 2` block.
- **`image_vectors` sites (all in src/database.rs):** CREATE (mig 1, ~111), `insert_image` DELETE+INSERT (244,250), `insert_images_batch` (350-359), `clean_missing_files` DELETE (581), `search_similar_images` (396-403), `search_similar_images_with_raw_blob` (440-448), `search_similar_images_meta` (493-501). Embedding bound as `embedding.as_bytes()` / `.as_slice().as_bytes()`; rowid = images.id.
- **Path conversions:** `abs_to_relative_path(&Path,&Path)->Result<PathBuf>`, `relative_to_abs_path(&Path,&Path)->PathBuf` (src/lib.rs:88-99). 10 call sites (db methods + index_directory). DB stores RELATIVE paths; search returns relative; FS access uses absolute. `Database.parent_dir` is the base.
- **favorites pattern** (src/database.rs:200-320): table + toggle/is/list methods — mirror for tags/collections.
- **GraphQL** (src/graphql.rs): `Query` (stub `search`@29, `favorites`,`isFavorite`,`imagesByBounds`), `Mutation` (`toggleFavorite`), `ImageLocation`/`ImageBoundsResult` (GraphQLObject derives), `Schema`/`create_schema`. Context: `GraphQLContext{db, basepath, embedder: Arc<ClipEmbedder>}`. Embedder: `get_text_embedding(&self,&str)->Result<Vec<f32>>`. Resolvers may be sync or `async`.
- **serve()** (src/main.rs:227-247, async/tokio): blocking `ClipEmbedder::new` → `GraphQLContext::new` → `app(context)` → axum::serve. **routes** (src/routes.rs:54-85): `app(context)` nests `/graphql`, `/api/v1`, static; Extension(context) layered. No health route.
- **index_directory** (src/main.rs:266-595): Phase1 collect pending; Phase2 chunked decode→batch-embed→insert_images_batch→**per-image metadata loop (513-537)**→; Phase3 thumbnails; WAL checkpoint. `extract_image_metadata(&str)->Result<ImageMetadata>` (pure, sync, I/O-heavy, src/database.rs:920). `insert_or_update_metadata(&mut self,i64,&ImageMetadata)`. rayon 1.10 already a dep (used in thumbnail.rs).
- **CLI**: `#[derive(Parser)] Cli{command: Commands}` name "imgfind"; `Commands` enum (src/main.rs:24-127). clap 4.0 derive. clap_complete NOT yet a dep.
- **TUI**: `ImageEntry{path, score: f32, ...}` (src/tui/app/search.rs) — score already carried. `render_image(...)->Result<Rect>` (src/tui/widget/image.rs:12-39) draws a bordered block + centered image, no text label. Grid via `nine_block`.

---

## Per-item design

### W50 — path newtypes (do FIRST; everything else builds on the cleaner signatures)
- In src/lib.rs define:
  ```rust
  #[derive(Debug,Clone,PartialEq,Eq,Hash)] pub struct RelativePath(pub PathBuf);
  #[derive(Debug,Clone,PartialEq,Eq,Hash)] pub struct AbsolutePath(pub PathBuf);
  ```
  with helpers: `RelativePath::as_str`/`Display`, `AbsolutePath::as_path`, and conversions `AbsolutePath::to_relative(&self, base:&Path)->Result<RelativePath>`, `RelativePath::to_absolute(&self, base:&Path)->AbsolutePath`. Reimplement `abs_to_relative_path`/`relative_to_abs_path` in terms of these (or keep as thin shims).
- Thread through the 10 call sites + the Database methods that deal in paths: `insert_image`, `is_image_indexed`, `get_image_id` (take `&AbsolutePath`); methods returning FS paths (`get_sample_images`, `get_images_without_thumbnails`, `get_images_without_metadata`, `get_images_by_bounds`) return `AbsolutePath`; methods over stored relative paths (`toggle_favorite`,`is_favorite`,`list_favorites`,`get_image_hash`) take/return `RelativePath`. Search methods continue returning relative path strings — wrap as `RelativePath` (decide: keep `String` at the API boundary to avoid churning the REST/GraphQL JSON, converting at the edge). **Boundary rule:** newtypes internal to Rust; serialized JSON/GraphQL stays plain strings.
- Update all callers (main.rs index loop, api, tui) to construct the right newtype. Build must stay green; this is mechanical.
- Tests: conversion round-trip (`abs.to_relative(base).to_absolute(base) == abs` within base), and that `to_relative` errors outside base.

### W52 — multi-model embedding storage (migration 2 + active-model-parameterized queries)
- **Migration 2** (shared with W36): create
  ```sql
  CREATE TABLE IF NOT EXISTS models (
    name TEXT PRIMARY KEY, dim INTEGER NOT NULL, table_name TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 0, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
  ```
  Seed the baseline model: `INSERT OR IGNORE INTO models(name,dim,table_name,is_active) VALUES ('openai/clip-vit-base-patch32', 512, 'image_vectors', 1)`. (Keep existing `image_vectors` as its table — no vec0 rename.)
- **ModelInfo + active-model resolution:** `Database::active_model(&self) -> Result<ModelInfo{name:String,dim:usize,table:String}>` (query `is_active=1`). A `Database::set_active_model(&self, name)` flips `is_active` (and errors if unknown).
- **Parameterize the vec0 table name** in every `image_vectors` site: build the SQL with the active model's `table` interpolated (trusted value from the registry — NOT user free-text; validate it matches `^[A-Za-z0-9_]+$` defensively). `insert_image`/`insert_images_batch`/`clean_missing_files` write to the active table; search methods read from it. Add a small private helper `fn vectors_table(&self) -> Result<String>` used by these methods.
- **Creating a new model:** `Database::register_model(name, dim) -> Result<()>` inserts a models row with `table_name = format!("image_vectors_{}", sanitized(name))` and `CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(embedding float[{dim}])`. (Not wired to a real second model yet — clipper caveat.)
- **CLI:** `--model <name>` flag on `index` and `search` selects the active model for that run (defaults to the current active). A `imgfind models {list|use <name>}` subcommand is a reasonable surface — DECISION captured: include `models list` + `models use <name>`; `register` is internal until clipper supports a second model.
- Tests: migration 2 seeds exactly one active model = baseline/dim512/table image_vectors; `active_model()` returns it; `vectors_table()` returns `image_vectors`; existing search/insert tests still pass (they now go through the active-table lookup, which resolves to image_vectors). A `register_model` + `set_active_model` round-trip creates a new vec0 table and flips active.
- **Backward-compat:** existing DBs (user_version 1) get migration 2 which adds the models table + seeds baseline pointing at their existing `image_vectors` — no data move.

### W36 — tags & collections (migration 2 + methods + GraphQL)
- **Migration 2** also creates:
  ```sql
  CREATE TABLE IF NOT EXISTS tags (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
  CREATE TABLE IF NOT EXISTS image_tags (image_id INTEGER NOT NULL, tag_id INTEGER NOT NULL, PRIMARY KEY(image_id,tag_id),
    FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE, FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE);
  CREATE TABLE IF NOT EXISTS collections (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
  CREATE TABLE IF NOT EXISTS collection_images (collection_id INTEGER NOT NULL, image_id INTEGER NOT NULL, PRIMARY KEY(collection_id,image_id),
    FOREIGN KEY(collection_id) REFERENCES collections(id) ON DELETE CASCADE, FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE);
  ```
- **Database methods** (mirror favorites; take `&RelativePath` where a path identifies an image, resolve to image_id): `create_tag(name)->Result<i64>`, `tag_image(path, tag)`, `untag_image(path, tag)`, `tags_for_image(path)->Vec<String>`, `list_tags()->Vec<String>`, `images_by_tag(name)->Vec<RelativePath>`; collections: `create_collection(name)->i64`, `add_to_collection(name, path)`, `remove_from_collection(name, path)`, `collection_images(name)->Vec<RelativePath>`, `list_collections()->Vec<String>`. Unit-tested against the test DB fixture.
- **GraphQL** on the existing roots: queries `tags`, `tagsForImage(path)`, `collections`, `collectionImages(name)`, `imagesByTag(name)`; mutations `createTag(name)`, `tagImage(path,tag)`, `untagImage(path,tag)`, `createCollection(name)`, `addToCollection(name,path)`, `removeFromCollection(name,path)`. Return scalars/`[String!]!` (paths) to avoid needing a rich Image type here (W45 adds ImageResult separately).

### W45 — GraphQL search (real)
- Add `#[derive(GraphQLObject)] struct ImageResult { path: String, distance: f64, file_size: Option<i32> }` (juniper wants f64/i32; cast from f32/i64).
- Replace the stub: `pub fn search(context, query: String, limit: Option<i32>, offset: Option<i32>) -> FieldResult<Vec<ImageResult>>` — embed via `context.embedder` (W46-aware: if not ready, return a FieldResult error / handled per W46), `SearchEngine::search_meta(emb, limit?80, offset?0, SearchConfig::default().{threshold,max_k})`, map rows → ImageResult. Keep it sync unless the embedder readiness requires async (W46 may make embedder access async).

### W46 — lazy CLIP init + readiness
- Change `GraphQLContext.embedder` to a shared readiness holder, e.g. `Arc<tokio::sync::OnceCell<ClipEmbedder>>` plus an `Arc<AtomicBool> ready` (or model load spawned and the OnceCell awaited). In `serve()`: build context with an empty OnceCell, spawn a background task (`tokio::task::spawn_blocking`) that loads `ClipEmbedder::new(...)` and sets the cell; start axum immediately.
- Embedder access at request time: a helper `context.embedder_ready() -> Option<&ClipEmbedder>` (or `try_get`). REST search (api/search.rs) and GraphQL search return **503 + `Retry-After: 5`** (REST) / a `FieldResult` error tagged not-ready (GraphQL) when not loaded. Add `GET /healthz` → `{ "model": "loading" | "ready" }` (200 either way; or 503 when loading — pick 200 with status body for simplicity), registered in `app()`.
- Index/CLI paths keep eager per-command loading (unchanged) — W46 is serve-only.
- DECISION captured: `/healthz` returns 200 with `{model: loading|ready}`; search endpoints 503 while loading.

### W53 — parallel metadata extraction (rayon)
- In `index_directory` Phase 2, replace the per-image sequential metadata loop (main.rs:513-537) with a rayon `par_iter` over the chunk's survivors that calls the pure `extract_image_metadata(abs)` in parallel, collecting `(image_id_or_path, Result<ImageMetadata>)`, then a sequential pass does the DB writes (`insert_or_update_metadata`) — DB writes stay serial (single pool/txn), parallelism is the I/O-bound extract. Also apply the same pattern to `extract_missing_metadata` (src/metadata.rs) backfill loop. Preserve existing error counting (failures logged, counted, non-fatal).

### W38 — shell completions
- Add `clap_complete = "4.0"` dep. Add `Commands::Completions { shell: clap_complete::Shell }`. Handler: `clap_complete::generate(shell, &mut Cli::command(), "imgfind", &mut std::io::stdout())`. (Requires `use clap::CommandFactory`.)

### W44 — TUI score label
- In `render_image` (src/tui/widget/image.rs), after rendering the image, draw a small score label (e.g. bottom row of the cell) from `image_entry.score` — format compactly (e.g. `{:.3}`). Use a `Span`/`Line` rendered into the cell area (reserve one row, or overlay on the border title). Keep it unobtrusive; lower distance = closer match.

---

## Build order (one branch, sequential — heavy database.rs contention)
1. **W50** newtypes (touches signatures everything else uses).
2. **W52 + W36** migration 2 (shared) + model storage parameterization + tag/collection methods. (Biggest; database.rs core.)
3. **W45** GraphQL search; **W36** GraphQL surface.
4. **W46** lazy embedder + healthz (serve/context/api/graphql embedder access).
5. **W53** parallel metadata.
6. **W38**, **W44** UX.

## Verification
`cargo test` + `cargo build --release`; web `yarn lint`/`test`/`build` (only if W45/W46 touch the SPA — likely not; the SPA uses REST, which is unchanged here, so web likely untouched). Migration 2 tested idempotent on fresh + on a v1 DB. New unit tests: path-newtype round-trip; migration 2 seeds/active model; tag/collection method round-trips; vectors_table resolution.

## Acceptance criteria
- `cargo test` green; release build OK; web green (or untouched).
- Migration 2: fresh DB → models(baseline,512,image_vectors,active) + tags/image_tags/collections/collection_images; v1 DB upgrades cleanly; double-run no-op; `user_version`=2.
- Search/index operate through the active model's vec0 table (defaults to image_vectors); `models list` shows the baseline; `models use`/`--model` switch active (register creates a new vec0 table at the model's dim).
- Path newtypes enforce abs/rel at compile time across DB boundaries; JSON/GraphQL still emit plain path strings.
- GraphQL `search(query,limit,offset)` returns `[ImageResult{path,distance,fileSize}]`; tags/collections queries+mutations work.
- `serve` starts immediately; `/healthz` reports loading→ready; search returns 503+Retry-After until the model loads, then works.
- Indexing extracts metadata in parallel (rayon) with unchanged correctness/counts.
- `imgfind completions bash|zsh|fish` emits a script; TUI shows per-thumbnail scores.

## Out of scope / deferred
- Actually emitting a non-512 / second model's embeddings (needs ../clipper changes). W37 (embedding cache). W54/W55 (tag/album UI). SPA changes for GraphQL search (REST SPA path unchanged).
