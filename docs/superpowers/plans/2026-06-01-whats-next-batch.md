# whats-next batch 2 (2026-06-01) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement W43 (schema-migration runner), W4+W41 (paginated metadata-first search), and W42 (thumbnail generation during indexing) on branch `whats-next-batch/2026-06-01`.

**Architecture:** Convert `initialize_schema` into a `PRAGMA user_version`-gated migration runner (migration 1 = current full schema, idempotent). Change the REST search endpoint to accept `?limit&offset` and return metadata-first JSON (no base64); the React SPA lazy-loads thumbnails via the existing `/thumb` route and gains a "Load more" button. Wire the existing parallel thumbnail generator into the index flow (default-on, `--no-thumbnails` opt-out).

**Tech Stack:** Rust 2024 (rusqlite, r2d2, sqlite-vec, axum, serde, rayon, clap), React 19 + Vite + TS + Tailwind v4 + vitest.

**Commit grouping:** commit per task; surface-prefixed messages. One PR. Build order within a task touching `site/`: `yarn build` so rust-embed picks up assets; `yarn lint` (0 warnings) + `yarn test`.

**Reference:** spec `docs/superpowers/specs/2026-06-01-whats-next-batch-design.md`. Read it first.

**Execution order:** Task 1 (W43) → Tasks 2-4 (W4+W41) → Task 5 (W42). Sequential (database.rs touched by T1 + T2).

---

## Task 1: `user_version` migration runner (W43)

**Files:**
- Modify: `src/database.rs` (`initialize_schema` ~73-187 → migration runner; `Database::new` call site ~61)
- Test: inline `#[cfg(test)] mod tests` in `src/database.rs`

- [ ] **Step 1: Read the current code**

Read `src/database.rs:27-187` — `Database::new` (extension auto-load → pool build → `initialize_schema`) and the full `initialize_schema` body (13 DDL statements). Note how existing tests construct a `Database` (temp/in-memory) — you'll reuse that fixture.

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)] mod tests` block (adapt the DB constructor to the existing fixture pattern — e.g. `temp_db()`):

```rust
#[test]
fn migrations_set_user_version_and_create_tables() {
    let db = /* existing temp-db fixture */;
    let conn = db.pool.get().unwrap();
    let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, LATEST_MIGRATION_VERSION);
    // core tables exist
    for t in ["images", "image_vectors", "thumbnails", "image_metadata", "favorites"] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "table {t} should exist");
    }
}

#[test]
fn migrations_are_idempotent() {
    let db = /* existing temp-db fixture */;
    let conn = db.pool.get().unwrap();
    // run again — should be a no-op, no error, version unchanged
    run_migrations(&conn).unwrap();
    let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, LATEST_MIGRATION_VERSION);
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test database::tests::migrations`
Expected: FAIL — `run_migrations` / `LATEST_MIGRATION_VERSION` not defined.

- [ ] **Step 4: Implement the runner**

Move the entire current `initialize_schema` DDL into a migration-1 function and add the runner:

```rust
const LATEST_MIGRATION_VERSION: i32 = 1;

/// Ordered schema migrations. Each runs once, gated by PRAGMA user_version.
/// Migration 1 is the full baseline schema (idempotent IF NOT EXISTS DDL), so an
/// existing DB at user_version 0 adopts it as a no-op and is stamped to 1.
fn run_migrations(conn: &Connection) -> Result<()> {
    let current: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current < 1 {
        migration_001_baseline(conn).context("migration 1 (baseline schema)")?;
        conn.execute_batch("PRAGMA user_version = 1;")?;
    }
    // future: if current < 2 { migration_002(conn)?; PRAGMA user_version = 2; } ...
    Ok(())
}

