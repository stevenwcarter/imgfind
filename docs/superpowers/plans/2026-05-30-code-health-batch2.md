# Code-Health Batch 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve seven bughunt findings (P4, D1, D2, D3, D5, O1, O3) — dead-code deletion, a redundant-normalization fix, logging cleanup, and a metadata-failure summary — plus strip the won't-fix O2.

**Architecture:** Independent small commits, one finding each, build green + clippy-clean after every commit. Most are pure cleanup/no-ops; only O3 changes behavior. No new unit tests — these are deletions, idempotent refactors (P4 is covered by the existing `normalize_vector` tests), and logging swaps; correctness is verified by build/clippy/test staying green.

**Tech Stack:** Rust (edition 2024), Axum, Juniper, ratatui, tracing. Verify: `cargo build`, `cargo clippy --all-targets`, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-05-30-code-health-batch2-design.md`. Each task strips its finding from `bughunt.md` in the same commit.

**Ordering note:** D1 before D5 — both edit `src/api/mod.rs` imports. D1 removes `Json`/`Serialize`; D5 then removes the `/test` route, which is what makes `routing::get` unused in that file, so D5 owns the `get` import removal. D2 and D3 both edit `src/graphql.rs` but are separate commits.

**The crate is currently warning-clean (0 clippy warnings). Every task must leave it that way** — remove imports a deletion makes unused.

---

## Task 1: P4 — single normalization boundary

`SearchEngine`'s methods already call `normalize_vector` internally; three callers redundantly normalize first. Remove the caller-side normalization in all three; pass the raw embedding. One commit, three files.

**Files:** `src/main.rs`, `src/api/search.rs`, `src/tui/app/search.rs`

- [ ] **Step 1: `src/main.rs::search_images`** — find:
```rust
    let normalized_query = normalize_vector(&query_embedding);

    // Search database
    info!("Searching database...");
    let search_engine = SearchEngine::new(db);
    let all_results = search_engine.search(&normalized_query, usize::MAX)?; // Get all results first
```
Replace with:
```rust
    // Search database (SearchEngine normalizes the query internally)
    info!("Searching database...");
    let search_engine = SearchEngine::new(db);
    let all_results = search_engine.search(&query_embedding, usize::MAX)?; // Get all results first
```
Leave `use imgfind::search::{SearchEngine, normalize_vector};` unchanged — `normalize_vector` is still used in `index_directory`.

- [ ] **Step 2: `src/api/search.rs::search`** — find:
```rust
    let query_embedding = context
        .embedder
        .get_text_embedding(search.as_str())
        .context("Failed to generate text embedding")?;
    let normalized_query = normalize_vector(&query_embedding);

    let search = SearchEngine::new(&context.db);
    let result = search
        .search_with_thumbnails(&normalized_query, 80)
        .context("Failed to perform search")?;
```
Replace with:
```rust
    let query_embedding = context
        .embedder
        .get_text_embedding(search.as_str())
        .context("Failed to generate text embedding")?;

    let search = SearchEngine::new(&context.db);
    let result = search
        .search_with_thumbnails(&query_embedding, 80)
        .context("Failed to perform search")?;
```
Then change the import `use crate::{ ... search::{SearchEngine, normalize_vector}, ... };` to drop `normalize_vector` (it has no other use in this file): `search::SearchEngine,`.

- [ ] **Step 3: `src/tui/app/search.rs::handle_search`** — find:
```rust
                let query_embedding = model
                    .get_text_embedding(&query)
                    .context("Failed to generate text embedding")?;

                let normalized_query = normalize_vector(&query_embedding);

                // Search database
                let search_engine = SearchEngine::new(&db);
                let all_results =
                    search_engine.search_with_thumbnails_raw(&normalized_query, 99, 0)?;
```
Replace with:
```rust
                let query_embedding = model
                    .get_text_embedding(&query)
                    .context("Failed to generate text embedding")?;

                // Search database (SearchEngine normalizes the query internally)
                let search_engine = SearchEngine::new(&db);
                let all_results =
                    search_engine.search_with_thumbnails_raw(&query_embedding, 99, 0)?;
