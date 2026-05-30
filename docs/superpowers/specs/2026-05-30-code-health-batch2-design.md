# Code-Health Batch 2 — Design Spec

Date: 2026-05-30
Branch: `code-health/2026-05-30`
Source audit: `bughunt.md` (strip each fixed finding in the same commit)

## Scope

Seven actionable findings plus two bookkeeping edits. **In scope:** P4, D1, D2, D3, D5, O1, O3. **Bookkeeping:** strip O2 (won't-fix) and leave P3 (deferred) in `bughunt.md`. No rewrites; each finding is an independent small commit. Only O3 changes behavior (adds a failure summary); the rest are pure cleanup or no-ops.

---

## Findings & approach

### P4 — single normalization boundary
`SearchEngine::{search, search_with_thumbnails, search_with_thumbnails_raw}` already call `normalize_vector` internally (`src/search.rs:15,24,35`). Three callers redundantly normalize first:
- `src/main.rs::search_images` — `let normalized_query = normalize_vector(&query_embedding);` then `search_engine.search(&normalized_query, ...)`.
- `src/api/search.rs::search` — normalizes then `search_with_thumbnails(&normalized_query, 80)`.
- `src/tui/app/search.rs::handle_search` — normalizes then `search_with_thumbnails_raw(&normalized_query, 99, 0)`.

**Fix:** drop the caller-side `normalize_vector` call in all three; pass the raw `query_embedding` straight into the `SearchEngine` method. `SearchEngine` remains the sole normalization boundary. Remove the now-unused `normalize_vector` import in `api/search.rs` and `tui/app/search.rs`. **Keep** the import in `main.rs` (indexing still calls `normalize_vector` on image embeddings). Functionally identical — normalization is idempotent (`test_double_normalize_vector` in `search.rs` proves it).

### D1 — delete dead+buggy `err_wrapper`
`src/api/mod.rs:32` — zero callers; also `.unwrap()`s on the mapped `Result` (would panic on `Err`). Delete the function. Then remove `use axum::Json;` (line 1) and `use serde::Serialize;` (line 4) **if** they have no other use in the file (clippy will confirm; `AppError`/`middleware`/`get_test` don't use them).

### D2 — delete dead `graphql_translate`
`src/graphql.rs:96` — zero callers. Delete the function. Then remove `FieldError` from the `juniper::{...}` import (line 2) and `error` from `use tracing::{error, info};` (line 3) — both are used only by this function (`info` stays, used in `images_by_bounds`).

### D3 — delete unused `Mutation` struct
`src/graphql.rs:84` — `pub struct Mutation;` is unused; the schema uses `EmptyMutation`. Delete the line.

### D5 — remove scaffolding routes
- `src/routes.rs` — delete `async fn root() -> &'static str { "Hello world!" }` (line 88) and the `.route("/test", get(root))` registration (line 72, under `graphql_routes`).
- `src/api/mod.rs` — delete `pub async fn get_test() -> &'static str { "hello world" }` and the `.route("/test", get(get_test))` registration (line 24).
- Remove any import made unused by these deletions (clippy will flag).

### O1 — `eprintln!` → `tracing::error!` in the TUI
`eprintln!` corrupts the alternate-screen TUI. Replace at:
- `src/tui/app.rs:203` (HandleSearch error), `:215` and `:221` (NextPage / PreviousPage update errors).
- `src/tui/app/search.rs:207` (search-task error).

For `app.rs`, add `error` to its tracing import (currently `use tracing::info;` → `use tracing::{error, info};`) and use `error!(...)`. For `search.rs`, `error` is already imported (`use tracing::{debug, error};`) — just swap `eprintln!` → `error!`.

### O3 — surface metadata-backfill failures
`src/metadata.rs::extract_missing_metadata` currently logs extract failures at `debug!` and silently skips. **Fix:** add a `failed` counter; increment it in both the store-failure arm (line ~49) and the extract-failure arm (line ~59); keep the per-item `debug!`/`warn!` detail. After the loop, when `failed > 0`, emit `warn!("{failed} images failed metadata extraction or storage");` and, when `!quiet`, a user line consistent with the existing emoji summary style (e.g. `println!("  ⚠️  Failed: {failed}");`).

---

## Bookkeeping (no code)
- **O2** — remove its entry from `bughunt.md`: the metadata-backfill `println!`s are intentional user-facing CLI progress (gated by `!quiet`, consistent with `index_directory`); converting to tracing would remove visible output. Won't-fix.
- **P3** — leave its entry in `bughunt.md` (deferred; per-action cost, and `ImageEntry` is not cloneable so it needs an `Arc<DynamicImage>` refactor, out of scope for this batch).

## Verification
`cargo build`, `cargo clippy --all-targets` (expect zero warnings — the crate is currently warning-clean, so any new unused-import warning must be cleaned up within its task), `cargo test` after each change. The existing `normalize_vector` and `double_normalize` tests cover P4's safety.

## Commit convention
One commit per finding: `<type>(<area>): <summary> [<id>]` (`fix`/`refactor`/`chore` as fits), stripping that finding from `bughunt.md` in the same commit. O2 is stripped in whichever commit is most natural (its own small `docs:` commit). P3 is untouched.