fn migration_001_baseline(conn: &Connection) -> Result<()> {
    // EXACT current initialize_schema DDL, unchanged (all CREATE ... IF NOT EXISTS).
    // images, image_vectors (vec0 float[512]), idx_images_path, idx_images_hash,
    // thumbnails, idx_thumbnails_hash_size, image_metadata, idx_metadata_image_id,
    // idx_metadata_gps, idx_metadata_geo_time, idx_metadata_camera_time,
    // idx_metadata_datetime (partial), favorites.
    // Paste the existing statements here verbatim.
    Ok(())
}
```

Replace the `initialize_schema(&conn)?` call in `Database::new` with `run_migrations(&conn)?`. If any test or other call site references `initialize_schema` by name, keep a thin wrapper `fn initialize_schema(conn: &Connection) -> Result<()> { run_migrations(conn) }` or update the references (grep `initialize_schema`). Ensure `Connection` is imported (rusqlite).

Note: keep the vec0 `CREATE VIRTUAL TABLE` inside migration 1 — the sqlite-vec extension is auto-registered before `Database::new` builds the pool, so it's available.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test database::tests`
Expected: PASS (new migration tests + all existing DB tests, since the schema is byte-identical).

- [ ] **Step 6: Commit**

```bash
git add src/database.rs
git commit -m "feat(db): user_version-gated migration runner (migration 1 = baseline schema) [W43]"
```

---

## Task 2: metadata-first search DB method (W4+W41, backend part 1)

**Files:**
- Modify: `src/database.rs` (add `search_similar_images_meta`, near the other search methods ~405-473)
- Modify: `src/search.rs` (add `search_meta` to `SearchEngine`, mirroring `search_with_thumbnails` ~25-40)
- Test: inline test in `src/database.rs` only if a fixture with indexed images + metadata exists; otherwise rely on build + the API test in Task 3.

- [ ] **Step 1: Read the existing query**

Read `src/database.rs:405-473` — `search_similar_images_with_raw_blob` (the vec0 MATCH query joining images + LEFT JOIN thumbnails, taking `offset`, `distance_threshold`, `max_k`) and `search_similar_images_with_blob`.

- [ ] **Step 2: Add the metadata-first method**

Add a method that returns (relative path, distance, file_size) with NO thumbnail join, but a LEFT JOIN to `image_metadata` for `file_size`:

```rust
/// Metadata-first search: (relative_path, distance, file_size) with no thumbnail blob.
/// Mirrors search_similar_images_with_raw_blob's vec0 MATCH query (same offset/threshold/k
/// handling) but joins image_metadata for file_size instead of thumbnails.
pub fn search_similar_images_meta(
    &self,
    query_embedding: &[f32],
    limit: usize,
    offset: usize,
    distance_threshold: f32,
    max_k: usize,
) -> Result<Vec<(String, f32, Option<i64>)>> {
    let conn = self.pool.get().context("get connection")?;
    let k = limit.clamp(1, max_k);
    let sql = format!(
        "SELECT i.path, v.distance, m.file_size \
         FROM image_vectors v \
         JOIN images i ON i.id = v.rowid \
         LEFT JOIN image_metadata m ON m.image_id = i.id \
         WHERE v.embedding MATCH ?1 AND k = {k} AND v.distance <= {distance_threshold:.6} \
         ORDER BY v.distance LIMIT {k} OFFSET {offset}"
    );
    // Bind the embedding bytes exactly as the existing methods do (zerocopy as_bytes()).
    // Read search_similar_images_with_raw_blob for the precise embedding-bind + row-map shape
    // and replicate it (path: String, distance: f32, file_size: Option<i64>).
    // ... prepare, query_map, collect ...
}
```