```
Then change the import `use crate::{ search::{SearchEngine, normalize_vector}, tui::app::App, };` to drop `normalize_vector`: `use crate::{search::SearchEngine, tui::app::App};`.

- [ ] **Step 4: Verify** — `cargo build && cargo clippy --all-targets && cargo test`. Expect: compiles, zero warnings, all tests pass (including `test_normalize_vector` / `test_double_normalize_vector`).

- [ ] **Step 5: Strip + commit** — remove the `### P4 — Query vector normalized twice (CLI)` heading and body from `bughunt.md`. Then:
```bash
git add src/main.rs src/api/search.rs src/tui/app/search.rs bughunt.md
git commit -m "refactor(search): normalize query once in SearchEngine [P4]"
```

---

## Task 2: D1 — delete dead+buggy `err_wrapper`

**Files:** `src/api/mod.rs`

- [ ] **Step 1: Delete the function** — remove:
```rust
pub fn err_wrapper<T: Serialize>(result: anyhow::Result<T>) -> impl IntoResponse {
    Json(
        result
            .map_err(|err| (StatusCode::NOT_FOUND, err.to_string()))
            .unwrap(),
    )
}
```

- [ ] **Step 2: Remove imports it solely used** — delete line `use axum::Json;` and line `use serde::Serialize;`. Keep `use axum::{Router, http::StatusCode, routing::get};` (StatusCode is still used by `AppError`; `get` is still used by the `/test` route until Task 3) and `use axum::response::{IntoResponse, Response};` (used by `AppError`).

- [ ] **Step 3: Verify** — `cargo build && cargo clippy --all-targets`. Expect compiles, zero warnings (no "unused import" for Json/Serialize).

- [ ] **Step 4: Strip + commit** — remove the `### D1 — err_wrapper is dead AND buggy` heading and body from `bughunt.md`. Then:
```bash
git add src/api/mod.rs bughunt.md
git commit -m "chore(api): remove dead err_wrapper helper [D1]"
```

---

## Task 3: D5 — remove scaffolding `/test` routes

**Files:** `src/api/mod.rs`, `src/routes.rs`

- [ ] **Step 1: `src/api/mod.rs`** — remove the `/test` route from `api_routes`:
```rust
pub fn api_routes(context: GraphQLContext) -> Router {
    Router::new()
        .route("/test", get(get_test))
        .nest("/search", search_routes(context.clone()))
}
```
becomes:
```rust
pub fn api_routes(context: GraphQLContext) -> Router {
    Router::new().nest("/search", search_routes(context.clone()))
}
```
And delete the handler:
```rust
pub async fn get_test() -> &'static str {
    "hello world"
}
```

- [ ] **Step 2: `src/api/mod.rs` import fallout** — `routing::get` is now unused. Change `use axum::{Router, http::StatusCode, routing::get};` to `use axum::{Router, http::StatusCode};`.

- [ ] **Step 3: `src/routes.rs`** — remove the `/test` route from the `graphql_routes` builder. Find:
```rust
        .route("/test", get(root))
```
and delete that line. Then delete the handler:
```rust
async fn root() -> &'static str {
    "Hello world!"
}
```
Leave the `get` import in `routes.rs` alone — it's still used by `index_handler`/`static_handler` routes.

- [ ] **Step 4: Verify** — `cargo build && cargo clippy --all-targets`. Expect compiles, zero warnings.

- [ ] **Step 5: Strip + commit** — remove the `### D5 — Leftover scaffolding routes` heading and body from `bughunt.md`. Then:
```bash
git add src/api/mod.rs src/routes.rs bughunt.md
git commit -m "chore(api): remove scaffolding /test routes [D5]"
```

---

## Task 4: D2 — delete dead `graphql_translate`

**Files:** `src/graphql.rs`

- [ ] **Step 1: Delete the function** — remove:
```rust
pub fn graphql_translate<T>(res: Result<T, anyhow::Error>) -> FieldResult<T> {
    match res {
        Ok(t) => Ok(t),
        Err(e) => {
            error!("graphql error: {:#?}", e);
            Err(FieldError::from(e))
        }
    }
}
```

