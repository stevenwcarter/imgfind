# imgfind — Code Health Audit (bughunt.md)

> **For future sessions reading this file:** when you fix an item listed here, strip it from this file in the same commit that fixes it. The list is intended to reflect open issues only; resolved items shouldn't linger. This keeps the file's signal-to-noise high for the next audit pass.

Audit date: 2026-05-29. Scope: full repo (Rust CLI/TUI/Axum server + React SPA). Findings verified by reading; severity is my assessment, open to discussion.

---

## SECURITY

### S1 — Path traversal in file-serving endpoint `[HIGH]`
`src/api/search.rs:68` — the `/api/v1/search/file/{*filename}` handler does:
```rust
let full_path = std::path::Path::new(&context.basepath).join(&filename);
std::fs::read(&full_path)
```
`filename` is the unsanitized wildcard segment. A request like `/api/v1/search/file/../../../../etc/passwd` escapes `basepath` and reads arbitrary files. Combined with S2 (binds `0.0.0.0`) this is network-reachable with no auth. **Fix:** canonicalize the joined path and verify it still starts with the canonicalized `basepath`; reject otherwise.

### S2 — Server binds `0.0.0.0` with no auth `[MED]`
`src/main.rs:200` — `TcpListener::bind(format!("0.0.0.0:{port}"))`. Exposes the SPA, GraphQL, and the (vulnerable) file endpoint on all interfaces. Consider binding `127.0.0.1` by default with an opt-in flag for `0.0.0.0`.

---

## PERFORMANCE / ARCHITECTURE

### P1 — CLIP model reloaded on every web search request `[HIGH]`
`src/api/search.rs:47` — `ClipEmbedder::new(None, None, false)` runs inside the `/search/{query}` handler, reloading the model from disk on every request. **Fix:** load once at startup, store in `GraphQLContext` (e.g. `Arc<ClipEmbedder>`), reuse. (CLI `main.rs:246,477` and the TUI search task `tui/app/search.rs:151` load per-invocation too, which is acceptable for one-shot/spawned use.)

### P2 — `Arc<Mutex<Database>>` serializes all DB access in the server `[MED]`
`src/context.rs:6` wraps `Database` in a `Mutex`, but `Database` already holds an r2d2 connection pool and is `Clone` + `Send`/`Sync`. The mutex serializes every request through one lock, defeating the pool's concurrency. **Fix:** drop the `Mutex` and clone the pooled `Database` per handler (or share `Arc<Database>` without the lock). Also removes the `.lock().unwrap()` poison-panic risk in `graphql.rs:46` and `api/search.rs:30,55`.

### P3 — TUI clones decoded images per page update `[MED]`
`src/tui/app/search.rs:104,110` — `image.clone()` on heavy `ImageEntry` values during paging. Consider `Rc`/`Arc` or index-based access.

### P4 — Query vector normalized twice (CLI) `[LOW]`
`src/main.rs:485` normalizes, then `SearchEngine::search` (`src/search.rs:15`) normalizes again. Harmless but redundant; normalize at one layer.

---

## CORRECTNESS

### C1 — `get_db_path(Some(dir))` panics instead of returning Err `[MED]`
`src/lib.rs:27` — `panic!("No database found in this directory")`. Reachable from `serve`/`tui`/`metadata` with `--dir`. **Fix:** return `Err(anyhow!(...))` so the CLI prints a clean message.

### C3 — `serve()` panics on bind/serve + stale comment `[LOW]`
`src/main.rs:200-209` — `.unwrap()`/`.expect()` instead of `?`; line 197 comment says "Placeholder for future server implementation" though it's fully implemented. Propagate errors and delete the comment.

### C4 — TUI focus underflow when no images `[MED]`
`src/tui/app/focus.rs:32-33` — `images_len - 1` on unsigned/`u8` when `images_len == 0` wraps/panics. Guard the empty case. *(Confirm exact lines before fixing.)*

### C5 — TUI zoom path full of `.expect()` `[MED]`
`src/tui/app/zoom.rs:34,73-75,94` — chained `expect()` on image lookup, file open, decode, and channel send. A missing/unreadable image panics the whole TUI. **Fix:** handle errors and surface a message instead of crashing.

