# whats-next batch 2 (2026-06-01) — design spec

Bundled implementation of 4 `WHATS-NEXT.md` items selected via `/whats-next --execute`:
W4, W41, W42, W43. One branch (`whats-next-batch/2026-06-01`, cut from `main`). Forward-looking
opportunity work — no bug fixes.

## Decisions locked with the user (brainstorming)

- **W4+W41 — search response:** change `GET /api/v1/search/{query}` to accept `?limit&offset` and return a **metadata-first** JSON object (no inline base64 thumbnails); the SPA lazy-loads each thumbnail from the existing `/thumb:{size}/{*path}` route. SPA gets a **"Load more"** button (append next offset page). Breaking response-shape change, SPA updated in lockstep.
- **W42 — thumbnails on index:** generate thumbnails **by default during `index`** (reusing the already-parallel `generate_missing_thumbnails_batch`), with a `--no-thumbnails` opt-out. The standalone `thumbnails` command is already rayon-parallel (no change needed there beyond confirming).
- **W43 — migrations:** hand-rolled runner gated by SQLite `PRAGMA user_version`. **Migration 1 = the current full schema** (idempotent `IF NOT EXISTS` DDL), so existing DBs run it as a no-op then get stamped to v1; fresh DBs create everything. Future schema changes = migrations 2+. No diesel.

## Key current-state facts (from code anchors, 2026-06-01)

### Search (W4/W41)
- REST handler `src/api/search.rs:80-99`: `async fn search(Extension(context), Path(search)) -> Result<impl IntoResponse, AppError>`. Calls `context.embedder.get_text_embedding(search)` then `SearchEngine::new(&context.db).search_with_thumbnails(embedding, 80, distance_threshold, max_k)`, returns `Json(...)`.
- Current response: `Vec<(String, f32, Option<String>)>` = (relative path, distance, base64 thumbnail|null).
- `src/search.rs:25-40` `search_with_thumbnails` → `db.search_similar_images_with_blob(embedding, limit, threshold, max_k)`.
- `src/database.rs:405-449` `search_similar_images_with_raw_blob(&self, query_embedding, limit, offset, distance_threshold, max_k) -> Result<Vec<(String,f32,Option<Vec<u8>>)>>` — **already takes `offset`**; joins images + LEFT JOIN thumbnails (hardcoded `t.size = 300`). `:450-473` `search_similar_images_with_blob` wraps it with offset=0 and base64-encodes.
- Routes `src/routes.rs:51-58`: `/{search}` → search; `/file/{*filename}` → file; `/thumb:{size}/{*filename}` → thumb. Mounted at `/api/v1/search` (`:78`).
- SPA `site/src/page/Images.tsx`: `export type ImageFromServer = [string, string, string | null]` (line 15) — **mislabeled** ("filename, filesize, base64") but actually holds (path, distance, base64). `getImages` fetches `/api/v1/search/{q}`, sets `images` to the JSON array. Grid maps each tuple to `<LightboxViewer image=… />`. Lightbox slides use `src=/api/v1/search/file/{img[0]}`, `thumbnail=/api/v1/search/thumb:300/{img[0]}`.
- `site/src/components/LightboxViewer.tsx`: `image: ImageFromServer`; `imageSrc = image[2] ? data:base64 : /api/v1/search/thumb:300/{image[0]}`; renders `<img class="w-full h-auto" src=imageSrc>`; hover shows `image[1]` (currently distance, mislabeled as filesize).

### Thumbnails on index (W42)
- `src/thumbnail.rs:35-161` `generate_missing_thumbnails_batch(db: &mut Database, size: u32, count: usize) -> Result<usize>` — **already parallel**: `rayon par_iter` over `get_images_without_thumbnails(size, limit)` + mpsc channel → single writer thread with transactions. Default size 300. Decodes from filesystem path, resizes square Lanczos3, JPEG-encodes.
- `src/main.rs:208-211` Thumbnails subcommand calls `generate_thumbnails_batch(&mut db, size, count)`.
- `src/main.rs:264-571` `index_directory`: Phase 1 collect pending (`:399-429`), Phase 2 batch embed+insert + per-image metadata (`:434-537`), metadata backfill (`:552-558`). Index subcommand args (`:36-52`): dir, recursive, quiet, root, batch_size — **no `--no-thumbnails`**.
- thumbnails table keyed `(image_hash, size)`.

