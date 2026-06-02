# WHATS-NEXT.md — forward-looking work suggestions

Last triage: 2026-05-31 against `whats-next-skill/2026-05-31` @ 75d77ca.

> **For future sessions reading this file:** when an item listed here ships,
> strip it from this file in the same change that ships it. The list is
> intended to reflect open opportunities only; shipped items shouldn't linger.
> This keeps the file's signal-to-noise high for the next whats-next pass.

## How to use this file
- Check `[x] execute` on items to hand them to `/ship-it --ask` for implementation.
- Check `[x] skip` on items to never re-flag (the skill records them in user memory).
- Items left unchecked persist in WHATS-NEXT.md for the next run.
- Ranking is value-to-effort (bang-for-buck): effort IS folded into the score.
- When ready, run `/whats-next --execute`.

## Critical

### W2. Confirmation/dry-run before the destructive `clean` command (CLI — src/main.rs:172)
- Lens: ux
- Score: 4.00 (value 4 / effort S)
- What: Add a `--dry-run` flag and a `--confirm`/`-y` flag (or interactive `Remove N entries for missing files? (y/n)` prompt) before `clean_missing_files()` deletes rows.
- Why: `clean` is irreversible; an accidental run (typo, muscle memory) deletes DB records with no recovery. A preview/confirm guard is expected for destructive CLI actions.
- Blocked by: —
- [ ] execute   [ ] skip

### W3. Image-to-image similarity search by stored path (Search Engine/API — src/api/search.rs:80)
- Lens: feature-gap
- Score: 4.00 (value 4 / effort S)
- What: Add `/api/v1/search/similar/{*path}` (and a CLI `search --like <path>`) that loads the stored embedding for the given image and returns its nearest neighbors — no text query required.
- Why: "Find more like this" is a core expectation of an image-search tool. The embeddings are already in `image_vectors`, so this is a thin endpoint over the existing vector search.
- Blocked by: —
- Notes: Distinct from W34 (reverse search by *uploaded* file) — this one searches by an already-indexed path.
- [ ] execute   [ ] skip

### W7. Persist the search query in the URL for shareable links and back/forward (React SPA — site/src/page/Images.tsx:28)
- Lens: ux
- Score: 4.00 (value 4 / effort S)
- What: Sync the query to the URL on every search and fully restore query + results on back/forward and direct navigation to `/?query=…`.
- Why: Users expect to bookmark and share a result link; today the query is lost on reload or back/forward, forcing re-entry.
- Blocked by: —
- Notes: `useSearchParams` is present but state may not fully round-trip; verify back-button behavior.
- [ ] execute   [ ] skip

### W8. Pattern-syntax examples in `config` command help (CLI — src/main.rs:112)
- Lens: ux
- Score: 3.00 (value 3 / effort S)
- What: Add help text to the `config` subcommands explaining that ignore patterns are regex with a substring fallback, with examples (`node_modules`, `\.git`, `^tmp.*`).
- Why: Users currently can't tell whether patterns are literal or regex, causing silent match failures and trial-and-error.
- Blocked by: —
- [ ] execute   [ ] skip

### W11. "No data" messaging on the map view (React SPA — site/src/page/MapView.tsx:196)
- Lens: ux
- Score: 3.00 (value 3 / effort S)
- What: Show a centered banner when `images.length === 0 && !loading`, e.g. "No geotagged images in this area — index images with GPS EXIF to see them here."
- Why: A blank map is ambiguous between "empty database" and "untagged region"; explicit messaging cuts confusion and support burden.
- Blocked by: —
- [ ] execute   [ ] skip

### W12. Search history / saved searches on the web frontend (React SPA — site/src/page/Images.tsx)
- Lens: feature-gap
- Score: 3.00 (value 3 / effort S)
- What: Persist recent queries in `localStorage` and show them as a dropdown; allow pinning/favoriting frequent searches.
- Why: Users repeat searches constantly; recall-and-rerun is a cheap power-user win requiring no backend change.
- Blocked by: —
- [ ] execute   [ ] skip

### W14. Show result count and active query in the web results header (React SPA — site/src/page/Images.tsx:85)
- Lens: ux
- Score: 3.00 (value 3 / effort S)
- What: Display "Found N images for "query"" as a subtitle and surface the per-result similarity score on hover/tooltip.
- Why: After typing, the query scrolls out of view and users can't tell how many results came back or how confident the matches are.
- Blocked by: —
- [ ] execute   [ ] skip

