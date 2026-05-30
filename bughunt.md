# imgfind — Code Health Audit (bughunt.md)

> **For future sessions reading this file:** when you fix an item listed here, strip it from this file in the same commit that fixes it. The list is intended to reflect open issues only; resolved items shouldn't linger. This keeps the file's signal-to-noise high for the next audit pass.

Audit date: 2026-05-29. Scope: full repo (Rust CLI/TUI/Axum server + React SPA). Findings verified by reading; severity is my assessment, open to discussion.

---

## PERFORMANCE / ARCHITECTURE

### P3 — TUI clones decoded images per page update `[MED]`
`src/tui/app/search.rs:104,110` — `image.clone()` on heavy `ImageEntry` values during paging. Consider `Rc`/`Arc` or index-based access.

---

## CORRECTNESS

### C7 — Frontend double-fetch on mount `[LOW]`
`site/src/page/MapView.tsx` (~93-167) — direct `fetchGeotaggedImages()` call plus an effect that also fires it. Causes a redundant initial query.

---

## DEAD CODE / QUALITY

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
`src/tui/ui.rs:63` — `_x` cursor calc computed but never applied (cursor position not set). (The `ZoomIn`/`ZoomOut` `mouse_event` params are now used — cursor-relative zoom, 2026-05-29.)

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