### Schema/migrations (W43)
- `src/database.rs:73-187` `initialize_schema` creates, in order: images, image_vectors (vec0 `float[512]`), idx_images_path, idx_images_hash, thumbnails, idx_thumbnails_hash_size, image_metadata, idx_metadata_image_id, idx_metadata_gps, idx_metadata_geo_time, idx_metadata_camera_time, idx_metadata_datetime (partial), favorites. All `IF NOT EXISTS`.
- `Database::new` (`:27-63`): (1) `sqlite3_auto_extension` registers sqlite-vec, (2) build pool with `with_init("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")`, (3) get conn, (4) `initialize_schema`.
- No `PRAGMA user_version` used anywhere. diesel + diesel_migrations in Cargo.toml (`:25-29`) but unused for the live schema.

---

## Per-item design

### W43 — `user_version` migration runner (do first; foundational)
Files: `src/database.rs`.

- Define a migration list: `const MIGRATIONS: &[(i32, &str)]` or `fn migrations() -> Vec<(i32, fn(&Connection) -> rusqlite::Result<()>)>`. **Migration 1** executes the current full DDL (move the existing `initialize_schema` body into migration 1, unchanged — all `IF NOT EXISTS`).
- New `run_migrations(conn: &Connection) -> Result<()>`:
  1. `let current: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;`
  2. For each `(version, apply)` in MIGRATIONS where `version > current`: run `apply` (each migration in its own transaction), then `conn.pragma_update(None, "user_version", version)?` (or `execute_batch(&format!("PRAGMA user_version = {version};"))`).
  3. Migrations run in ascending version order.
- `initialize_schema` is replaced by `run_migrations` in `Database::new` (called after extension load + pool init, same position). Keep the name or rename — prefer renaming the call site to `run_migrations` for clarity; keep a thin `initialize_schema` wrapper only if other call sites/tests use it (check tests).
- **Idempotent legacy adoption:** an existing DB has `user_version = 0` and all tables already present; migration 1's `IF NOT EXISTS` DDL is a no-op, then `user_version` becomes 1. A fresh DB (`user_version 0`, no tables) runs migration 1 to create everything. Identical end state.
- vec0 virtual table is created inside migration 1 — the extension is already auto-registered before `Database::new` builds the pool, so it's available. Confirm ordering preserved.
- Tests: `run_migrations` on a fresh in-memory/temp DB sets `user_version` to the max migration version and creates the tables; running it twice is a no-op (second call applies nothing, version unchanged); a simulated "legacy" DB (tables pre-created, user_version 0) ends at v1 without error.

### W4 + W41 — paginated metadata-first search
Files: `src/database.rs` (new method), `src/search.rs` (SearchEngine method), `src/api/search.rs` (handler + params + response struct), `src/routes.rs` (unchanged route path), SPA `site/src/page/Images.tsx` + `site/src/components/LightboxViewer.tsx`.

**Backend:**
- New DB method `search_similar_images_meta(&self, query_embedding, limit, offset, distance_threshold, max_k) -> Result<Vec<(String, f32, Option<i64>)>>` returning (relative path, distance, file_size) — same vec0 MATCH query as `search_similar_images_with_raw_blob` but **no thumbnails join**; LEFT JOIN `image_metadata` for `file_size`. (Reuse the existing query skeleton; drop the thumbnails join, add the metadata join.)
- `SearchEngine` gets a matching `search_meta(embedding, limit, offset, threshold, max_k)` wrapper (mirror `search_with_thumbnails`).
- REST handler: add an Axum `Query<SearchParams>` extractor: `struct SearchParams { limit: Option<usize>, offset: Option<usize> }` (serde defaults). Resolve `limit = params.limit.unwrap_or(80)`, `offset = params.offset.unwrap_or(0)`. Define response structs:
  ```rust
  #[derive(serde::Serialize)] struct SearchResultItem { path: String, distance: f32, file_size: Option<i64> }
  #[derive(serde::Serialize)] struct SearchResponse { results: Vec<SearchResultItem>, has_more: bool }
  ```
  `has_more = results.len() == limit`. Return `Json(SearchResponse{...})`. (serde renames: emit `fileSize`/`hasMore` camelCase via `#[serde(rename_all = "camelCase")]`.)