## High

### W16. `--output` flag on the `metadata` command (CLI — src/main.rs:141)
- Lens: ux
- Score: 2.00 (value 2 / effort S)
- What: Add `--output <path>` to write EXIF data as JSON or CSV instead of stdout-only.
- Why: Auditing EXIF coverage or extracting by date/camera currently requires manual redirection; a flag makes the intent explicit and scriptable.
- Blocked by: —
- [ ] execute   [ ] skip

### W19. `index --max-depth` / `--watch` modes (CLI — src/main.rs:146)
- Lens: feature-gap
- Score: 2.00 (value 2 / effort S)
- What: Add `--max-depth <n>` to bound recursion and `--watch` to continuously monitor a directory and index new files.
- Why: Indexing deep/large trees is slow and all-or-nothing today; depth control and a watch mode give users incremental, ongoing indexing.
- Blocked by: —
- [ ] execute   [ ] skip

### W20. In-memory LRU thumbnail cache in the web server context (web serving — src/context.rs)
- Lens: scale-perf
- Score: 2.00 (value 2 / effort S)
- What: Add an LRU cache (keyed by `(image_hash, size)`) to `GraphQLContext` so repeated thumbnail requests skip the DB roundtrip; invalidate on new thumbnail generation.
- Why: Search returns 80 results and users re-request the same set repeatedly; caching the hot 100-500 thumbnails removes the bulk of thumbnail DB queries on a busy web frontend.
- Blocked by: —
- [ ] execute   [ ] skip

### W21. "Copy error" button on the map view error display (React SPA — site/src/page/MapView.tsx:189)
- Lens: ux
- Score: 2.00 (value 2 / effort S)
- What: Add a clickable "Copy error" control next to the error message that copies the full text to the clipboard.
- Why: GraphQL/fetch errors are verbose and awkward to screenshot; copy-to-clipboard makes bug reports accurate.
- Blocked by: —
- [ ] execute   [ ] skip

### W22. Negative prompts / concept exclusion in search (Search Engine/GraphQL — src/graphql.rs)
- Lens: feature-gap
- Score: 2.00 (value 2 / effort S)
- What: Support a negative term (e.g. `sunset except:people` or an API `exclude` param) that penalizes results near the negative embedding.
- Why: Users frequently want "X but not Y"; CLIP embeddings make this a cheap distance computation against a second query vector.
- Blocked by: —
- [ ] execute   [ ] skip

### W23. OpenAPI spec + Swagger UI for the REST API (API — src/api/search.rs)
- Lens: feature-gap
- Score: 2.00 (value 2 / effort S)
- What: Generate an OpenAPI 3.0 spec (e.g. via `utoipa`) for the REST endpoints and serve Swagger UI at `/docs`.
- Why: The REST surface is only discoverable by reading code today; a spec eases third-party integration and self-documents the API.
- Blocked by: —
- [ ] execute   [ ] skip

### W26. Export current TUI results to a file (TUI — src/tui/app.rs)
- Lens: feature-gap
- Score: 2.00 (value 2 / effort S)
- What: Add a keybinding (e.g. `w` to write, `y` to yank) that exports current results (paths, or paths+scores) to text/JSON/CSV or the clipboard.
- Why: TUI users can't pipe results the way the CLI's `--short` allows; an in-app export closes that gap.
- Blocked by: —
- [ ] execute   [ ] skip

### W28. Consistent API error shape and HTTP status codes (API — src/api/mod.rs:28)
- Lens: ux
- Score: 2.00 (value 4 / effort M)
- What: Return a structured error JSON (`{error, code, details}`) with the right status (400 bad query, 404 missing file, 500 server error) instead of `AppError`'s blanket 500 + debug string.
- Why: Clients currently can't distinguish bad input from server error from not-found, blocking sensible client-side error handling, messaging, and retries.
- Blocked by: —
- [ ] execute   [ ] skip