### C6 — Frontend `imagesByBounds` binds east/west swapped `[LOW — smell]`
`site/src/page/MapView.tsx:17` — `east: $west, west: $east`. **Behaviorally harmless today** because the resolver normalizes with `min()/max()` (`src/database.rs:596-597`), but it's misleading and becomes a real bug if the resolver stops normalizing. Fix the binding to match the names.

### C7 — Frontend double-fetch on mount `[LOW]`
`site/src/page/MapView.tsx` (~93-167) — direct `fetchGeotaggedImages()` call plus an effect that also fires it. Causes a redundant initial query.

---

## DEAD CODE / QUALITY

### D1 — `err_wrapper` is dead AND buggy `[LOW]`
`src/api/mod.rs:32` — zero callers; also `result.map_err(...).unwrap()` would panic on `Err` rather than returning an error response. Delete.

### D2 — `graphql_translate` is dead `[LOW]`
`src/graphql.rs:97` — zero callers. Delete.

### D3 — `Mutation` struct unused `[LOW]`
`src/graphql.rs:85` — schema uses `EmptyMutation`. Delete.

### D4 — `Query::search` GraphQL resolver is a stub `[LOW]`
`src/graphql.rs:29` — returns hardcoded `"Search result 1/2/3"`. Either implement or remove.

### D5 — Leftover scaffolding routes `[LOW]`
`src/routes.rs:88` (`root` → "Hello world!", route `/graphql/test`) and `src/api/mod.rs:28` (`get_test` → "hello world", route `/api/v1/test`). Remove.

### D6 — Commented-out dead code `[LOW]`
`src/database.rs:386` (`get_connection`), `database.rs:427-435` (path conversion in `get_image_hash`), `database.rs:239-240` (rel→abs in `search_similar_images`), `src/routes.rs:62` (subscriptions route), `tui/app/search.rs:171-173` (`.skip/.take` + no-op `.filter(|_| true)`), `tui/app/zoom.rs:78` (resize), `site/src/page/MapView.tsx:238-264` (Popup block). Remove.

### D7 — Unused vars/params `[LOW]`
`src/tui/app.rs:143,149` — `mouse_event` unused in `ZoomIn`/`ZoomOut`. `src/tui/ui.rs:63` — `_x` cursor calc computed but never applied (cursor position not set).

### D8 — Frontend lint debt `[LOW]`
`site/src/page/Images.tsx:47` — `console.log('Images fetched', data)`. `Images.tsx:54` — `e: any` (use `React.MouseEvent`). `Images.tsx:23-24` — `zoomRef`/`thumbnailsRef` possibly unused.

---

## OBSERVABILITY

### O1 — `eprintln!` in TUI error paths `[MED]`
`src/tui/app.rs:172,184,190` and `tui/app/search.rs:207` — `eprintln!` is invisible/garbled in alternate-screen mode. Use `tracing::error!`.

### O2 — `println!` mixed with tracing in metadata backfill `[LOW]`
`src/metadata.rs:12,72` — inconsistent with the rest of the module's `info!`. (Acceptable as user-facing CLI progress, but worth a consistent choice.)

### O3 — Metadata backfill swallows failures `[LOW]`
`src/metadata.rs:49` (insert failure → `warn!` then data discarded) and `:59` (extraction failure logged at `debug!`). Silent under quiet mode; consider an aggregated error count.

---

## RESOURCE / MISC

### R1 — Fire-and-forget `tokio::spawn` handles dropped `[LOW]`
`src/tui/event.rs:67`, `tui/app/zoom.rs:65` — JoinHandles dropped; no await/abort on shutdown. Low real risk since the process exits with the TUI.

### R2 — Logging guard leak + ANSI in file logs `[LOW]`
`src/logging.rs:22` — `mem::forget(_guard)` keeps the non-blocking appender alive for the program lifetime (intentional, but undocumented; add a comment). `logging.rs:19` — `with_ansi(true)` writes ANSI escape codes into file logs; gate on TTY detection.