- [ ] **Step 2: Remove imports it solely used** — in `use juniper::{EmptyMutation, EmptySubscription, FieldError, FieldResult, GraphQLObject, RootNode};` drop `FieldError` (→ `use juniper::{EmptyMutation, EmptySubscription, FieldResult, GraphQLObject, RootNode};`). In `use tracing::{error, info};` drop `error` (→ `use tracing::info;`). Keep `FieldResult` (used by `Query` resolvers) and `info` (used in `images_by_bounds`).

- [ ] **Step 3: Verify** — `cargo build && cargo clippy --all-targets`. Expect compiles, zero warnings.

- [ ] **Step 4: Strip + commit** — remove the `### D2 — graphql_translate is dead` heading and body from `bughunt.md`. Then:
```bash
git add src/graphql.rs bughunt.md
git commit -m "chore(graphql): remove dead graphql_translate helper [D2]"
```

---

## Task 5: D3 — delete unused `Mutation` struct

**Files:** `src/graphql.rs`

- [ ] **Step 1: Delete the line** — remove:
```rust
pub struct Mutation;
```
(The schema uses `EmptyMutation`; nothing references `Mutation`.)

- [ ] **Step 2: Verify** — `cargo build && cargo clippy --all-targets`. Expect compiles, zero warnings.

- [ ] **Step 3: Strip + commit** — remove the `### D3 — Mutation struct unused` heading and body from `bughunt.md`. Then:
```bash
git add src/graphql.rs bughunt.md
git commit -m "chore(graphql): remove unused Mutation struct [D3]"
```

---

## Task 6: O1 — `eprintln!` → `tracing::error!` in the TUI

**Files:** `src/tui/app.rs`, `src/tui/app/search.rs`

- [ ] **Step 1: `src/tui/app.rs` import** — change `use tracing::info;` to `use tracing::{error, info};`.

- [ ] **Step 2: `src/tui/app.rs` — replace the three `eprintln!`** (in the `HandleSearch`, `NextPage`, `PreviousPage` arms):
  - In `HandleSearch`:
    ```rust
                    if let Err(err) = self.handle_search(&query) {
                        // Handle the error, e.g., log it or display a message to the user.
                        // For now, we'll just print it to the console.
                        eprintln!("Error handling search: {:?}", err);
                    }
    ```
    becomes:
    ```rust
                    if let Err(err) = self.handle_search(&query) {
                        error!("Error handling search: {:?}", err);
                    }
    ```
  - In `NextPage`:
    ```rust
                    if let Err(err) = self.update_page() {
                        eprintln!("Error updating page: {:?}", err);
                    }
    ```
    becomes:
    ```rust
                    if let Err(err) = self.update_page() {
                        error!("Error updating page: {:?}", err);
                    }
    ```
  - In `PreviousPage` (same body as NextPage):
    ```rust
                    if let Err(err) = self.update_page() {
                        eprintln!("Error updating page: {:?}", err);
                    }
    ```
    becomes:
    ```rust
                    if let Err(err) = self.update_page() {
                        error!("Error updating page: {:?}", err);
                    }
    ```

- [ ] **Step 3: `src/tui/app/search.rs:207`** — `error` is already imported (`use tracing::{debug, error};`). Replace:
```rust
                    eprintln!("Search error: {:?}", err);
```
with:
```rust
                    error!("Search error: {:?}", err);
```

- [ ] **Step 4: Verify** — `cargo build && cargo clippy --all-targets && cargo test`. Expect compiles, zero warnings, tests pass. Confirm no `eprintln!` remains in the TUI: `grep -rn "eprintln!" src/tui/` returns nothing.

- [ ] **Step 5: Strip + commit** — remove the `### O1 — eprintln! in TUI error paths` heading and body from `bughunt.md`. Then:
```bash
git add src/tui/app.rs src/tui/app/search.rs bughunt.md
git commit -m "fix(tui): log errors via tracing instead of eprintln [O1]"
```

---

## Task 7: O3 — surface metadata-backfill failures

**Files:** `src/metadata.rs`