### W29. Batch + parallel metadata extraction and insertion (metadata extraction — src/metadata.rs:37)
- Lens: scale-perf
- Score: 2.00 (value 4 / effort M)
- What: Extract metadata in parallel (rayon `par_iter`) and buffer inserts into transactions of 100-200 rows via a new `insert_metadata_batch()` instead of one extract+insert+connection per image.
- Why: At 10K+ images the serial extract/insert backfill is a bottleneck; batching cuts DB round-trips ~100-200× and parallel EXIF reads use all cores.
- Blocked by: —
- [ ] execute   [ ] skip

### W30. `search --format {text|json|csv}` output flag (CLI — src/main.rs:61)
- Lens: ux
- Score: 2.00 (value 4 / effort M)
- What: Add `--format` to `search` so results (path + score + optional metadata) can be emitted as JSON or CSV; `--short` (bare paths) stays as-is.
- Why: Power users need machine-readable output to pipe into `jq` or spreadsheets; parsing the human text output is brittle and `--short` drops scores/metadata.
- Blocked by: —
- Notes: `search_images()` already has score/limit info; this is purely an output formatter.
- [ ] execute   [ ] skip

### W31. Duplicate / near-duplicate detection command (Database/CLI — src/database.rs)
- Lens: feature-gap
- Score: 2.00 (value 4 / effort M)
- What: Add `imgfind find-duplicates --threshold 0.95` that groups images with near-identical embeddings and reports the clusters.
- Why: De-duplication is a top use case for large libraries; the embeddings are already computed, so this is threshold clustering over existing vector search.
- Blocked by: —
- [ ] execute   [ ] skip

### W33. Metadata-based search filters (date range, camera, GPS, dimensions) (Search Engine/GraphQL — src/graphql.rs)
- Lens: feature-gap
- Score: 2.00 (value 4 / effort M)
- What: Add filter args to search/GraphQL: `dateRange` (before/after), `camera` (make/model), `gpsRadius` (lat/lon + radius), `dimensions` (min/max), `fileSize`.
- Why: All of this metadata is already extracted into `image_metadata` but never exposed; users routinely need to narrow semantic results by when/where/which-camera.
- Blocked by: —
- Notes: Benefits from W10 (composite indexes). Backend for W40 (faceted pills).
- [ ] execute   [ ] skip

### W34. Reverse search from an uploaded image or URL (Web API/SPA — src/api/search.rs)
- Lens: feature-gap
- Score: 2.00 (value 4 / effort M)
- What: Add `/api/v1/search/by-image` accepting multipart/binary image data, embed it via clipper, and return similar results; add a file-upload + preview affordance in the SPA.
- Why: Users often have a sample photo and want to find it (or similar) in the library; today only text search exists.
- Blocked by: —
- Notes: Distinct from W3 (similarity by already-indexed path) — this embeds an arbitrary uploaded image.
- [ ] execute   [ ] skip

### W35. TUI "copy path" and "open in external viewer" keybindings (TUI — src/tui/app.rs:264)
- Lens: ux
- Score: 2.00 (value 4 / effort M)
- What: Bind `y` to copy the focused image's path to the clipboard and `o` to open it in the system image viewer.
- Why: Grabbing a path for another tool or viewing full-res currently means leaving the TUI and copying manually; single keys streamline the most common follow-on actions.
- Blocked by: —
- Notes: Needs a clipboard crate (`arboard`) and the `open` crate for the viewer.
- [ ] execute   [ ] skip

### W36. Persistence schema for user-created metadata (tags, collections, favorites) (database — src/database.rs:112)
- Lens: unblock-debt
- Score: 2.00 (value 4 / effort M)
- What: Add `user_tags`, `image_tags`, `collections`, `favorites` tables to store user annotations separately from EXIF, plus filtering by tag/collection.
- Why: `image_metadata` holds only machine-extracted EXIF; any curation feature needs user-data tables. Foundation (with W32) for W55 (tagging) and W54 (albums).
- Blocked by: —
- Notes: Pairs with W32 (mutation root) and W43 (migrations) to evolve the schema safely.
- [ ] execute   [ ] skip

### W37. Persistent embedding cache to skip re-computation on re-index (indexing — src/main.rs:254)
- Lens: scale-perf
- Score: 1.50 (value 3 / effort M)
- What: Keep an on-disk cache of `(oshash → 512-dim embedding)`; on re-index, reuse cached embeddings for unchanged images instead of re-running CLIP.
- Why: Re-indexing a partially-indexed tree currently re-embeds skipped images; a hash-keyed cache saves seconds per image (CPU) across sessions.
- Blocked by: —
- Notes: Invalidate only when the content hash changes.
- [ ] execute   [ ] skip