- Keep `distance_threshold`/`max_k` resolution as today (SearchConfig::default() in the API path, per the W18/W33 deferral — unchanged).

**SPA:**
- Replace `ImageFromServer` tuple type with `interface SearchResultItem { path: string; distance: number; fileSize: number | null }` and `interface SearchResponse { results: SearchResultItem[]; hasMore: boolean }`.
- `getImages(q, offset=0)`: fetch `/api/v1/search/${encodeURIComponent(q)}?limit=${PAGE}&offset=${offset}` (PAGE e.g. 40); on first page set results, on "load more" append; track `hasMore` + current `offset`. Preserve the loading/error/empty/idle states from the prior batch (selectViewState) — resultCount uses results length.
- Grid tile: render `<img src={`/api/v1/search/thumb:300/${item.path}`} loading="lazy" alt={item.path} class="w-full h-auto" />` (no base64). Update `LightboxViewer` to take `SearchResultItem`, drop the base64 branch (always use the /thumb route), and show `fileSize` (formatted) on hover instead of the mislabeled distance. Lightbox slides: `src=/api/v1/search/file/${item.path}`, `thumbnail=/api/v1/search/thumb:300/${item.path}`.
- "Load more" button shown when `hasMore && !loading`; calls `getImages(query, results.length)` appending.
- Keep masonry layout (columns) from the prior batch.
- Tests (vitest): a small pure helper for the fetch URL builder and/or the append-vs-replace logic if extracted; otherwise rely on lint+build. Keep/extend selectViewState usage.

### W42 — thumbnails during index (opt-out)
Files: `src/main.rs` (Index subcommand + `index_directory`).

- Add `#[arg(long)] no_thumbnails: bool` to the Index subcommand; thread into `index_directory`.
- After Phase 2/metadata in `index_directory`, unless `no_thumbnails`, call the existing `generate_missing_thumbnails_batch(&mut db, 300, <count covering newly indexed>)` (use a count large enough to cover all missing — e.g. the number of pending/indexed this run, or call the existing "generate all missing" path the standalone command uses). Respect `quiet` for progress. Log a summary line (thumbnails generated).
- The standalone `thumbnails` command already uses the parallel batch generator — no change required (confirm it does; if it calls a sequential variant, switch it to `generate_missing_thumbnails_batch`).
- No tests needed (I/O + already-tested generator); covered by build. Optionally a count/arg-parsing assertion.

---

## Cross-cutting notes
- **Order:** build W43 first (isolates the schema→migration refactor), then W4+W41 (new search method + REST + SPA), then W42 (index wiring). All sequential; database.rs touched by W43 + W4 — no conflict when sequential.
- **Relative-path invariant:** search returns relative paths (as today); the SPA passes them to `/thumb`/`/file` routes which resolve against basepath (unchanged).
- **SPA embedded:** `yarn build` before `cargo build` so rust-embed picks up fresh assets.
- **No scope creep:** W41 touches only the REST search path + SPA Images page; the map's `imagesByBounds` base64 thumbnails are out of scope. GraphQL `Query::search` remains a stub (untouched).
- **Backward compat:** existing user DBs adopt migration v1 transparently (idempotent). The search response shape change is breaking for any external REST consumer, but the only known consumer is the in-repo SPA, updated in lockstep.

## Acceptance criteria
- `cargo test` green; `cargo build --release` OK; `cd site && yarn lint && yarn test && yarn build` green (0 eslint warnings).
- `GET /api/v1/search/{q}?limit=N&offset=M` returns `{results:[{path,distance,fileSize}], hasMore}` with no base64; omitting params preserves limit 80 / offset 0.
- SPA: results render via lazy `/thumb` images; "Load more" fetches and appends the next page; loading/empty/error/idle states intact; masonry preserved.
- `imgfind index <dir>` generates thumbnails by default (parallel); `--no-thumbnails` skips them; standalone `thumbnails` still works.
- A migration runner gated on `user_version`: fresh DB → all tables + `user_version` = latest; existing DB → no-op adoption to v1; double-run is a no-op. Covered by unit tests.

## Out of scope / deferred
- Threading per-request threshold/limit overrides into search config beyond CLI (W33). Map `imagesByBounds` lazy thumbnails. GraphQL search implementation (W45). Multi-size thumbnail prefetch. Infinite-scroll (Load-more button instead).