Implementer: copy the embedding-binding and statement-execution idiom verbatim from `search_similar_images_with_raw_blob` (don't invent it); only the SELECT columns, joins, and row mapping differ. Confirm the vec0 rowid↔images.id linkage matches the existing query (it joins on `i.id = v.rowid`).

- [ ] **Step 3: Add the SearchEngine wrapper**

In `src/search.rs`, mirror `search_with_thumbnails`:

```rust
pub fn search_meta(
    &self,
    query_embedding: Vec<f32>,
    limit: usize,
    offset: usize,
    distance_threshold: f32,
    max_k: usize,
) -> Result<Vec<(String, f32, Option<i64>)>> {
    self.db
        .search_similar_images_meta(&query_embedding, limit, offset, distance_threshold, max_k)
}
```

(Match the exact `self.db` access pattern used by the existing `search_with_thumbnails`.)

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add src/database.rs src/search.rs
git commit -m "feat(search): metadata-first search method (path, distance, file_size) [W4,W41]"
```

---

## Task 3: paginated metadata-first REST endpoint (W4+W41, backend part 2)

**Files:**
- Modify: `src/api/search.rs` (the `search` handler ~80-99; add params + response structs)
- Test: inline `#[cfg(test)]` for the pure `has_more` rule and param defaults if extractable; otherwise build-verify.

- [ ] **Step 1: Add params + response structs**

In `src/api/search.rs`:

```rust
#[derive(serde::Deserialize)]
struct SearchParams {
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResultItem {
    path: String,
    distance: f32,
    file_size: Option<i64>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    results: Vec<SearchResultItem>,
    has_more: bool,
}
```

- [ ] **Step 2: Rewrite the handler**

```rust
async fn search(
    Extension(context): Extension<GraphQLContext>,
    Path(search): Path<String>,
    Query(params): Query<SearchParams>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(80);
    let offset = params.offset.unwrap_or(0);
    let embedding = context.embedder.get_text_embedding(&search)?; // match existing call
    let engine = SearchEngine::new(&context.db);
    let cfg = crate::config::SearchConfig::default(); // preserve current API behavior (W33 deferral)
    let rows = engine.search_meta(embedding, limit, offset, cfg.distance_threshold, cfg.max_k)?;
    let has_more = rows.len() == limit;
    let results = rows
        .into_iter()
        .map(|(path, distance, file_size)| SearchResultItem { path, distance, file_size })
        .collect();
    Ok(Json(SearchResponse { results, has_more }))
}
```

Match the existing imports/exact embedder + SearchEngine call signatures (read the current handler first; the embedding call may be `get_text_embedding(search)` returning `Vec<f32>` or similar — replicate it). Add `axum::extract::Query` to imports. The route registration in `src/routes.rs` stays `/{search}` (Query params need no route change).

- [ ] **Step 3: Build + sanity test**

Run: `cargo build && cargo test`
Expected: compiles; existing tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/api/search.rs
git commit -m "feat(search): paginated metadata-first /api/v1/search response (limit/offset, hasMore) [W4,W41]"
```

---

## Task 4: SPA lazy thumbnails + Load more (W4+W41, frontend)

**Files:**
- Modify: `site/src/page/Images.tsx`
- Modify: `site/src/components/LightboxViewer.tsx`
- Test: `site/src/page/searchUrl.test.ts` (new, vitest) for the URL builder

- [ ] **Step 1: Read current SPA code**

Read `site/src/page/Images.tsx` (the `ImageFromServer` type, `getImages`, state, grid render, Lightbox slides, the prior batch's loading/empty/error states + `selectViewState`) and `site/src/components/LightboxViewer.tsx` (props, `imageSrc` base64-vs-thumb logic, hover filesize span).

- [ ] **Step 2: Add + test a pure URL builder**

Create `site/src/page/searchUrl.ts`:

```typescript
export const SEARCH_PAGE_SIZE = 40;

export function buildSearchUrl(query: string, offset: number): string {
  return `/api/v1/search/${encodeURIComponent(query)}?limit=${SEARCH_PAGE_SIZE}&offset=${offset}`;
}
```

Create `site/src/page/searchUrl.test.ts`:

```typescript
import { describe, it, expect } from 'vitest';
import { buildSearchUrl, SEARCH_PAGE_SIZE } from './searchUrl';

describe('buildSearchUrl', () => {
  it('encodes query and includes limit+offset', () => {
    expect(buildSearchUrl('a b/c', 0)).toBe(
      `/api/v1/search/a%20b%2Fc?limit=${SEARCH_PAGE_SIZE}&offset=0`,
    );
  });
  it('uses the given offset', () => {
    expect(buildSearchUrl('cat', 40)).toBe(
      `/api/v1/search/cat?limit=${SEARCH_PAGE_SIZE}&offset=40`,
    );
  });
});
```

Run: `cd site && yarn test --run searchUrl` → PASS.

- [ ] **Step 3: Update types + fetch + state**

In `Images.tsx`, replace the tuple type:

```typescript
export interface SearchResultItem {
  path: string;
  distance: number;
  fileSize: number | null;
}
interface SearchResponse {
  results: SearchResultItem[];
  hasMore: boolean;
}
```

Change `images` state to `SearchResultItem[]`, add `hasMore` state. Rewrite `getImages`:

```typescript
const getImages = async (q: string, offset = 0) => {
  setLoading(true);
  setError(null);
  setHasSearched(true);
  try {
    const response = await fetch(buildSearchUrl(q, offset));
    if (!response.ok) throw new Error(`Search failed (${response.status})`);
    const data: SearchResponse = await response.json();
    setImages((prev) => (offset === 0 ? data.results : [...prev, ...data.results]));
    setHasMore(data.hasMore);
  } catch (e) {
    setError(e instanceof Error ? e.message : 'Search failed');
    if (offset === 0) setImages([]);
  } finally {
    setLoading(false);
  }
};
```

Keep the empty-query/idle handling (clear results, hasSearched=false, hasMore=false). `selectViewState` resultCount = `images.length`.

- [ ] **Step 4: Update grid render + Load more**

Grid tile (lazy thumbnail, no base64), keep masonry wrapper:

```tsx
<div className="columns-2 gap-4 p-4 sm:columns-3 lg:columns-4">
  {images.map((item) => (
    <div key={item.path} className="mb-4 break-inside-avoid">
      <LightboxViewer item={item} handleClick={handleClick} />
    </div>
  ))}
</div>
{hasMore && !loading && (
  <div className="flex justify-center p-4">
    <button
      type="button"
      className="rounded bg-gray-700 px-4 py-2 text-white hover:bg-gray-600"
      onClick={() => getImages(query, images.length)}
    >
      Load more
    </button>
  </div>
)}
```

Lightbox slides: `src={`/api/v1/search/file/${item.path}`}`, `thumbnail={`/api/v1/search/thumb:300/${item.path}`}` (map over `images`).

- [ ] **Step 5: Update LightboxViewer**

`site/src/components/LightboxViewer.tsx`: change props to `{ item: SearchResultItem; handleClick: (e: React.MouseEvent, item: SearchResultItem) => void }`. Always use the thumb route (no base64 branch):

```tsx
<img
  className="w-full h-auto group-..."   // preserve existing classes
  src={`/api/v1/search/thumb:300/${item.path}`}
  loading="lazy"
  alt={item.path}
  onClick={(e) => handleClick(e, item)}
/>
```

Show formatted `item.fileSize` on hover (replace the prior `image[1]` distance span). A small inline formatter is fine:

```tsx
{item.fileSize != null && <span className="...hover classes...">{Math.round(item.fileSize / 1024)} KB</span>}
```

Import `SearchResultItem` from the page (or move the interface to a shared `searchUrl.ts`/types file and import in both — prefer a shared location to avoid a circular import; e.g. export `SearchResultItem` from `searchUrl.ts`).

- [ ] **Step 6: Lint + test + build**

Run: `cd site && yarn lint && yarn test --run && yarn build`
Expected: 0 eslint warnings, tests pass, build succeeds.

- [ ] **Step 7: Commit**

```bash
git add site/src/
git commit -m "feat(web): lazy-load thumbnails via /thumb + Load more pagination [W4,W41]"
```

---

## Task 5: thumbnails during index (W42)

**Files:**
- Modify: `src/main.rs` (Index subcommand args ~36-52; `index_directory` ~264-571; confirm `thumbnails` command uses the parallel generator ~208-211)

- [ ] **Step 1: Read the generator + index flow**

Read `src/thumbnail.rs:35-161` (`generate_missing_thumbnails_batch(db, size, count)` — already rayon-parallel) and `src/main.rs` `index_directory` phases + the `Thumbnails` command handler. Note the `quiet` flag and how the standalone command counts/sizes (default 300).

- [ ] **Step 2: Add the `--no-thumbnails` flag**

In the `Index` subcommand struct add:

```rust
/// Skip generating thumbnails during indexing (generate later with `thumbnails`).
#[arg(long)]
no_thumbnails: bool,
```

Thread `no_thumbnails` into `index_directory`'s signature and the call site.

- [ ] **Step 3: Generate thumbnails after indexing**

After Phase 2 + metadata backfill in `index_directory`, before `checkpoint_wal`, add:

```rust
if !no_thumbnails {
    // Generate any missing 300px thumbnails for the freshly indexed images.
    // Reuse the existing parallel batch generator; count large enough to cover all missing.
    let made = thumbnail::generate_missing_thumbnails_batch(db, 300, usize::MAX)
        .unwrap_or_else(|e| { warn!("thumbnail generation failed (non-fatal): {e:#}"); 0 });
    if !quiet { info!("Generated {made} thumbnails"); }
}
```

Confirm the exact function name/path (`thumbnail::generate_missing_thumbnails_batch` vs a re-exported `generate_thumbnails_batch`) and that passing a large `count` means "all missing" (read the function — if `count` caps the batch, pass a count covering all images without thumbnails, e.g. the total image count or `usize::MAX` if it's used as a LIMIT that SQLite treats as unbounded; adjust to the real semantics). Keep WAL checkpoint after this.

- [ ] **Step 4: Confirm standalone command is parallel**

Verify the `Thumbnails` command handler (`src/main.rs:208-211`) calls the parallel `generate_missing_thumbnails_batch` (or an alias of it). If it calls a sequential variant, switch it to the parallel one. No behavior change otherwise.

- [ ] **Step 5: Build + test**

Run: `cargo build && cargo test`
Expected: compiles; tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat(index): generate thumbnails during index by default (--no-thumbnails to skip) [W42]"
```

---

## Task 6: full verification

- [ ] **Step 1: SPA first**

Run: `cd site && yarn install && yarn lint && yarn test --run && yarn build && cd ..`
Expected: all green; `site/build/` regenerated.

- [ ] **Step 2: Rust**

Run: `cargo test && cargo build --release`
Expected: all tests pass; release builds.

- [ ] **Step 3: Confirm acceptance criteria** from the spec (static confirmation; no live server/indexing run). Note anything unconfirmable.

---

## Self-review (author checklist, completed)

- **Spec coverage:** W43→T1; W4→T2/T3/T4; W41→T2/T3/T4; W42→T5; verification→T6. All mapped.
- **Placeholders:** code shown per step; where exact replication is required (the vec0 embedding-bind in T2, the baseline DDL in T1, the existing embedder call in T3, thumbnail count semantics in T5) the step names the source to copy verbatim rather than guessing — deliberate, to avoid inventing SQL/bindings.
- **Type consistency:** `run_migrations`/`LATEST_MIGRATION_VERSION` (T1); `search_similar_images_meta` → `(String,f32,Option<i64>)` consumed by `search_meta` (T2) → `SearchResultItem{path,distance,fileSize}`/`SearchResponse{results,hasMore}` (T3) → SPA `SearchResultItem`/`SearchResponse` + `buildSearchUrl`/`SEARCH_PAGE_SIZE` (T4); `no_thumbnails` + `generate_missing_thumbnails_batch` (T5). Names consistent across tasks.