### W38. Shell completion scripts (bash/zsh/fish) (CLI — src/main.rs:22)
- Lens: ux
- Score: 1.50 (value 3 / effort M)
- What: Add an `imgfind completions {bash|zsh|fish}` command that emits a completion script (via `clap_complete`).
- Why: Power users expect tab completion for subcommands and flags; without it they must memorize names or re-run `--help`.
- Blocked by: —
- [ ] execute   [ ] skip

### W39. Bulk export of search results as a zip (API/CLI — src/api/mod.rs)
- Lens: feature-gap
- Score: 1.50 (value 3 / effort M)
- What: Add `/api/v1/search/export/{query}?format=zip&limit=N` (and a CLI equivalent) that streams the matching images as an archive; wire a bulk-download action into the web UI.
- Why: Search results have no bulk action today; users want to batch-export matches to share, back up, or post-process.
- Blocked by: —
- Notes: Needs a zip crate and a streaming multi-file response.
- [ ] execute   [ ] skip

### W40. Faceted filter pills on the web results page (React SPA — site/src/page/Images.tsx)
- Lens: feature-gap
- Score: 1.50 (value 3 / effort M)
- What: After a text search, render interactive chips (date range, camera model, GPS region) that refine the current results without a new query.
- Why: Faceted refinement is standard in media libraries and exposes the metadata that's otherwise invisible on the main page.
- Blocked by: —
- Notes: Depends on W33 (backend metadata filters) being in place.
- [ ] execute   [ ] skip

### W44. Show similarity scores beside TUI thumbnails (TUI — src/tui/widget/image.rs)
- Lens: ux
- Score: 1.50 (value 3 / effort M)
- What: Render each result's similarity score as a small label/bar near its thumbnail in the 3×3 grid.
- Why: Users can't judge result quality at a glance in the TUI; the CLI shows scores, so hiding them in the TUI is inconsistent.
- Blocked by: —
- [ ] execute   [ ] skip

### W45. Implement (or remove) the GraphQL `Query::search` stub (graphql — src/graphql.rs:29)
- Lens: unblock-debt
- Score: 1.00 (value 1 / effort S)
- What: `Query::search` returns hardcoded `"Search result 1/2/3"`; wire it to the real search engine returning image results, or remove it if REST is canonical.
- Why: A GraphQL search client is impossible while the resolver is a stub; only `imagesByBounds` and REST search work.
- Blocked by: —
- Notes: Overlaps bughunt D4 (which frames it as dead-code cleanup); here the forward value is "enable a GraphQL search client".
- [ ] execute   [ ] skip

### W46. Async/lazy CLIP model initialization for fast server startup (embeddings — src/main.rs:203)
- Lens: unblock-debt
- Score: 1.00 (value 2 / effort M)
- What: `ClipEmbedder::new()` blocks `serve()` until the model loads; lazy/async-init it in a background task cached in `Arc<OnceCell<_>>`.
- Why: The server hangs (and health checks time out) until a 1GB+ model loads — a problem for containerized deploys and fast restart/failover.
- Blocked by: —
- [ ] execute   [ ] skip

### W47. Cache embeddings + results for hot repeat text queries (search — src/search.rs)
- Lens: scale-perf
- Score: 1.00 (value 2 / effort M)
- What: Cache the text embedding and results for the top-N repeated queries; serve from cache when a query matches within epsilon, invalidating on re-index.
- Why: Repeat queries re-run CLIP inference and vector search; caching the hottest queries removes redundant GPU work on a busy frontend.
- Blocked by: —
- Notes: Diminishing returns unless the deployment sees many repeated queries.
- [ ] execute   [ ] skip

### W48. EXIF metadata editing command (CLI/API — src/metadata.rs)
- Lens: feature-gap
- Score: 1.00 (value 2 / effort M)
- What: Add `imgfind metadata --edit --path <img> [--datetime …] [--camera-model …] [--lat …] [--lon …]` to overwrite EXIF in place.
- Why: Photos from older devices often have missing/wrong metadata; users could correct it without external tools since imgfind already reads and stores EXIF.
- Blocked by: —
- Notes: Needs an EXIF-writing library and careful validation; riskier than the read path.
- [ ] execute   [ ] skip