- [ ] **Step 1: Add a failure counter and increment it.** In `extract_missing_metadata`, the current loop body is:
```rust
        let mut metadata_extracted = 0;
        for (image_id, image_path, _hash) in images_without_metadata {
            if !quiet {
                metadata_progress.set_message(format!(
                    "{}",
                    std::path::Path::new(&image_path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            }

            match extract_image_metadata(&image_path) {
                Ok(metadata) => {
                    if let Err(e) = db.insert_or_update_metadata(image_id, &metadata) {
                        warn!(
                            "Failed to store backfilled metadata for {}: {}",
                            image_path, e
                        );
                    } else {
                        metadata_extracted += 1;
                        debug!("Backfilled metadata for: {}", image_path);
                    }
                }
                Err(e) => {
                    debug!(
                        "Failed to extract backfill metadata for {}: {}",
                        image_path, e
                    );
                }
            }
            metadata_progress.inc(1);
        }
```
Replace it with (adds `let mut failed = 0;` and increments in both failure arms):
```rust
        let mut metadata_extracted = 0;
        let mut failed = 0;
        for (image_id, image_path, _hash) in images_without_metadata {
            if !quiet {
                metadata_progress.set_message(format!(
                    "{}",
                    std::path::Path::new(&image_path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            }

            match extract_image_metadata(&image_path) {
                Ok(metadata) => {
                    if let Err(e) = db.insert_or_update_metadata(image_id, &metadata) {
                        warn!(
                            "Failed to store backfilled metadata for {}: {}",
                            image_path, e
                        );
                        failed += 1;
                    } else {
                        metadata_extracted += 1;
                        debug!("Backfilled metadata for: {}", image_path);
                    }
                }
                Err(e) => {
                    debug!(
                        "Failed to extract backfill metadata for {}: {}",
                        image_path, e
                    );
                    failed += 1;
                }
            }
            metadata_progress.inc(1);
        }
```

- [ ] **Step 2: Emit the summary after the loop.** The current post-loop block is:
```rust
        metadata_progress.finish_with_message("Metadata extraction complete!");

        if !quiet {
            println!("  📊 Metadata extracted: {}", metadata_extracted);
        }
        info!(
            "Metadata backfill complete: {} extracted",
            metadata_extracted
        );
```
Replace with:
```rust
        metadata_progress.finish_with_message("Metadata extraction complete!");

        if !quiet {
            println!("  📊 Metadata extracted: {}", metadata_extracted);
            if failed > 0 {
                println!("  ⚠️  Failed: {}", failed);
            }
        }
        if failed > 0 {
            warn!("{} images failed metadata extraction or storage", failed);
        }
        info!(
            "Metadata backfill complete: {} extracted, {} failed",
            metadata_extracted, failed
        );
```
(`warn` is already imported via `use tracing::{debug, info, warn};`.)

- [ ] **Step 3: Verify** — `cargo build && cargo clippy --all-targets && cargo test`. Expect compiles, zero warnings, tests pass.

- [ ] **Step 4: Strip + commit** — remove the `### O3 — Metadata backfill swallows failures` heading and body from `bughunt.md`. Then:
```bash
git add src/metadata.rs bughunt.md
git commit -m "fix(metadata): count and report backfill failures [O3]"
```

---

## Task 8: O2 — strip won't-fix from bughunt.md

**Files:** `bughunt.md`

- [ ] **Step 1: Remove the O2 entry.** Delete the `### O2 — println! mixed with tracing in metadata backfill` heading and body. The metadata-backfill `println!`s are intentional user-facing CLI progress (gated by `!quiet`, consistent with `index_directory`); converting them to tracing would remove visible output. Won't-fix — no code change.

- [ ] **Step 2: Commit:**
```bash
git add bughunt.md
git commit -m "docs: drop O2 as won't-fix (intentional user-facing output) [O2]"
```

---

## Final verification

- [ ] `cargo build && cargo clippy --all-targets && cargo test` — green, **0 warnings**.
- [ ] `grep -rn "eprintln!" src/tui/` — no matches.
- [ ] `grep -rn "err_wrapper\|graphql_translate\|fn get_test\|fn root\|struct Mutation" src/` — no matches.
- [ ] `bughunt.md` no longer contains P4, D1, D2, D3, D5, O1, O2, O3; it still contains P3 (deferred), C6, C7, D4, D6, D7, D8, R1, R2.
- [ ] No summary commit — per-finding commits are the audit trail.