### W49. Cluster drill-down on the map view (React SPA/GraphQL — site/src/page/MapView.tsx)
- Lens: feature-gap
- Score: 1.00 (value 2 / effort M)
- What: Show a count badge on clustered markers; clicking a cluster zooms and re-fetches to reveal member images (optionally listing them in a sidebar).
- Why: The backend already clusters dense results, but the UI gives no feedback or way in — users can't tell a 1-image marker from a 100-image one.
- Blocked by: —
- [ ] execute   [ ] skip

### W50. Enforce the relative-path invariant with a wrapper type (database — src/database.rs:146)
- Lens: unblock-debt
- Score: 1.00 (value 2 / effort M)
- What: Replace manual `abs_to_relative_path`/`relative_to_abs_path` calls at boundaries with a `RelativePath`/`AbsolutePath` newtype so mixing is a compile error.
- Why: As path-handling surface grows (tagging, filtering, collections), the risk of storing absolute paths or passing the wrong kind grows; a type makes the invariant unmissable.
- Blocked by: —
- Notes: More of a type-safety hardening; consider running it through the `typecheck` sibling instead if that file is created.
- [ ] execute   [ ] skip

### W51. Metadata-aware result re-ranking (Search Engine — src/search.rs)
- Lens: feature-gap
- Score: 1.00 (value 2 / effort M)
- What: Add optional boosts to search ranking — prefer recent images, GPS-tagged images, a specific camera, or higher resolution — applied as a post-query re-rank.
- Why: Pure semantic similarity doesn't capture intent like "the most recent beach photos"; metadata signals improve relevance for many queries.
- Blocked by: —
- Notes: Strongest once W33 (metadata filters) exposes these fields to the query layer.
- [ ] execute   [ ] skip

### W52. Model versioning / multi-model embedding support (embeddings — src/database.rs:75)
- Lens: unblock-debt
- Score: 1.00 (value 4 / effort L)
- What: Add a `model_id` column to `images`/`image_vectors`, make the embedding dimension configurable per model, and let `ClipEmbedder` init select a model (the table is hardcoded to `float[512]` today).
- Why: Switching CLIP models (ViT-L vs ViT-H, or other vendors) currently requires a schema migration and full re-index; versioning unblocks fine-tuned/domain models and side-by-side quality comparison with gradual re-indexing.
- Blocked by: —
- Notes: Best done after W43 (migrations) so the schema change is safe.
- [ ] execute   [ ] skip

### W53. Parallelize metadata extraction off the indexing critical path (indexing — src/main.rs:401)
- Lens: scale-perf
- Score: 1.00 (value 4 / effort L)
- What: Move inline `extract_image_metadata` into a parallel JoinSet/rayon pass — extract all images concurrently and merge after embedding inserts — instead of blocking per image.
- Why: Indexing stalls on per-image EXIF read + decode (I/O-bound); parallel extraction unblocks 5-10× throughput at 100+ images.
- Blocked by: —
- Notes: Overlaps W29 (batch metadata); could be designed together. Needs careful error handling + transaction management.
- [ ] execute   [ ] skip

## Medium

### W54. Image collections / albums (Database/API/SPA — src/database.rs)
- Lens: feature-gap
- Score: 0.75 (value 3 / effort L)
- What: Add `albums` and `album_images` (many-to-many) tables, CRUD endpoints, and UI to create named albums, add search results to them, and view/export an album.
- Why: Complements semantic search with explicit curation — users want to assemble thematic collections from results.
- Blocked by: —
- Notes: Depends on W32 (mutation root) + W36 (user-data schema); pairs naturally with W55.
- [ ] execute   [ ] skip

### W55. User-defined tagging system (Database/API/SPA — src/database.rs)
- Lens: feature-gap
- Score: 0.75 (value 3 / effort L)
- What: Add an `image_tags` table plus APIs and web UI to assign user-defined tags and search/filter by them.
- Why: Lets users organize results into custom categories (favorites, to-print, needs-editing), layering explicit curation on top of semantic search.
- Blocked by: —
- Notes: Depends on W32 (mutation root) + W36 (user-data schema); shares foundations with W54.
- [ ] execute   [ ] skip

## Low

_(none)_

## Skip (do not re-flag in future runs)
